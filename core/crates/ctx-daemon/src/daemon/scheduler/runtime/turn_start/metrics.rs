use ctx_core::models::Session;

use crate::daemon::scheduler::host::TurnRuntimeHost;

pub(super) async fn record_queue_wait_metric(
    host: &TurnRuntimeHost,
    session: &Session,
    full_model_id: &str,
    execution_environment: &str,
    session_root_kind: &str,
    perf_run_id: Option<String>,
    queue_wait_ms: u64,
) {
    host.record_queue_wait_metric(
        session,
        full_model_id,
        execution_environment,
        session_root_kind,
        perf_run_id,
        queue_wait_ms,
    )
    .await;
}
