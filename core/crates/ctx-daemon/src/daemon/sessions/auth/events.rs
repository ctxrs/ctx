use ctx_core::ids::SessionId;
use ctx_core::models::SessionEventType;
use ctx_providers::events::NormalizedEvent;
use ctx_session_tools::order_seq::attach_order_seq;
use tokio::sync::mpsc;

use crate::daemon::session_control_effects::SessionAuthEventHost;

use super::SessionAuthError;

pub(in crate::daemon) fn spawn_session_auth_event_sink(
    host: SessionAuthEventHost,
    store: ctx_store::Store,
    session_id: SessionId,
) -> mpsc::Sender<NormalizedEvent> {
    let (event_sender, mut event_receiver) = mpsc::channel::<NormalizedEvent>(128);
    tokio::spawn(async move {
        while let Some(event) = event_receiver.recv().await {
            let mut event_type = event.event_type.clone();
            let mut payload = event.payload_json.clone();
            if matches!(event.event_type, SessionEventType::Init) {
                if payload.get("crp_session_id").is_some() {
                    host.emit_compat_payload_reject_counter(
                        "sessions.auth_event_init",
                        "crp_session_id",
                        None,
                    )
                    .await;
                }
                if let Some(provider_session_id) = payload
                    .get("provider_session_id")
                    .and_then(serde_json::Value::as_str)
                {
                    if let Err(err) = store
                        .claim_session_provider_session_ref(
                            session_id,
                            provider_session_id.to_string(),
                            "sessions.auth_event_init",
                        )
                        .await
                    {
                        event_type = SessionEventType::Notice;
                        payload = serde_json::json!({
                            "kind": "provider_session_ref_claim_failed",
                            "message": err.to_string(),
                            "reason": "provider_session_ref_claim_failed",
                            "details": {
                                "provider_session_id": provider_session_id,
                            },
                        });
                    }
                }
            }
            if payload.is_object() && should_attach_order_seq(&event) {
                let order_seq_state = host.session_order_seq_state(&store, session_id).await;
                let mut order_seq_state = order_seq_state.lock().await;
                attach_order_seq(
                    &mut order_seq_state,
                    &event.event_type,
                    &mut payload,
                    None,
                    0,
                );
            }
            if let Ok(appended_event) = store
                .append_session_event(session_id, None, None, event_type, payload)
                .await
            {
                host.publish_event(appended_event).await;
            }
        }
    });
    event_sender
}

fn should_attach_order_seq(event: &NormalizedEvent) -> bool {
    matches!(
        event.event_type,
        SessionEventType::UserMessage
            | SessionEventType::AssistantChunk
            | SessionEventType::AssistantComplete
            | SessionEventType::AssistantMessageInserted
            | SessionEventType::ThoughtChunk
            | SessionEventType::ToolCall
            | SessionEventType::ToolCallUpdate
            | SessionEventType::ToolResult
    ) || (matches!(event.event_type, SessionEventType::Notice)
        && event
            .payload_json
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| kind == "reasoning_summary" || kind == "ask_user_question"))
}

pub(in crate::daemon) async fn append_auth_notice(
    host: &SessionAuthEventHost,
    store: &ctx_store::Store,
    session_id: SessionId,
    payload: serde_json::Value,
) -> Result<(), SessionAuthError> {
    let event = store
        .append_session_event(session_id, None, None, SessionEventType::Notice, payload)
        .await
        .map_err(|_| SessionAuthError::Internal("failed to append auth event".to_string()))?;
    host.publish_event(event).await;
    Ok(())
}
