use base64::Engine;
use ctx_core::ids::{MessageId, SessionId, TurnId};
use ctx_core::models::{MessageAttachment, MessageDelivery};
use ctx_route_contracts::sessions::{
    DeleteSessionMessageRouteParams, PostSessionMessageRouteRequest,
    PostSessionMessageRouteResponse, SessionMessageRouteError, SessionRouteParams,
};
use ctx_session_message_service::message_delivery::{
    resolve_message_client_ids, MessageClientIdResolutionError, MessageClientIds,
};

use crate::daemon::sessions::command_dispatch::SessionSchedulerCommandError;
use crate::daemon::sessions::route_contract::parse_session_route_id;
use crate::daemon::sessions::SessionImageBlobStoreError;
use crate::daemon::SessionMessageCommandHandle;

const QUEUED_MESSAGES_ENABLED_ENV: &str = "CTX_QUEUED_MESSAGES_ENABLED";
const MAX_MESSAGE_IMAGE_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
const MAX_MESSAGE_IMAGE_ATTACHMENT_MIB: usize = MAX_MESSAGE_IMAGE_ATTACHMENT_BYTES / (1024 * 1024);

impl SessionMessageCommandHandle {
    pub async fn post_session_message_for_route(
        &self,
        params: SessionRouteParams,
        request: PostSessionMessageRouteRequest,
        run_id_header: Option<String>,
    ) -> Result<PostSessionMessageRouteResponse, SessionMessageRouteError> {
        let session_id = parse_post_session_id(params)?;
        let (message_id, turn_id, content, delivery, attachments) = request.into_parts();
        let message_id = parse_optional_message_id(message_id.as_deref())?;
        let turn_id = parse_optional_turn_id(turn_id.as_deref())?;
        let client_ids =
            resolve_message_client_ids(message_id, turn_id).map_err(client_id_resolution_error)?;
        let attachments = self
            .normalize_message_attachments_for_route(attachments)
            .await?;

        let input = post_user_message_input_for_route(
            client_ids,
            content,
            delivery,
            attachments,
            queued_messages_enabled_from_env(),
            run_id_header,
        );

        self.post_user_message_for_request(session_id, input)
            .await
            .map(PostSessionMessageRouteResponse::new)
            .map_err(post_user_message_route_error)
    }

    pub async fn delete_session_message_for_route(
        &self,
        params: DeleteSessionMessageRouteParams,
    ) -> Result<(), SessionMessageRouteError> {
        let session_id = parse_session_route_id(params.session_id())
            .map_err(|_| SessionMessageRouteError::bad_request("invalid session id"))?;
        let message_id = parse_message_id(params.message_id())?;
        self.delete_queued_session_message(session_id, message_id)
            .await
            .map_err(delete_message_route_error)
    }

    async fn normalize_message_attachments_for_route(
        &self,
        attachments: Vec<MessageAttachment>,
    ) -> Result<Vec<MessageAttachment>, SessionMessageRouteError> {
        let mut out = Vec::with_capacity(attachments.len());
        for attachment in attachments {
            match attachment {
                MessageAttachment::Image {
                    mime_type,
                    data_base64,
                    name,
                } => {
                    let bytes = decode_inline_image_attachment(&data_base64)?;
                    let blob_id = self
                        .store_inline_image_blob(&bytes, &mime_type, name.as_deref())
                        .await
                        .map_err(image_blob_store_error)?;
                    out.push(MessageAttachment::ImageRef {
                        blob_id,
                        mime_type,
                        name,
                    });
                }
                MessageAttachment::ImageRef { blob_id, name, .. } => {
                    let mime_type = self.load_image_blob_mime_type_for_route(&blob_id).await?;
                    out.push(MessageAttachment::ImageRef {
                        blob_id,
                        mime_type,
                        name,
                    });
                }
            }
        }
        Ok(out)
    }

    async fn load_image_blob_mime_type_for_route(
        &self,
        blob_id: &str,
    ) -> Result<String, SessionMessageRouteError> {
        let blob = self.get_blob(blob_id).await.map_err(|_| {
            SessionMessageRouteError::internal("Failed to inspect image attachment.")
        })?;
        let Some((_sha256, stored_mime_type, bytes, _stored_name, _created_at)) = blob else {
            return Err(SessionMessageRouteError::bad_request(
                "Image attachment blob was not found.",
            ));
        };
        ensure_image_attachment_mime_type(&stored_mime_type)?;
        let bytes = usize::try_from(bytes).map_err(|_| {
            SessionMessageRouteError::internal("Invalid image attachment metadata.")
        })?;
        ensure_image_attachment_size(bytes)?;
        Ok(stored_mime_type)
    }
}

