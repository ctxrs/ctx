use ctx_core::models::{SessionEvent, SessionEventType};
use serde_json::Value;

use ctx_session_tools::order_seq::read_order_seq;

use self::persist::persist_assistant_complete_content;
use super::super::helpers::strip_emitted_prefix;
use super::state::EventLoopRuntimeState;
use super::TurnEventLoop;

mod persist;

pub(super) async fn handle_assistant_complete(
    ctx: &TurnEventLoop,
    runtime: &mut EventLoopRuntimeState,
    event: &SessionEvent,
) {
    let Some(host) = ctx.host() else {
        return;
    };
    let provider_message_id = event
        .payload_json
        .get("message_id")
        .or_else(|| event.payload_json.get("messageId"))
        .and_then(Value::as_str)
        .map(|s: &str| s.to_string())
        .or_else(|| runtime.assistant_partial_message_id.clone());
    let order_seq = read_order_seq(&event.payload_json).unwrap_or_else(|| {
        tracing::warn!(
            session_id = %ctx.session_id.0,
            run_id = %ctx.run_id.0,
            turn_id = %ctx.turn_id.0,
            "assistant_complete missing order_seq; assigning fallback message order"
        );
        0
    });
    let content: Option<String> = event
        .payload_json
        .get("full_content")
        .or_else(|| event.payload_json.get("content"))
        .and_then(Value::as_str)
        .map(|s: &str| s.to_string())
        .or_else(|| {
            if runtime.assistant_partial.is_empty() {
                None
            } else {
                Some(runtime.assistant_partial.clone())
            }
        });

    if let Some(content) = content {
        if let Some(content) = strip_emitted_prefix(&content, &runtime.assistant_emitted) {
            persist_assistant_complete_content(
                ctx,
                runtime,
                host.as_ref(),
                event,
                content,
                provider_message_id,
                order_seq,
            )
            .await;
        } else {
            runtime.assistant_partial.clear();
            runtime.assistant_partial_message_id = None;
        }
    }

    let _ = ctx
        .store
        .delete_session_events_for_turn_types(
            ctx.session_id,
            ctx.turn_id,
            &[
                SessionEventType::AssistantChunk,
                SessionEventType::ThoughtChunk,
            ],
        )
        .await;
}
