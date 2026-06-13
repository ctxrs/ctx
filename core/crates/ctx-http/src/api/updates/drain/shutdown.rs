use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;

use super::lease::maintenance_route_error;
use crate::api::errors::ApiErrorResp;
use ctx_daemon::daemon::DaemonShutdownHandle;
use ctx_update_service::route_contract::{ShutdownDaemonRouteRequest, ShutdownDaemonRouteResult};

const SHUTDOWN_TOKEN_HEADER_NAME: &str = "x-ctx-local-daemon-shutdown-token";

pub(in crate::api) async fn shutdown_daemon(
    State(shutdown): State<DaemonShutdownHandle>,
    headers: HeaderMap,
    Json(req): Json<ShutdownDaemonRouteRequest>,
) -> Result<Json<ShutdownDaemonRouteResult>, (StatusCode, Json<ApiErrorResp>)> {
    let supplied_token = headers
        .get(SHUTDOWN_TOKEN_HEADER_NAME)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let result = shutdown
        .request_daemon_shutdown_for_route(req.with_supplied_shutdown_token(supplied_token))
        .await
        .map_err(maintenance_route_error)?;
    Ok(Json(result))
}
