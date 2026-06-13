use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use ctx_session_artifacts::route_contract::SessionArtifactRouteError;

use super::super::errors::ApiErrorResp;

mod list;
mod set;

pub(in crate::api) use list::list_session_artifacts;
pub(in crate::api) use set::set_session_artifacts;

fn session_artifact_api_error(
    error: SessionArtifactRouteError,
) -> (StatusCode, Json<ApiErrorResp>) {
    match error {
        SessionArtifactRouteError::Unauthorized(error) => {
            (StatusCode::UNAUTHORIZED, Json(ApiErrorResp { error }))
        }
        SessionArtifactRouteError::NotFound => (
            StatusCode::NOT_FOUND,
            Json(ApiErrorResp {
                error: "session not found".to_string(),
            }),
        ),
        SessionArtifactRouteError::BadRequest(error) => {
            (StatusCode::BAD_REQUEST, Json(ApiErrorResp { error }))
        }
        SessionArtifactRouteError::Internal(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorResp { error }),
        ),
    }
}

pub(in crate::api::artifacts) fn session_artifact_status(
    error: SessionArtifactRouteError,
) -> StatusCode {
    match error {
        SessionArtifactRouteError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
        SessionArtifactRouteError::NotFound => StatusCode::NOT_FOUND,
        SessionArtifactRouteError::BadRequest(_) => StatusCode::BAD_REQUEST,
        SessionArtifactRouteError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
