use super::*;
use ctx_daemon::daemon::HealthHandle;
use ctx_route_contracts::health::DaemonHealthSnapshot;

pub(in crate::api) async fn health(
    State(state): State<HealthHandle>,
    headers: HeaderMap,
) -> Result<Json<DaemonHealthSnapshot>, StatusCode> {
    let include_sensitive = health_request_is_authorized(&state, &headers);
    let snapshot = state
        .health_snapshot(env!("CARGO_PKG_VERSION"), include_sensitive)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(snapshot))
}

#[derive(Debug, Serialize)]
pub(in crate::api) struct DevClockResp {
    pub daemon_unix_ms: i64,
}

pub(in crate::api) async fn dev_clock() -> Json<DevClockResp> {
    Json(DevClockResp {
        daemon_unix_ms: chrono::Utc::now().timestamp_millis(),
    })
}

fn health_request_is_authorized(state: &HealthHandle, headers: &HeaderMap) -> bool {
    let Some(expected) = state.auth_token() else {
        return true;
    };
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|value| value == expected)
}
