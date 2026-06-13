use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use ctx_daemon::daemon::UpdateReleaseHandle;
use ctx_update_service::route_contract::{
    DownloadAppImageUpdateRequest, DownloadAppImageUpdateResult,
};

use crate::api::errors::ApiErrorResp;
use crate::api::updates::update_route_error;

pub(in crate::api) async fn download_appimage_update(
    State(updates): State<UpdateReleaseHandle>,
    Json(req): Json<DownloadAppImageUpdateRequest>,
) -> Result<Json<DownloadAppImageUpdateResult>, (StatusCode, Json<ApiErrorResp>)> {
    updates
        .download_appimage_update(env!("CARGO_PKG_VERSION"), req)
        .await
        .map(Json)
        .map_err(update_route_error)
}
