use std::collections::HashMap;
use std::time::Instant;

use ctx_core::ids::{MessageId, RunId, SessionId, TurnId};
use ctx_core::models::{
    Message, MessageDelivery, MessageRole, Session, SessionEvent, SessionEventType, SessionTurn,
    SessionTurnStatus,
};
use ctx_observability::perf_telemetry::{PerfMetric, PerfMetricKind};
use ctx_store::Store;

use super::super::super::errors::ApiResult;
use super::super::super::{internal_api_error, PersistedSubagentPrompt};
use super::SubagentSpawnHost;
use crate::daemon::session_store_access_anyhow;

impl SubagentSpawnHost {
    pub(in crate::daemon) async fn persist_subagent_prompt(
        &self,
        session: &Session,
        prompt: String,
    ) -> ApiResult<PersistedSubagentPrompt> {
        let store = self
            .session_stores
            .existing_session_store_for_write(session.id)
            .await
            .map_err(|error| internal_api_error(session_store_access_anyhow(error)))?;
        let run_id = RunId::new();
        let turn_id = TurnId::new();
        let message_id = MessageId::new();
        let order_seq_state = self
            .session_runtime
            .get_order_seq_state(&store, session.id)
            .await;
        let order_seq = {
            let mut order_seq_state = order_seq_state.lock().await;
            order_seq_state.get_or_assign(format!("message:{}", message_id.0), None)
        };
        let has_backlog = self.session_runtime.is_running(session.id).await
            || !store
                .list_queued_messages_for_session(session.id)
                .await
                .map_err(internal_api_error)?
                .is_empty()
            || store
                .get_latest_turn_for_session(session.id)
                .await
                .map_err(internal_api_error)?
                .as_ref()
                .is_some_and(|turn| ctx_subagent_service::is_active_turn_status(&turn.status));
        let delivery = if has_backlog {
            MessageDelivery::Queued
        } else {
            MessageDelivery::Immediate
        };
        let msg = Message {
            id: message_id,
            session_id: session.id,
            task_id: session.task_id,
            run_id: Some(run_id),
            turn_id: Some(turn_id),
            turn_sequence: Some(0),
            order_seq: Some(order_seq),
            role: MessageRole::User,
            content: prompt,
            attachments: vec![],
            delivery,
            delivered_at: None,
            created_at: chrono::Utc::now(),
        };
        let saved = store
            .insert_message(msg)
            .await
            .map_err(internal_api_error)?;
        let event = store
            .append_session_event(
                session.id,
                Some(run_id),
                Some(turn_id),
                SessionEventType::UserMessage,
                serde_json::json!({
                    "message_id": saved.id.0,
                    "content": saved.content.clone(),
                    "delivery": saved.delivery.clone(),
                    "attachments": saved.attachments,
                    "order_seq": order_seq,
                }),
            )
            .await
            .map_err(internal_api_error)?;
        let start_seq = event.seq;
        let mut last_event_seq = start_seq;

        let turn = SessionTurn {
            turn_id,
            session_id: session.id,
            run_id: Some(run_id),
            user_message_id: Some(saved.id),
            status: match saved.delivery {
                MessageDelivery::Queued => SessionTurnStatus::Queued,
                MessageDelivery::Immediate => SessionTurnStatus::Starting,
            },
            start_seq: Some(start_seq),
            end_seq: None,
            started_at: saved.created_at,
            updated_at: saved.created_at,
            assistant_partial: None,
            thought_partial: None,
            metrics_json: None,
            failure: None,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
        };
        let _ = store.insert_session_turn(turn).await;
        self.publish_event(event).await;
        if matches!(saved.delivery, MessageDelivery::Queued) {
            last_event_seq = self
                .append_and_publish_queued_prompt_events(&store, session, run_id, turn_id, &saved)
                .await?;
        }

        Ok(PersistedSubagentPrompt {
            run_id,
            saved_message: saved,
            last_event_seq,
        })
    }

