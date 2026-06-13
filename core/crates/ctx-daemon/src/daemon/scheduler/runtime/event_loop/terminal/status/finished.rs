use ctx_core::models::{SessionEvent, SessionEventType, SessionTurnStatus};
use ctx_core::session_projection::terminal_status_from_finished_payload;
use serde_json::Value;

use super::super::super::state::EventLoopRuntimeState;
use super::super::super::telemetry::{record_failed_turn_telemetry, record_terminal_run_telemetry};
use super::super::super::TurnEventLoop;

pub(in crate::daemon::scheduler::runtime::event_loop) async fn handle_turn_finished(
    ctx: &TurnEventLoop,
    runtime: &mut EventLoopRuntimeState,
    event: &SessionEvent,
) {
    let Some(status) = terminal_status_from_finished_payload(&event.payload_json) else {
        return;
    };
    let Some(host) = ctx.host() else {
        return;
    };
    runtime.promote_terminal(&ctx.start_progress_tx);
    if runtime.terminal_status.is_some() {
        return;
    }
    match status {
        SessionTurnStatus::Completed => {
            record_terminal_run_telemetry(
                ctx,
                runtime,
                host.as_ref(),
                "run_complete",
                true,
                "completed",
            )
            .await;
        }
        SessionTurnStatus::Failed => {
            let error_message = event
                .payload_json
                .get("message")
                .or_else(|| event.payload_json.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("provider runtime error")
                .to_string();
            record_failed_turn_telemetry(
                ctx,
                runtime,
                host.as_ref(),
                error_message,
                event.payload_json.get("details").cloned(),
                event.payload_json.get("kind").cloned(),
            )
            .await;
        }
        SessionTurnStatus::Interrupted => {
            record_terminal_run_telemetry(
                ctx,
                runtime,
                host.as_ref(),
                "run_interrupt",
                false,
                "interrupted",
            )
            .await;
        }
        SessionTurnStatus::Queued | SessionTurnStatus::Starting | SessionTurnStatus::Running => {}
    }
    runtime.terminal_status = Some(status);
    let _ = ctx
        .store
        .delete_session_events_for_turn_types(
            ctx.session_id,
            ctx.turn_id,
            &[
                SessionEventType::AssistantChunk,
                SessionEventType::ThoughtChunk,
                SessionEventType::ContextWindowUpdate,
            ],
        )
        .await;
}
