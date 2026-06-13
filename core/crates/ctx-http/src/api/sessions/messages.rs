use super::*;

type ApiErr = (StatusCode, Json<ApiErrorResp>);

mod delete;
mod post;

pub(crate) use delete::delete_session_message;
pub(crate) use post::post_message;

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_route_contracts::sessions::SessionMessageRouteError;

    #[test]
    fn post_message_errors_are_json_and_delete_errors_are_bare_statuses() {
        let (status, body) =
            session_message_api_error(SessionMessageRouteError::payload_too_large("too large"));
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body.0.error, "too large");

        assert_eq!(
            session_message_bare_status(SessionMessageRouteError::unsupported_media_type(
                "bad type"
            )),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
    }
}
