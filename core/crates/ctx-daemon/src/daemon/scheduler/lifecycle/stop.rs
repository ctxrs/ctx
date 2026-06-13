use serde_json::json;

use ctx_core::ids::SessionId;
use ctx_core::models::SessionEventType;
use ctx_session_tools::interrupt_telemetry::InterruptTelemetryContext;

use crate::daemon::scheduler::host::WorkerLifecycleHost;

use super::super::persistence::emit_event_with_host;
use super::terminalization::{
    finalize_provider_outcome_required, wait_for_provider_outcome, wait_for_turn_event_loop,
};
use super::{RunningTurn, StopReason};

mod interruption;

use interruption::{emit_interrupt_requested_event, interrupted_fallback_outcome};

pub async fn stop_running_turn(
    lifecycle: &WorkerLifecycleHost,
    session_id: SessionId,
    mut turn: RunningTurn,
    reason: StopReason,
    interrupt: Option<InterruptTelemetryContext>,
) -> bool {
    if matches!(reason, StopReason::StorageEmergency) {
        let _ = emit_event_with_host(
            lifecycle,
            session_id,
            Some(turn.run_id),
            Some(turn.turn_id),
            SessionEventType::Notice,
            json!({
                "kind": "storage_guard_kill",
                "message": "Storage emergency interrupted this session to protect local data.",
            }),
        )
        .await;
    }
    if let Some(interrupt) = interrupt.as_ref() {
        lifecycle
            .record_interrupt_request_telemetry(session_id, &turn, interrupt)
            .await;
    }
    if reason.should_emit_interrupt_requested() {
        emit_interrupt_requested_event(lifecycle, session_id, &turn, interrupt.as_ref()).await;
    }
    let run_id = turn.run_id;
    let turn_id = turn.turn_id;
    let cancel_started = std::time::Instant::now();
    let _ = turn.adapter.cancel(&mut turn.handle).await;
    if let Some(interrupt) = interrupt.as_ref() {
        let cancel_ms = cancel_started.elapsed().as_millis() as u64;
        lifecycle
            .record_provider_cancel_telemetry(session_id, &turn, interrupt, cancel_ms)
            .await;
    }
    let outcome = wait_for_provider_outcome(
        &mut turn.handle,
        interrupted_fallback_outcome(reason.missing_outcome_reason(), false),
        interrupted_fallback_outcome(reason.outcome_timeout_reason(), false),
    )
    .await;
    drop(turn.event_tx);
    let finalized = if let Some(events_done) = turn.events_done.take() {
        wait_for_turn_event_loop(session_id, run_id, turn_id, events_done).await;
        finalize_provider_outcome_required(
            lifecycle,
            session_id,
            Some(run_id),
            turn_id,
            turn.message_id,
            outcome,
        )
        .await
    } else {
        finalize_provider_outcome_required(
            lifecycle,
            session_id,
            Some(run_id),
            turn_id,
            turn.message_id,
            outcome,
        )
        .await
    };
    lifecycle.revoke_turn_mcp_token(&mut turn.mcp_token).await;
    lifecycle.set_running(session_id, false).await;
    reason.suspend_queue() || !finalized
}
