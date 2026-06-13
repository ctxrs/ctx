use ctx_core::ids::MessageId;
use ctx_core::models::{SessionEvent, SessionEventType};
use ctx_session_tools::order_seq::attach_order_seq;
use ctx_storage_admission::{is_storage_exhaustion_error, storage_exhaustion_message};
use serde_json::json;

use crate::daemon::scheduler::host::TurnEventLoopHost;
use crate::daemon::scheduler::persistence::{emit_event_with_host, persist_assistant_message};

use super::super::failure::{fail_turn, TurnFailurePayload};
use super::super::state::EventLoopRuntimeState;
use super::super::TurnEventLoop;

pub(super) async fn persist_assistant_complete_content(
    ctx: &TurnEventLoop,
    runtime: &mut EventLoopRuntimeState,
    host: &TurnEventLoopHost,
    event: &SessionEvent,
    content: String,
    provider_message_id: Option<String>,
    order_seq: i64,
) {
    let assistant_message_id = MessageId::new();
    let persisted_order_seq = if order_seq > 0 {
        order_seq
    } else {
        let mut order_seq_state = ctx.order_seq_state.lock().await;
        order_seq_state.get_or_assign(format!("message:{}", assistant_message_id.0), None)
    };
    match persist_assistant_message(
        &ctx.store,
        ctx.workspace_id,
        assistant_message_id,
        persisted_order_seq,
        ctx.session_id,
        ctx.task_id,
        ctx.run_id,
        ctx.turn_id,
        content,
        runtime.assistant_sequence + 1,
        event.created_at,
    )
    .await
    {
        Ok(saved) => {
            runtime.assistant_sequence += 1;
            runtime.assistant_emitted.push_str(&saved.content);
            runtime.assistant_partial.clear();
            let mut payload = json!({
                "message_id": saved.id.0,
                "content": saved.content,
                "delivery": saved.delivery,
                "attachments": saved.attachments,
                "turn_sequence": saved.turn_sequence,
                "order_seq": saved.order_seq,
            });
            if let Some(provider_message_id) = provider_message_id {
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert(
                        "provider_message_id".to_string(),
                        json!(provider_message_id),
                    );
                }
            }
            {
                let mut order_seq_state = ctx.order_seq_state.lock().await;
                attach_order_seq(
                    &mut order_seq_state,
                    &SessionEventType::AssistantMessageInserted,
                    &mut payload,
                    Some(&ctx.turn_id),
                    runtime.assistant_sequence,
                );
            }
            let _ = emit_event_with_host(
                host,
                ctx.session_id,
                Some(ctx.run_id),
                Some(ctx.turn_id),
                SessionEventType::AssistantMessageInserted,
                payload,
            )
            .await;
            runtime.assistant_partial_message_id = None;
        }
        Err(err) => {
            let err_string = format!("{err:#}");
            let is_storage_exhausted = is_storage_exhaustion_error(&err_string);
            let details = Some(json!({
                "provider_message_id": provider_message_id,
                "order_seq": persisted_order_seq,
                "turn_sequence": runtime.assistant_sequence + 1,
                "root_cause": err_string,
            }));
            let storage_status = host.storage_guard_snapshot();
            fail_turn(
                ctx,
                runtime,
                TurnFailurePayload {
                    error_message: if is_storage_exhausted {
                        storage_exhaustion_message(storage_status.active.as_ref())
                    } else {
                        format!("failed to persist assistant message: {err:#}")
                    },
                    details,
                    kind: Some(json!(if is_storage_exhausted {
                        "storage_exhausted"
                    } else {
                        "assistant_message_persist_failed"
                    })),
                },
            )
            .await;
            runtime.assistant_partial.clear();
            runtime.assistant_partial_message_id = None;
        }
    }
}
