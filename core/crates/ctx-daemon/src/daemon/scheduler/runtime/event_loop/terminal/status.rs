use ctx_core::models::{SessionEvent, SessionEventType, SessionTurnStatus};

use super::super::state::EventLoopRuntimeState;
use super::super::telemetry::{record_interrupt_visible_telemetry, record_terminal_run_telemetry};
use super::super::TurnEventLoop;

mod finished;
pub(in crate::daemon::scheduler::runtime::event_loop) use finished::handle_turn_finished;

pub(in crate::daemon::scheduler::runtime::event_loop) async fn handle_done_event(
    ctx: &TurnEventLoop,
    runtime: &mut EventLoopRuntimeState,
) {
    let Some(host) = ctx.host() else {
        return;
    };
    runtime.promote_terminal(&ctx.start_progress_tx);
    if runtime.terminal_status.is_some() {
        return;
    }
    record_terminal_run_telemetry(
        ctx,
        runtime,
        host.as_ref(),
        "run_complete",
        true,
        "completed",
    )
    .await;
    let _ = ctx
        .store
        .delete_session_events_for_turn_types(
            ctx.session_id,
            ctx.turn_id,
            &[
                SessionEventType::ThoughtChunk,
                SessionEventType::ContextWindowUpdate,
            ],
        )
        .await;
    runtime.terminal_status = Some(SessionTurnStatus::Completed);
}

pub(in crate::daemon::scheduler::runtime::event_loop) async fn handle_turn_interrupted(
    ctx: &TurnEventLoop,
    runtime: &mut EventLoopRuntimeState,
    event: &SessionEvent,
) {
    let Some(host) = ctx.host() else {
        return;
    };
    runtime.promote_terminal(&ctx.start_progress_tx);
    if runtime.terminal_status.is_some() {
        return;
    }
    record_terminal_run_telemetry(
        ctx,
        runtime,
        host.as_ref(),
        "run_interrupt",
        false,
        "interrupted",
    )
    .await;
    record_interrupt_visible_telemetry(ctx, host.as_ref(), event).await;
    runtime.terminal_status = Some(SessionTurnStatus::Interrupted);
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