fn parse_post_session_id(
    params: SessionRouteParams,
) -> Result<SessionId, SessionMessageRouteError> {
    parse_session_route_id(params.session_id())
        .map_err(|_| SessionMessageRouteError::bad_request("Invalid session id."))
}

fn parse_optional_message_id(
    raw: Option<&str>,
) -> Result<Option<MessageId>, SessionMessageRouteError> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(parse_message_id_with_post_message)
        .transpose()
}

fn parse_optional_turn_id(raw: Option<&str>) -> Result<Option<TurnId>, SessionMessageRouteError> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(parse_turn_id_with_post_message)
        .transpose()
}

fn parse_message_id(value: &str) -> Result<MessageId, SessionMessageRouteError> {
    uuid::Uuid::parse_str(value)
        .map(MessageId)
        .map_err(|_| SessionMessageRouteError::bad_request("invalid message id"))
}

fn parse_message_id_with_post_message(value: &str) -> Result<MessageId, SessionMessageRouteError> {
    uuid::Uuid::parse_str(value)
        .map(MessageId)
        .map_err(|_| SessionMessageRouteError::bad_request("Invalid message id."))
}

fn parse_turn_id_with_post_message(value: &str) -> Result<TurnId, SessionMessageRouteError> {
    uuid::Uuid::parse_str(value)
        .map(TurnId)
        .map_err(|_| SessionMessageRouteError::bad_request("Invalid turn id."))
}

fn client_id_resolution_error(error: MessageClientIdResolutionError) -> SessionMessageRouteError {
    match error {
        MessageClientIdResolutionError::PartialClientIds => {
            SessionMessageRouteError::bad_request(error.message())
        }
    }
}

fn post_user_message_input_for_route(
    client_ids: MessageClientIds,
    content: String,
    delivery: Option<MessageDelivery>,
    attachments: Vec<MessageAttachment>,
    queued_messages_enabled: bool,
    run_id_header: Option<String>,
) -> crate::daemon::sessions::PostUserMessageInput {
    crate::daemon::sessions::PostUserMessageInput {
        message_id: client_ids.message_id,
        turn_id: client_ids.turn_id,
        client_supplied_ids: client_ids.client_supplied,
        content,
        requested_delivery: delivery,
        attachments,
        queued_messages_enabled,
        run_id_header,
    }
}

fn image_blob_store_error(error: SessionImageBlobStoreError) -> SessionMessageRouteError {
    match error {
        SessionImageBlobStoreError::PayloadTooLarge => image_attachment_too_large_error(),
        SessionImageBlobStoreError::UnsupportedMediaType => {
            SessionMessageRouteError::unsupported_media_type(
                "Only image attachments are supported.",
            )
        }
        SessionImageBlobStoreError::Internal => {
            SessionMessageRouteError::internal("Failed to persist image attachment.")
        }
    }
}

fn post_user_message_route_error(
    error: crate::daemon::sessions::PostUserMessageError,
) -> SessionMessageRouteError {
    match error {
        crate::daemon::sessions::PostUserMessageError::BadRequest(error) => {
            SessionMessageRouteError::bad_request(error)
        }
        crate::daemon::sessions::PostUserMessageError::Conflict(error) => {
            SessionMessageRouteError::conflict(error)
        }
        crate::daemon::sessions::PostUserMessageError::NotFound(error) => {
            SessionMessageRouteError::not_found(error)
        }
        crate::daemon::sessions::PostUserMessageError::ServiceUnavailable(error) => {
            SessionMessageRouteError::service_unavailable(error)
        }
        crate::daemon::sessions::PostUserMessageError::Internal(error) => {
            SessionMessageRouteError::internal(error)
        }
    }
}

fn delete_message_route_error(error: SessionSchedulerCommandError) -> SessionMessageRouteError {
    match error {
        SessionSchedulerCommandError::BadRequest => {
            SessionMessageRouteError::bad_request("bad request")
        }
        SessionSchedulerCommandError::NotFound => {
            SessionMessageRouteError::not_found("message not found")
        }
        SessionSchedulerCommandError::StoreUnavailable => {
            SessionMessageRouteError::internal("session store unavailable")
        }
    }
}

