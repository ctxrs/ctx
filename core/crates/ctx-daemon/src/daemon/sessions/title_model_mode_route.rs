use ctx_core::ids::SessionId;
use ctx_observability::logs;
use ctx_route_contracts::sessions::{
    GenerateSessionTitleRouteRequest, GenerateSessionTitleRouteResponse, SessionRouteParams,
    SessionTitleModelModeRouteError, SetSessionModeRouteRequest, SetSessionModelRouteRequest,
    SetSessionModelRouteResponse,
};

use crate::daemon::sessions::route_contract::parse_session_route_id;
use crate::daemon::sessions::{
    GenerateSessionTitleError, SetSessionModeError, SetSessionModelError, SetSessionModelErrorKind,
};
use crate::daemon::SessionTitleModelModeHandle;

impl SessionTitleModelModeHandle {
    pub async fn generate_session_title_for_route(
        &self,
        params: SessionRouteParams,
        request: GenerateSessionTitleRouteRequest,
    ) -> Result<GenerateSessionTitleRouteResponse, SessionTitleModelModeRouteError> {
        let session_id = parse_title_model_mode_session_id(params)?;
        let (prompt, force) = request.into_parts();
        self.generate_session_title_for_request(session_id, prompt, force)
            .await
            .map(GenerateSessionTitleRouteResponse::new)
            .map_err(generate_title_route_error)
    }

    pub async fn set_session_model_for_route(
        &self,
        params: SessionRouteParams,
        request: SetSessionModelRouteRequest,
    ) -> Result<SetSessionModelRouteResponse, SessionTitleModelModeRouteError> {
        let session_id = parse_title_model_mode_session_id(params)?;
        let (model_id, reasoning_effort) = request.into_parts();
        self.set_session_model_for_request(
            session_id,
            crate::daemon::sessions::SetSessionModelRequest {
                model_id,
                reasoning_effort,
            },
        )
        .await
        .map(SetSessionModelRouteResponse::new)
        .map_err(set_model_route_error)
    }

    pub async fn set_session_mode_for_route(
        &self,
        params: SessionRouteParams,
        request: SetSessionModeRouteRequest,
    ) -> Result<(), SessionTitleModelModeRouteError> {
        let session_id = parse_title_model_mode_session_id(params)?;
        self.set_session_mode_for_request(session_id, request.into_mode_id())
            .await
            .map_err(set_mode_route_error)
    }
}

fn parse_title_model_mode_session_id(
    params: SessionRouteParams,
) -> Result<SessionId, SessionTitleModelModeRouteError> {
    parse_session_route_id(params.session_id())
        .map_err(|_| SessionTitleModelModeRouteError::bad_request("invalid session id"))
}

fn generate_title_route_error(error: GenerateSessionTitleError) -> SessionTitleModelModeRouteError {
    match error {
        GenerateSessionTitleError::NotFound => {
            SessionTitleModelModeRouteError::not_found("session not found")
        }
        GenerateSessionTitleError::PromptRequired => {
            SessionTitleModelModeRouteError::bad_request("prompt required")
        }
        GenerateSessionTitleError::Skipped => {
            SessionTitleModelModeRouteError::bad_request("title generation skipped")
        }
        GenerateSessionTitleError::Internal(error) => {
            SessionTitleModelModeRouteError::internal(logs::redact_sensitive(&error.to_string()))
        }
    }
}

fn set_model_route_error(error: SetSessionModelError) -> SessionTitleModelModeRouteError {
    match error.kind() {
        SetSessionModelErrorKind::BadRequest => {
            SessionTitleModelModeRouteError::bad_request(error.message())
        }
        SetSessionModelErrorKind::NotFound => {
            SessionTitleModelModeRouteError::not_found(error.message())
        }
        SetSessionModelErrorKind::Forbidden => {
            SessionTitleModelModeRouteError::forbidden(error.message())
        }
        SetSessionModelErrorKind::InsufficientStorage => {
            SessionTitleModelModeRouteError::insufficient_storage(error.message())
        }
        SetSessionModelErrorKind::ProviderUnavailable => {
            SessionTitleModelModeRouteError::provider_unavailable(error.message())
        }
        SetSessionModelErrorKind::LiveSwitchRejected => {
            SessionTitleModelModeRouteError::live_switch_rejected(error.message())
        }
        SetSessionModelErrorKind::Internal => {
            SessionTitleModelModeRouteError::internal(error.message())
        }
    }
}

