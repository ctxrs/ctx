use super::*;

use ctx_observability::perf_telemetry::{PerfMetric, PerfMetricKind};
use ctx_session_tools::interrupt_telemetry::metric_labels;

pub(in crate::daemon::scheduler::lifecycle::stop) async fn record_interrupt_request_telemetry(
    state: &Arc<DaemonState>,
    session_id: SessionId,
    turn: &RunningTurn,
    interrupt: &InterruptTelemetryContext,
) {
    record_interrupt_metric(state, turn, "request_age", interrupt.elapsed_ms()).await;
    tracing::info!(
        session_id = %session_id.0,
        run_id = %turn.run_id.0,
        turn_id = %turn.turn_id.0,
        interrupt_id = %interrupt.interrupt_id(),
        provider_id = %turn.provider_id,
        model_id = %turn.model_id,
        request_age_ms = interrupt.elapsed_ms(),
        "session interrupt requested"
    );
}

pub(in crate::daemon::scheduler::lifecycle::stop) async fn record_provider_cancel_telemetry(
    state: &Arc<DaemonState>,
    session_id: SessionId,
    turn: &RunningTurn,
    interrupt: &InterruptTelemetryContext,
    cancel_ms: u64,
) {
    record_interrupt_metric(state, turn, "provider_cancel", cancel_ms).await;
    tracing::info!(
        session_id = %session_id.0,
        run_id = %turn.run_id.0,
        turn_id = %turn.turn_id.0,
        interrupt_id = %interrupt.interrupt_id(),
        provider_cancel_ms = cancel_ms,
        interrupt_total_ms = interrupt.elapsed_ms(),
        "session interrupt provider cancel finished"
    );
}

async fn record_interrupt_metric(
    state: &Arc<DaemonState>,
    turn: &RunningTurn,
    event: &str,
    value_ms: u64,
) {
    let metric = PerfMetric {
        name: "scheduler.interrupt_latency_ms".to_string(),
        kind: PerfMetricKind::Histogram,
        unit: "ms".to_string(),
        value: value_ms as f64,
        labels: metric_labels(
            &turn.provider_id,
            &turn.model_id,
            &turn.execution_environment_label,
            &turn.session_root_kind,
            event,
        ),
    };
    state
        .telemetry
        .perf_telemetry
        .record_metric(metric, Some(turn.run_id.0.to_string()), None, None)
        .await;
}