fn queued_messages_enabled_from_env() -> bool {
    env_bool(std::env::var(QUEUED_MESSAGES_ENABLED_ENV).ok().as_deref()).unwrap_or(false)
}

fn env_bool(value: Option<&str>) -> Option<bool> {
    value.and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    })
}

fn image_attachment_too_large_error() -> SessionMessageRouteError {
    SessionMessageRouteError::payload_too_large(format!(
        "Image attachments must be {MAX_MESSAGE_IMAGE_ATTACHMENT_MIB} MiB or smaller."
    ))
}

fn ensure_image_attachment_size(bytes: usize) -> Result<(), SessionMessageRouteError> {
    if bytes > MAX_MESSAGE_IMAGE_ATTACHMENT_BYTES {
        return Err(image_attachment_too_large_error());
    }
    Ok(())
}

fn ensure_image_attachment_mime_type(mime_type: &str) -> Result<(), SessionMessageRouteError> {
    if mime_type
        .trim()
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/"))
    {
        return Ok(());
    }
    Err(SessionMessageRouteError::unsupported_media_type(
        "Only image attachments are supported.",
    ))
}

fn decoded_base64_len(data_base64: &str) -> Result<usize, SessionMessageRouteError> {
    let bytes = data_base64.as_bytes();
    if bytes.is_empty() {
        return Ok(0);
    }
    if !bytes.len().is_multiple_of(4) {
        return Err(SessionMessageRouteError::bad_request(
            "Invalid image attachment.",
        ));
    }
    let padding = if bytes.ends_with(b"==") {
        2
    } else if bytes.ends_with(b"=") {
        1
    } else {
        0
    };
    Ok((bytes.len() / 4) * 3 - padding)
}