    pub(in crate::daemon) async fn emit_subagent_invocation_notice(
        &self,
        parent_session_id: SessionId,
        parent_turn_id: Option<TurnId>,
        payload: serde_json::Value,
    ) -> ApiResult<()> {
        super::super::super::emit_subagent_invocation_notice(
            &self.child_run_host,
            parent_session_id,
            parent_turn_id,
            payload,
        )
        .await
    }

    pub(in crate::daemon) async fn dispatch_subagent_prompt(
        &self,
        session: &Session,
        saved: &Message,
    ) {
        let tx = self
            .scheduler_spawner
            .ensure_scheduler(&self.session_runtime, session.clone())
            .await;
        let queued = crate::daemon::scheduler::QueuedMessage {
            message: saved.clone(),
            enqueued_at: Instant::now(),
            run_id: None,
        };
        let _ = tx
            .send(crate::daemon::scheduler::SchedulerCommand::Enqueue(queued))
            .await;
    }

    async fn append_and_publish_queued_prompt_events(
        &self,
        store: &Store,
        session: &Session,
        run_id: RunId,
        turn_id: TurnId,
        saved: &Message,
    ) -> ApiResult<i64> {
        let queued = store
            .append_session_event(
                session.id,
                Some(run_id),
                Some(turn_id),
                SessionEventType::InputQueued,
                serde_json::json!({"message_id": saved.id.0}),
            )
            .await
            .map_err(internal_api_error)?;
        self.publish_event(queued).await;

        let queue_position = store
            .list_queued_messages_for_session(session.id)
            .await
            .ok()
            .and_then(|messages| {
                messages
                    .iter()
                    .position(|message| message.id == saved.id)
                    .map(|idx| idx as i64)
            });

        let queue_added = store
            .append_session_event(
                session.id,
                Some(run_id),
                Some(turn_id),
                SessionEventType::MessageQueueAdded,
                serde_json::json!({
                    "message_id": saved.id.0,
                    "queue_position": queue_position,
                }),
            )
            .await
            .map_err(internal_api_error)?;
        self.publish_event(queue_added).await;

        let turn_queued = store
            .append_session_event(
                session.id,
                Some(run_id),
                Some(turn_id),
                SessionEventType::TurnQueued,
                serde_json::json!({
                    "message_id": saved.id.0,
                    "queue_position": queue_position,
                }),
            )
            .await
            .map_err(internal_api_error)?;
        let last_event_seq = turn_queued.seq;
        self.publish_event(turn_queued).await;
        Ok(last_event_seq)
    }

    async fn publish_event(&self, event: SessionEvent) {
        self.session_runtime
            .publish_event_with_host(&self.publish_host, event)
            .await;
    }

    async fn emit_counter_metric(&self, name: &str, labels: HashMap<String, String>) {
        let metric = PerfMetric {
            name: name.to_string(),
            kind: PerfMetricKind::Counter,
            unit: "count".to_string(),
            value: 1.0,
            labels,
        };
        self.perf_telemetry
            .record_metric(metric, None, None, None)
            .await;
    }

    pub(in crate::daemon) async fn emit_compat_payload_reject_counter(
        &self,
        surface: &str,
        issue: &str,
        extra_label: Option<(&str, &str)>,
    ) {
        let mut labels = HashMap::new();
        labels.insert("source".to_string(), "daemon".to_string());
        labels.insert("surface".to_string(), surface.to_string());
        labels.insert("issue".to_string(), issue.to_string());
        if let Some((key, value)) = extra_label {
            labels.insert(key.to_string(), value.to_string());
        }
        self.emit_counter_metric("compat.payload_reject_count", labels)
            .await;
    }

    pub(in crate::daemon) async fn emit_product_fallback_applied_counter(
        &self,
        surface: &str,
        fallback: &str,
        extra_label: Option<(&str, &str)>,
    ) {
        let mut labels = HashMap::new();
        labels.insert("source".to_string(), "daemon".to_string());
        labels.insert("surface".to_string(), surface.to_string());
        labels.insert("fallback".to_string(), fallback.to_string());
        if let Some((key, value)) = extra_label {
            labels.insert(key.to_string(), value.to_string());
        }
        self.emit_counter_metric("product.fallback_applied_count", labels)
            .await;
    }
}
