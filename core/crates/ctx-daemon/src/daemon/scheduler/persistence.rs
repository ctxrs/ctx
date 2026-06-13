use anyhow::{anyhow, Result};
use async_trait::async_trait;
use ctx_core::ids::{RunId, TurnId};
use ctx_core::models::{Message, MessageDelivery, MessageRole, SessionEvent, SessionEventType};
use ctx_store::Store;

use crate::daemon::DaemonState;

mod retry;

pub use retry::{is_transient_store_error, sleep_store_write_retry, STORE_WRITE_RETRY_LIMIT};

#[async_trait]
pub(in crate::daemon::scheduler) trait SchedulerPersistenceHost:
    Send + Sync
{
    async fn store_for_session(&self, session_id: ctx_core::ids::SessionId) -> Result<Store>;

    async fn publish_event(&self, event: SessionEvent);
}

#[async_trait]
impl SchedulerPersistenceHost for std::sync::Arc<DaemonState> {
    async fn store_for_session(&self, session_id: ctx_core::ids::SessionId) -> Result<Store> {
        self.as_ref().store_for_session(session_id).await
    }

    async fn publish_event(&self, event: SessionEvent) {
        self.session_publication.publish_event(event).await;
    }
}

fn maybe_fail_persist_assistant_message() -> Result<()> {
    if let Err(err) =
        crate::fault_injection::maybe_fail("ctx_http.persist_assistant_message.transient")
    {
        return Err(anyhow!("database is locked (fault injection): {err}"));
    }
    crate::fault_injection::maybe_fail("ctx_http.persist_assistant_message.fatal")
}

pub async fn append_session_event_with_retry(
    store: &ctx_store::Store,
    session_id: ctx_core::ids::SessionId,
    run_id: Option<RunId>,
    turn_id: Option<TurnId>,
    event_type: SessionEventType,
    payload_json: serde_json::Value,
) -> Result<SessionEvent> {
    let mut attempt = 0usize;
    loop {
        match store
            .append_session_event(
                session_id,
                run_id,
                turn_id,
                event_type.clone(),
                payload_json.clone(),
            )
            .await
        {
            Ok(event) => return Ok(event),
            Err(err) => {
                if !is_transient_store_error(&err) || attempt >= STORE_WRITE_RETRY_LIMIT {
                    return Err(err);
                }
                attempt += 1;
                sleep_store_write_retry(attempt).await;
            }
        }
    }
}

pub async fn emit_event_with_host<H>(
    host: &H,
    session_id: ctx_core::ids::SessionId,
    run_id: Option<RunId>,
    turn_id: Option<TurnId>,
    event_type: SessionEventType,
    payload_json: serde_json::Value,
) -> Result<SessionEvent>
where
    H: SchedulerPersistenceHost + ?Sized,
{
    let store = host.store_for_session(session_id).await?;
    let event = append_session_event_with_retry(
        &store,
        session_id,
        run_id,
        turn_id,
        event_type,
        payload_json,
    )
    .await?;
    host.publish_event(event.clone()).await;
    Ok(event)
}

#[allow(clippy::too_many_arguments)]
pub async fn persist_assistant_message(
    store: &ctx_store::Store,
    _workspace_id: ctx_core::ids::WorkspaceId,
    message_id: ctx_core::ids::MessageId,
    order_seq: i64,
    session_id: ctx_core::ids::SessionId,
    task_id: ctx_core::ids::TaskId,
    run_id: RunId,
    turn_id: TurnId,
    content: String,
    turn_sequence: i64,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Result<Message> {
    if content.is_empty() {
        return Err(anyhow!("assistant message content empty"));
    }
    let msg = Message {
        id: message_id,
        session_id,
        task_id,
        run_id: Some(run_id),
        turn_id: Some(turn_id),
        turn_sequence: Some(turn_sequence),
        order_seq: Some(order_seq),
        role: MessageRole::Assistant,
        content,
        attachments: vec![],
        delivery: MessageDelivery::Immediate,
        delivered_at: Some(created_at),
        created_at,
    };
    let mut attempt = 0usize;
    loop {
        if let Err(err) = maybe_fail_persist_assistant_message() {
            if !is_transient_store_error(&err) || attempt >= STORE_WRITE_RETRY_LIMIT {
                tracing::warn!("assistant message insert failed: {err:#}");
                return Err(err);
            }
            attempt += 1;
            sleep_store_write_retry(attempt).await;
            continue;
        }
        match store.insert_message(msg.clone()).await {
            Ok(saved) => return Ok(saved),
            Err(err) => {
                if !is_transient_store_error(&err) || attempt >= STORE_WRITE_RETRY_LIMIT {
                    tracing::warn!("assistant message insert failed: {err:#}");
                    return Err(err);
                }
                attempt += 1;
                sleep_store_write_retry(attempt).await;
            }
        }
    }
}
