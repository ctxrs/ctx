use super::*;
use ctx_update_service::route_contract::{UpdateRouteError, UpdateRouteErrorKind};

mod activity;
mod appimage;
mod check;
mod drain;

pub(super) use activity::update_activity;
pub(super) use appimage::{apply_appimage_update, download_appimage_update};
pub(super) use check::check_updates;
pub(super) use drain::{begin_update_drain, release_update_drain, shutdown_daemon};

fn update_route_error(error: UpdateRouteError) -> (StatusCode, Json<ApiErrorResp>) {
    let status = match error.kind() {
        UpdateRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        UpdateRouteErrorKind::BadGateway => StatusCode::BAD_GATEWAY,
        UpdateRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}