fn set_mode_route_error(error: SetSessionModeError) -> SessionTitleModelModeRouteError {
    match error {
        SetSessionModeError::NotFound => {
            SessionTitleModelModeRouteError::not_found("session not found")
        }
        SetSessionModeError::BadRequest => {
            SessionTitleModelModeRouteError::bad_request("bad request")
        }
        SetSessionModeError::Internal => {
            SessionTitleModelModeRouteError::internal("internal server error")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_route_contracts::sessions::SessionTitleModelModeRouteErrorKind;

    #[test]
    fn invalid_session_id_uses_existing_route_message() {
        let error = parse_title_model_mode_session_id(SessionRouteParams::new("not-a-session"))
            .unwrap_err();
        assert_eq!(
            error.kind(),
            SessionTitleModelModeRouteErrorKind::BadRequest
        );
        assert_eq!(error.message(), "invalid session id");
    }

    #[test]
    fn title_errors_preserve_messages_and_redact_internal() {
        assert_eq!(
            generate_title_route_error(GenerateSessionTitleError::NotFound).message(),
            "session not found"
        );
        assert_eq!(
            generate_title_route_error(GenerateSessionTitleError::PromptRequired).message(),
            "prompt required"
        );
        assert_eq!(
            generate_title_route_error(GenerateSessionTitleError::Skipped).message(),
            "title generation skipped"
        );
        let raw_message = "CTX_MCP_TOKEN=secret-token-123";
        let error = generate_title_route_error(GenerateSessionTitleError::Internal(
            anyhow::anyhow!(raw_message),
        ));
        assert_eq!(error.kind(), SessionTitleModelModeRouteErrorKind::Internal);
        assert_eq!(error.message(), logs::redact_sensitive(raw_message));
        assert!(!error.message().contains("secret-token-123"));
    }

    #[test]
    fn model_error_kind_mapping_preserves_status_categories() {
        for (kind, expected_kind) in [
            (
                SetSessionModelErrorKind::BadRequest,
                SessionTitleModelModeRouteErrorKind::BadRequest,
            ),
            (
                SetSessionModelErrorKind::NotFound,
                SessionTitleModelModeRouteErrorKind::NotFound,
            ),
            (
                SetSessionModelErrorKind::Forbidden,
                SessionTitleModelModeRouteErrorKind::Forbidden,
            ),
            (
                SetSessionModelErrorKind::InsufficientStorage,
                SessionTitleModelModeRouteErrorKind::InsufficientStorage,
            ),
            (
                SetSessionModelErrorKind::ProviderUnavailable,
                SessionTitleModelModeRouteErrorKind::ProviderUnavailable,
            ),
            (
                SetSessionModelErrorKind::LiveSwitchRejected,
                SessionTitleModelModeRouteErrorKind::LiveSwitchRejected,
            ),
            (
                SetSessionModelErrorKind::Internal,
                SessionTitleModelModeRouteErrorKind::Internal,
            ),
        ] {
            let error = set_model_route_error(SetSessionModelError::new(kind, "model message"));
            assert_eq!(error.kind(), expected_kind);
            assert_eq!(error.message(), "model message");
        }
    }

    #[test]
    fn mode_error_mapping_preserves_bare_status_categories() {
        assert_eq!(
            set_mode_route_error(SetSessionModeError::NotFound).kind(),
            SessionTitleModelModeRouteErrorKind::NotFound
        );
        assert_eq!(
            set_mode_route_error(SetSessionModeError::BadRequest).kind(),
            SessionTitleModelModeRouteErrorKind::BadRequest
        );
        assert_eq!(
            set_mode_route_error(SetSessionModeError::Internal).kind(),
            SessionTitleModelModeRouteErrorKind::Internal
        );
    }
}
