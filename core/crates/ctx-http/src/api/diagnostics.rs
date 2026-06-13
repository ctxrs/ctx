use super::*;
use ctx_daemon::daemon::DiagnosticsHandle;
use ctx_route_contracts::diagnostics::DaemonDiagnosticsSnapshot;

pub(in crate::api) async fn diagnostics(
    State(diagnostics): State<DiagnosticsHandle>,
) -> Result<Json<DaemonDiagnosticsSnapshot>, StatusCode> {
    let snapshot = diagnostics
        .diagnostics_snapshot(env!("CARGO_PKG_VERSION"))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(snapshot))
}
