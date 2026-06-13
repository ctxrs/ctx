use super::super::helpers::should_track_thought_chunk;
use super::assistant::handle_assistant_complete;
use super::state::EventLoopRuntimeState;
use super::terminal::{
    handle_done_event, handle_session_gap_notice, handle_turn_finished, handle_turn_interrupted,
};
use super::tools::handle_persisted_tool_event;
use super::TurnEventLoop;
use ctx_core::models::{SessionEvent, SessionEventType};
use ctx_session_tools::NormalizedToolEvent;
use serde_json::Value;

pub(super) async fn handle_persisted_provider_event(
    ctx: &TurnEventLoop,
    runtime: &mut EventLoopRuntimeState,
    event: &SessionEvent,
    raw_payload: &Value,
    normalized_tool_event: Option<&NormalizedToolEvent>,
) {
    match &event.event_type {
        SessionEventType::AssistantChunk => {
            if let Some(fragment) = raw_payload.get("content_fragment").and_then(Value::as_str) {
                runtime.assistant_partial.push_str(fragment);
                if let Some(message_id) = raw_payload
                    .get("message_id")
                    .and_then(Value::as_str)
                    .map(|value| value.to_string())
                {
                    runtime.assistant_partial_message_id = Some(message_id);
                }
            }
        }
        SessionEventType::ThoughtChunk if should_track_thought_chunk(raw_payload) => {
            if let Some(fragment) = raw_payload.get("content_fragment").and_then(Value::as_str) {
                runtime.thought_partial.push_str(fragment);
                let _ = ctx
                    .store
                    .update_session_turn_partial(
                        ctx.session_id,
                        ctx.turn_id,
                        None,
                        Some(&runtime.thought_partial),
                        event.created_at,
                    )
                    .await;
            }
        }
        SessionEventType::Notice
            if event
                .payload_json
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "session_gap") =>
        {
            handle_session_gap_notice(ctx, event).await;
        }
        SessionEventType::ToolCall
        | SessionEventType::ToolCallUpdate
        | SessionEventType::ToolResult => {
            if let Some(tool_event) = normalized_tool_event {
                handle_persisted_tool_event(ctx, runtime, event, tool_event).await;
            }
        }
        SessionEventType::AssistantComplete => {
            handle_assistant_complete(ctx, runtime, event).await;
        }
        SessionEventType::Done => {
            handle_done_event(ctx, runtime).await;
        }
        SessionEventType::TurnInterrupted => {
            handle_turn_interrupted(ctx, runtime, event).await;
        }
        SessionEventType::TurnFinished => {
            handle_turn_finished(ctx, runtime, event).await;
        }
        _ => {}
    }
}
