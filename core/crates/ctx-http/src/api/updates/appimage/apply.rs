use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use ctx_daemon::daemon::UpdateReleaseHandle;
use ctx_update_service::route_contract::{ApplyAppImageUpdateRequest, ApplyAppImageUpdateResult};

use crate::api::errors::ApiErrorResp;
use crate::api::updates::update_route_error;

pub(in crate::api) async fn apply_appimage_update(
    State(updates): State<UpdateReleaseHandle>,
    Json(req): Json<ApplyAppImageUpdateRequest>,
) -> Result<Json<ApplyAppImageUpdateResult>, (StatusCode, Json<ApiErrorResp>)> {
    updates
        .apply_appimage_update(env!("CARGO_PKG_VERSION"), req)
        .await
        .map(Json)
        .map_err(update_route_error)
}
