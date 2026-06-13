use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;

use super::super::errors::ApiErrorResp;
use ctx_route_contracts::sessions::{
    GenerateSessionTitleRouteRequest, GenerateSessionTitleRouteResponse, SessionRouteParams,
    SessionTitleModelModeRouteError, SessionTitleModelModeRouteErrorKind,
    SetSessionModeRouteRequest, SetSessionModelRouteRequest, SetSessionModelRouteResponse,
};

mod mode;
mod model;
mod title;

pub(crate) use mode::set_session_mode;
pub(crate) use model::set_session_model;
pub(crate) use title::generate_session_title;

#[cfg(test)]
use crate::test_support::TestDaemonFixture;

type ApiErr = (StatusCode, Json<ApiErrorResp>);

fn session_title_model_mode_status(error: &SessionTitleModelModeRouteError) -> StatusCode {
    match error.kind() {
        SessionTitleModelModeRouteErrorKind::BadRequest
        | SessionTitleModelModeRouteErrorKind::LiveSwitchRejected => StatusCode::BAD_REQUEST,
        SessionTitleModelModeRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        SessionTitleModelModeRouteErrorKind::Forbidden => StatusCode::FORBIDDEN,
        SessionTitleModelModeRouteErrorKind::InsufficientStorage => {
            StatusCode::INSUFFICIENT_STORAGE
        }
        SessionTitleModelModeRouteErrorKind::ProviderUnavailable
        | SessionTitleModelModeRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn session_title_model_mode_api_error(error: SessionTitleModelModeRouteError) -> ApiErr {
    let status = session_title_model_mode_status(&error);
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}

fn session_title_model_mode_bare_status(error: SessionTitleModelModeRouteError) -> StatusCode {
    session_title_model_mode_status(&error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_core::ids::SessionId;
    use serde::de::DeserializeOwned;
    use serde_json::json;

    fn route_request<T: DeserializeOwned>(value: serde_json::Value) -> T {
        serde_json::from_value(value).unwrap()
    }

    #[tokio::test]
    async fn title_route_errors_are_json_api_errors() {
        let fixture = TestDaemonFixture::new("http://127.0.0.1:0").await;
        let sessions = fixture.daemon().session_title_model_mode_handle_for_test();

        let err = generate_session_title(
            State(sessions.clone()),
            Path("not-a-session".to_string()),
            Json(route_request::<GenerateSessionTitleRouteRequest>(json!({}))),
        )
        .await
        .unwrap_err();

        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1 .0.error, "invalid session id");

        let err = generate_session_title(
            State(sessions),
            Path(SessionId::new().0.to_string()),
            Json(route_request::<GenerateSessionTitleRouteRequest>(json!({}))),
        )
        .await
        .unwrap_err();

        assert_eq!(err.0, StatusCode::NOT_FOUND);
        assert_eq!(err.1 .0.error, "session not found");
    }

    #[tokio::test]
    async fn model_route_errors_are_json_api_errors() {
        let fixture = TestDaemonFixture::new("http://127.0.0.1:0").await;
        let sessions = fixture.daemon().session_title_model_mode_handle_for_test();

        let err = set_session_model(
            State(sessions),
            Path("not-a-session".to_string()),
            Json(route_request::<SetSessionModelRouteRequest>(json!({
                "model_id": "fake-model"
            }))),
        )
        .await
        .unwrap_err();

        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1 .0.error, "invalid session id");
    }

    #[tokio::test]
    async fn mode_route_errors_are_bare_status_codes() {
        let fixture = TestDaemonFixture::new("http://127.0.0.1:0").await;
        let sessions = fixture.daemon().session_title_model_mode_handle_for_test();

        let err = set_session_mode(
            State(sessions.clone()),
            Path("not-a-session".to_string()),
            Json(route_request::<SetSessionModeRouteRequest>(json!({
                "mode_id": "plan"
            }))),
        )
        .await
        .unwrap_err();

        assert_eq!(err, StatusCode::BAD_REQUEST);

        let err = set_session_mode(
            State(sessions),
            Path(SessionId::new().0.to_string()),
            Json(route_request::<SetSessionModeRouteRequest>(json!({
                "mode_id": "plan"
            }))),
        )
        .await
        .unwrap_err();

        assert_eq!(err, StatusCode::NOT_FOUND);
    }
}