fn decode_inline_image_attachment(data_base64: &str) -> Result<Vec<u8>, SessionMessageRouteError> {
    let decoded_len = decoded_base64_len(data_base64)?;
    ensure_image_attachment_size(decoded_len)?;
    base64::engine::general_purpose::STANDARD
        .decode(data_base64.as_bytes())
        .map_err(|_| SessionMessageRouteError::bad_request("Invalid image attachment."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_route_contracts::sessions::SessionMessageRouteErrorKind;
    use serde_json::json;

    #[test]
    fn post_request_adapter_exposes_optional_ids_delivery_and_attachments() {
        let request: PostSessionMessageRouteRequest = serde_json::from_value(json!({
            "id": MessageId::new().0.to_string(),
            "turn_id": TurnId::new().0.to_string(),
            "content": "hello",
            "delivery": "queued",
            "attachments": [{
                "kind": "image_ref",
                "blob_id": "blob",
                "mime_type": "image/png",
                "name": "pic.png"
            }]
        }))
        .unwrap();
        let (message_id, turn_id, content, delivery, attachments) = request.into_parts();
        assert!(message_id.is_some());
        assert!(turn_id.is_some());
        assert_eq!(content, "hello");
        assert!(matches!(
            delivery,
            Some(ctx_core::models::MessageDelivery::Queued)
        ));
        assert_eq!(attachments.len(), 1);
    }

    #[test]
    fn client_ids_trim_body_values_and_treat_empty_as_absent() {
        let message_id = MessageId::new();
        let turn_id = TurnId::new();
        assert_eq!(
            parse_optional_message_id(Some(&format!("  {}  ", message_id.0))).unwrap(),
            Some(message_id)
        );
        assert_eq!(
            parse_optional_turn_id(Some(&format!("\n{}\t", turn_id.0))).unwrap(),
            Some(turn_id)
        );
        assert_eq!(parse_optional_message_id(Some("   ")).unwrap(), None);
        assert_eq!(parse_optional_turn_id(Some("")).unwrap(), None);
    }

    #[test]
    fn post_path_session_id_is_not_trimmed() {
        let session_id = SessionId::new();
        let error = parse_post_session_id(SessionRouteParams::new(format!(" {} ", session_id.0)))
            .unwrap_err();
        assert_eq!(error.kind(), SessionMessageRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "Invalid session id.");
    }

    #[test]
    fn post_id_errors_preserve_existing_messages() {
        let error = parse_optional_message_id(Some("not-a-message")).unwrap_err();
        assert_eq!(error.kind(), SessionMessageRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "Invalid message id.");

        let error = parse_optional_turn_id(Some("not-a-turn")).unwrap_err();
        assert_eq!(error.kind(), SessionMessageRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "Invalid turn id.");
    }

    #[test]
    fn partial_client_ids_preserve_resolution_message() {
        let message_id = MessageId::new();
        let error =
            resolve_message_client_ids(Some(message_id), None).map_err(client_id_resolution_error);
        let error = error.unwrap_err();
        assert_eq!(error.kind(), SessionMessageRouteErrorKind::BadRequest);
        assert_eq!(
            error.message(),
            "Message id and turn id must either both be provided or both be omitted."
        );
    }

    #[test]
    fn delete_id_errors_are_bare_status_style_messages() {
        let error = parse_message_id("not-a-message").unwrap_err();
        assert_eq!(error.kind(), SessionMessageRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid message id");
    }

    #[test]
    fn env_bool_preserves_current_values() {
        for value in ["1", "true", "yes", "on"] {
            assert_eq!(env_bool(Some(value)), Some(true));
        }
        for value in ["0", "false", "no", "off"] {
            assert_eq!(env_bool(Some(value)), Some(false));
        }
        assert_eq!(env_bool(None), None);
        assert_eq!(env_bool(Some("maybe")), None);
    }

    #[test]
    fn post_user_message_input_preserves_daemon_owned_context_values() {
        let client_ids = resolve_message_client_ids(None, None).unwrap();
        let input = post_user_message_input_for_route(
            client_ids,
            "hello".to_string(),
            Some(MessageDelivery::Queued),
            Vec::new(),
            true,
            Some("run".to_string()),
        );

        assert_eq!(input.message_id, client_ids.message_id);
        assert_eq!(input.turn_id, client_ids.turn_id);
        assert!(!input.client_supplied_ids);
        assert_eq!(input.content, "hello");
        assert!(matches!(
            input.requested_delivery,
            Some(MessageDelivery::Queued)
        ));
        assert!(input.attachments.is_empty());
        assert!(input.queued_messages_enabled);
        assert_eq!(input.run_id_header.as_deref(), Some("run"));
    }

    #[test]
    fn queued_message_env_policy_is_daemon_owned() {
        assert_eq!(env_bool(Some("true")), Some(true));
        assert_eq!(env_bool(Some("false")), Some(false));
        assert_eq!(env_bool(Some("unknown")), None);
    }

    #[test]
    fn image_attachment_validation_preserves_errors() {
        assert_eq!(decoded_base64_len("YQ==").unwrap(), 1);
        assert_eq!(decoded_base64_len("YWE=").unwrap(), 2);
        assert_eq!(decoded_base64_len("YWFh").unwrap(), 3);

        let error = decode_inline_image_attachment("bad").unwrap_err();
        assert_eq!(error.kind(), SessionMessageRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "Invalid image attachment.");

        let error = ensure_image_attachment_mime_type("text/plain").unwrap_err();
        assert_eq!(
            error.kind(),
            SessionMessageRouteErrorKind::UnsupportedMediaType
        );
        assert_eq!(error.message(), "Only image attachments are supported.");

        let error =
            ensure_image_attachment_size(MAX_MESSAGE_IMAGE_ATTACHMENT_BYTES + 1).unwrap_err();
        assert_eq!(error.kind(), SessionMessageRouteErrorKind::PayloadTooLarge);
        assert_eq!(
            error.message(),
            "Image attachments must be 25 MiB or smaller."
        );
    }

    #[test]
    fn command_error_classification_preserves_status_categories() {
        let delete_error = delete_message_route_error(SessionSchedulerCommandError::BadRequest);
        assert_eq!(
            delete_error.kind(),
            SessionMessageRouteErrorKind::BadRequest
        );

        let post_error = post_user_message_route_error(
            crate::daemon::sessions::PostUserMessageError::ServiceUnavailable(
                "retry later".to_string(),
            ),
        );
        assert_eq!(
            post_error.kind(),
            SessionMessageRouteErrorKind::ServiceUnavailable
        );
        assert_eq!(post_error.message(), "retry later");
    }
}
