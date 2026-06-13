use ctx_core::models::{SessionEvent, SessionEventType};
use serde_json::Value;

use super::TurnEventLoop;

mod status;
pub(super) use status::{handle_done_event, handle_turn_finished, handle_turn_interrupted};

pub(super) fn is_truthful_start_activity(event_type: &SessionEventType) -> bool {
    matches!(
        event_type,
        SessionEventType::TurnStarted
            | SessionEventType::AssistantChunk
            | SessionEventType::ThoughtChunk
            | SessionEventType::AssistantComplete
            | SessionEventType::ContextWindowUpdate
            | SessionEventType::ToolCall
            | SessionEventType::ToolCallUpdate
            | SessionEventType::ToolResult
            | SessionEventType::Done
            | SessionEventType::TurnInterrupted
            | SessionEventType::TurnFinished
    )
}

pub(super) async fn handle_session_gap_notice(ctx: &TurnEventLoop, event: &SessionEvent) {
    let Some(host) = ctx.host() else {
        return;
    };
    let reason = event
        .payload_json
        .get("reason")
        .and_then(Value::as_str)
        .map(|value| value.to_string());
    host.publish_session_gap(ctx.workspace_id, ctx.session_id, event.seq, reason)
        .await;
}
