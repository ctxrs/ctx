use ctx_core::models::SessionEvent;
use ctx_session_tools::interrupt_telemetry::{latency_bucket, metric_labels};
use serde_json::Value;

use crate::daemon::scheduler::host::TurnEventLoopHost;
use ctx_observability::perf_telemetry::{PerfMetric, PerfMetricKind};
use ctx_observability::telemetry::TelemetryEvent;

use super::state::EventLoopRuntimeState;
use super::TurnEventLoop;

mod failed_turn;

pub(super) async fn record_terminal_run_telemetry(
    ctx: &TurnEventLoop,
    runtime: &mut EventLoopRuntimeState,
    host: &TurnEventLoopHost,
    event_label: &'static str,
    provider_call_success: bool,
    session_status: &'static str,
) {
    if runtime.telemetry_emitted {
        return;
    }

    runtime.telemetry_emitted = true;
    let duration_ms = ctx.run_started_at.elapsed().as_millis() as u64;
    let run_metric = PerfMetric {
        name: "scheduler.run_total_ms".to_string(),
        kind: PerfMetricKind::Histogram,
        unit: "ms".to_string(),
        value: duration_ms as f64,
        labels: metric_labels(
            &ctx.provider_id,
            &ctx.model_id,
            &ctx.execution_environment_label,
            &ctx.session_root_kind,
            event_label,
        ),
    };
    host.record_perf_metric(run_metric, ctx.perf_run_id.clone())
        .await;
    host.emit_telemetry(TelemetryEvent::provider_call(
        ctx.provider_id.clone(),
        ctx.model_id.clone(),
        Some(ctx.execution_environment_label.clone()),
        Some(ctx.session_root_kind.clone()),
        provider_call_success,
        duration_ms,
    ))
    .await;
    host.emit_telemetry(TelemetryEvent::session_completed(
        ctx.provider_id.clone(),
        ctx.model_id.clone(),
        Some(ctx.execution_environment_label.clone()),
        Some(ctx.session_root_kind.clone()),
        session_status.to_string(),
        duration_ms,
    ))
    .await;
}

pub(super) async fn record_interrupt_visible_telemetry(
    ctx: &TurnEventLoop,
    host: &TurnEventLoopHost,
    event: &SessionEvent,
) {
    let Some(requested_at_ms) = event
        .payload_json
        .get("requested_at_ms")
        .and_then(Value::as_i64)
    else {
        return;
    };

    let latency_ms = (event.created_at.timestamp_millis() - requested_at_ms).max(0) as u64;
    let bucket = latency_bucket(latency_ms).to_string();
    let interrupt_metric = PerfMetric {
        name: "scheduler.interrupt_total_ms".to_string(),
        kind: PerfMetricKind::Histogram,
        unit: "ms".to_string(),
        value: latency_ms as f64,
        labels: metric_labels(
            &ctx.provider_id,
            &ctx.model_id,
            &ctx.execution_environment_label,
            &ctx.session_root_kind,
            "turn_interrupted_visible",
        ),
    };
    host.record_perf_metric(interrupt_metric, ctx.perf_run_id.clone())
        .await;
    host.emit_telemetry(TelemetryEvent::session_interrupt_latency(
        ctx.provider_id.clone(),
        ctx.model_id.clone(),
        Some(ctx.execution_environment_label.clone()),
        Some(ctx.session_root_kind.clone()),
        latency_ms,
        bucket.clone(),
    ))
    .await;
    let interrupt_id = event
        .payload_json
        .get("interrupt_id")
        .and_then(Value::as_str);
    tracing::info!(
        session_id = %ctx.session_id.0,
        run_id = %ctx.run_id.0,
        turn_id = %ctx.turn_id.0,
        interrupt_id,
        interrupt_total_ms = latency_ms,
        duration_bucket = %bucket,
        "session interrupt became visible in event loop"
    );
}

pub(super) async fn record_failed_turn_telemetry(
    ctx: &TurnEventLoop,
    runtime: &mut EventLoopRuntimeState,
    host: &TurnEventLoopHost,
    error_message: String,
    details: Option<Value>,
    kind: Option<Value>,
) {
    record_terminal_run_telemetry(ctx, runtime, host, "run_failed", false, "failed").await;
    failed_turn::emit_failed_turn_ops_event(ctx, host, error_message, details, kind);
}
