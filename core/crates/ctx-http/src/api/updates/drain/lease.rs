use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use ctx_daemon::daemon::UpdateDrainHandle;
use ctx_update_service::route_contract::{
    BeginUpdateDrainRouteRequest, BeginUpdateDrainRouteResult, MaintenanceRouteError,
    MaintenanceRouteErrorKind, ReleaseUpdateDrainRouteRequest, ReleaseUpdateDrainRouteResult,
};

use crate::api::errors::ApiErrorResp;

pub(in crate::api) async fn begin_update_drain(
    State(execution): State<UpdateDrainHandle>,
    Json(req): Json<BeginUpdateDrainRouteRequest>,
) -> Result<Json<BeginUpdateDrainRouteResult>, (StatusCode, Json<ApiErrorResp>)> {
    let result = execution
        .begin_update_drain_for_route(req)
        .await
        .map_err(maintenance_route_error)?;
    Ok(Json(result))
}

pub(in crate::api) async fn release_update_drain(
    State(execution): State<UpdateDrainHandle>,
    Json(req): Json<ReleaseUpdateDrainRouteRequest>,
) -> Result<Json<ReleaseUpdateDrainRouteResult>, (StatusCode, Json<ApiErrorResp>)> {
    let result = execution
        .release_update_drain_for_route(req)
        .await
        .map_err(maintenance_route_error)?;
    Ok(Json(result))
}

pub(super) fn maintenance_route_error(
    error: MaintenanceRouteError,
) -> (StatusCode, Json<ApiErrorResp>) {
    let status = match error.kind() {
        MaintenanceRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        MaintenanceRouteErrorKind::Conflict => StatusCode::CONFLICT,
        MaintenanceRouteErrorKind::Forbidden => StatusCode::FORBIDDEN,
        MaintenanceRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}
