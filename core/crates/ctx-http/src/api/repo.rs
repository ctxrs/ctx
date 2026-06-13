use axum::extract::{Extension, Query, State};
use axum::http::StatusCode;
use axum::Json;

use super::errors::ApiErrorResp;
use crate::api::MobileAuthContext;
use ctx_daemon::daemon::RepoOnboardingHandle;
use ctx_route_contracts::repo_onboarding::{
    RepoCloneRouteRequest, RepoInitRouteRequest, RepoOnboardingRouteError,
    RepoOnboardingRouteErrorKind, RepoPathRouteResponse, RepoStatusRouteRequest,
    RepoStatusRouteResponse, RepoValidateDestinationRouteRequest,
};

mod auth;
mod clone;
mod destination;
mod init;
mod status;

use auth::reject_mobile_auth;
pub(super) use clone::repo_clone;
pub(super) use destination::{
    repo_staging_path, repo_validate_destination, repo_validate_destination_get,
};
pub(super) use init::repo_init;
pub(super) use status::repo_status;

fn repo_onboarding_error_response(
    error: RepoOnboardingRouteError,
) -> (StatusCode, Json<ApiErrorResp>) {
    let status = match error.kind() {
        RepoOnboardingRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        RepoOnboardingRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}
