use std::sync::Arc;

use chrono::Utc;
use ctx_core::ids::MessageId;
use ctx_core::models::{Message, MessageDelivery, MessageRole, Session, SessionEventType};
use ctx_provider_runtime::provider_restart::ProviderRestartEvent;
use serde_json::json;

use crate::daemon::provider_capability_hosts::ProviderLifecycleBackgroundHost;

pub(super) async fn notify_sessions(
    host: &Arc<ProviderLifecycleBackgroundHost>,
    event: &ProviderRestartEvent,
) {
    let message_text = match event.kind {
        "provider_restart_warning" => {
            "Provider memory is high; restart scheduled if it stays elevated."
        }
        "provider_restart" => "Provider restart requested after sustained high memory usage.",
        _ => "Provider restart notice.",
    };
    let session_ids = host.sessions().list_running_sessions().await;
    for session_id in session_ids {
        let store = match host.store_for_session(session_id).await {
            Ok(store) => store,
            Err(_) => continue,
        };
        let session = store.get_session(session_id).await.ok().flatten();
        let Some(session) = session else {
            continue;
        };
        if session.provider_id != event.sample.provider_id {
            continue;
        }

        let message_id = insert_system_message(host, &store, &session, message_text).await;
        let payload = json!({
            "provider": event.sample.provider_id,
            "kind": event.kind,
            "stage": event.stage,
            "pid": event.sample.pid,
            "memory_mb": bytes_to_mb(event.sample.memory_bytes),
            "tool_memory_mb": bytes_to_mb(event.sample.tool_memory_bytes),
            "system_total_mb": bytes_to_mb(event.system.memory_total_bytes),
            "system_used_mb": bytes_to_mb(event.system.memory_used_bytes),
            "limit_high_mb": event.limits.memory_high_mb,
            "limit_max_mb": event.limits.memory_max_mb,
            "grace_period_ms": event.limits.grace_period.as_millis() as u64,
            "restart_at_ms": event.restart_at_ms,
            "message": message_text,
            "message_id": message_id.map(|id| id.0),
        });
        match store
            .append_session_event(session_id, None, None, SessionEventType::Notice, payload)
            .await
        {
            Ok(event) => host.publish_event(event).await,
            Err(err) => tracing::warn!(
                provider_id = %event.sample.provider_id,
                session_id = %session_id.0,
                "provider restart failed to append session event: {err:#}"
            ),
        }
    }
}

async fn insert_system_message(
    host: &ProviderLifecycleBackgroundHost,
    store: &ctx_store::Store,
    session: &Session,
    content: &str,
) -> Option<MessageId> {
    let now = Utc::now();
    let message_id = MessageId::new();
    let order_seq_state = host.sessions().get_order_seq_state(store, session.id).await;
    let order_seq = {
        let mut order_seq_state = order_seq_state.lock().await;
        order_seq_state.get_or_assign(format!("message:{}", message_id.0), None)
    };
    let msg = Message {
        id: message_id,
        session_id: session.id,
        task_id: session.task_id,
        run_id: None,
        turn_id: None,
        turn_sequence: None,
        order_seq: Some(order_seq),
        role: MessageRole::System,
        content: content.to_string(),
        attachments: vec![],
        delivery: MessageDelivery::Immediate,
        delivered_at: Some(now),
        created_at: now,
    };
    match store.insert_message(msg).await {
        Ok(saved) => Some(saved.id),
        Err(err) => {
            tracing::warn!(
                session_id = %session.id.0,
                "provider restart failed to insert system message: {err:#}"
            );
            None
        }
    }
}

fn bytes_to_mb(value: u64) -> u64 {
    value / (1024 * 1024)
}
