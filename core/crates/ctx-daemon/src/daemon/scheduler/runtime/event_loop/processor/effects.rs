use super::super::dispatch::handle_persisted_provider_event;
use super::super::state::EventLoopRuntimeState;
use super::super::terminal::is_truthful_start_activity;
use super::super::TurnEventLoop;
use crate::daemon::scheduler::host::TurnEventLoopHost;
use ctx_core::models::{SessionEvent, SessionEventType, SessionTurnStatus};
use ctx_session_tools::NormalizedToolEvent;

pub(super) async fn handle_persisted_provider_event_effects(
    ctx: &TurnEventLoop,
    host: &TurnEventLoopHost,
    runtime: &mut EventLoopRuntimeState,
    event: SessionEvent,
    raw_payload: serde_json::Value,
    normalized_tool_event: Option<&NormalizedToolEvent>,
) {
    let publish_after_persist = matches!(
        &event.event_type,
        SessionEventType::ToolCall
            | SessionEventType::ToolCallUpdate
            | SessionEventType::ToolResult
    );
    if !publish_after_persist {
        host.publish_event(event.clone()).await;
    }

    if is_truthful_start_activity(&event.event_type)
        && runtime.promote_started_if_pending(&ctx.start_progress_tx)
    {
        let _ = ctx
            .store
            .update_session_turn_status(
                ctx.session_id,
                ctx.turn_id,
                SessionTurnStatus::Running,
                None,
                None,
                event.created_at,
            )
            .await;
    }

    handle_persisted_provider_event(ctx, runtime, &event, &raw_payload, normalized_tool_event)
        .await;

    if publish_after_persist {
        host.publish_event(event.clone()).await;
    }
}
