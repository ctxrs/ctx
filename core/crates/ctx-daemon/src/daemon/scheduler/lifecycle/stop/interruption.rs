use serde_json::json;

use ctx_core::ids::SessionId;
use ctx_core::models::SessionEventType;
use ctx_providers::adapters::ProviderTurnOutcome;
use ctx_session_tools::interrupt_telemetry::{payload_fields, InterruptTelemetryContext};

use crate::daemon::scheduler::host::WorkerLifecycleHost;

use super::super::super::persistence::emit_event_with_host;
use super::super::RunningTurn;

pub(super) async fn emit_interrupt_requested_event(
    lifecycle: &WorkerLifecycleHost,
    session_id: SessionId,
    turn: &RunningTurn,
    interrupt: Option<&InterruptTelemetryContext>,
) {
    let mut payload = json!({"by":"user"});
    if let Some(interrupt) = interrupt {
        if let Some(obj) = payload.as_object_mut() {
            let extra = payload_fields(interrupt);
            if let Some(extra_obj) = extra.as_object() {
                for (key, value) in extra_obj {
                    obj.insert(key.clone(), value.clone());
                }
            }
        }
    }
    let _ = emit_event_with_host(
        lifecycle,
        session_id,
        Some(turn.run_id),
        Some(turn.turn_id),
        SessionEventType::InterruptRequested,
        payload,
    )
    .await;
}

pub(super) fn interrupted_fallback_outcome(
    reason: &str,
    provider_cancelled: bool,
) -> ProviderTurnOutcome {
    ProviderTurnOutcome {
        terminal_event_emitted: false,
        ..ProviderTurnOutcome::interrupted(reason, provider_cancelled)
    }
}
