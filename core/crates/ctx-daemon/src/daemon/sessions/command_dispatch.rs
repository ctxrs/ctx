use std::sync::Arc;
use std::time::Instant;

use ctx_core::ids::{MessageId, SessionId};
use ctx_core::models::{Message, MessageDelivery, Session, SessionEventType};
use ctx_store::Store;

use crate::daemon::scheduler::{QueuedMessage, SchedulerCommand};
use crate::daemon::sessions::title_generation::schedule_session_title_generation;
use crate::daemon::{DaemonState, SessionStoreAccessError};

#[derive(Debug)]
pub enum SessionSchedulerCommandError {
    BadRequest,
    NotFound,
    StoreUnavailable,
}

pub async fn delete_queued_session_message(
    state: &Arc<DaemonState>,
    session_id: SessionId,
    message_id: MessageId,
) -> Result<(), SessionSchedulerCommandError> {
    let store = state
        .existing_session_store_for_write(session_id)
        .await
        .map_err(session_store_error)?;
    let msg = store
        .get_message(message_id)
        .await
        .map_err(|_| SessionSchedulerCommandError::StoreUnavailable)?
        .ok_or(SessionSchedulerCommandError::NotFound)?;
    if msg.session_id != session_id {
        return Err(SessionSchedulerCommandError::NotFound);
    }

    if !matches!(msg.delivery, MessageDelivery::Queued) || msg.delivered_at.is_some() {
        return Err(SessionSchedulerCommandError::BadRequest);
    }
    store
        .delete_message(message_id)
        .await
        .map_err(|_| SessionSchedulerCommandError::StoreUnavailable)?;
    if let Some(turn_id) = msg.turn_id {
        let _ = store.delete_session_turn(msg.session_id, turn_id).await;
    }
    let removed = store
        .append_session_event(
            msg.session_id,
            msg.run_id,
            msg.turn_id,
            SessionEventType::MessageQueueRemoved,
            serde_json::json!({
                "message_id": msg.id.0,
                "reason": "user_delete",
            }),
        )
        .await
        .map_err(|_| SessionSchedulerCommandError::StoreUnavailable)?;
    state.session_publication.publish_event(removed).await;

    if let Some(tx) = state.sessions.scheduler_sender(msg.session_id).await {
        let _ = tx.send(SchedulerCommand::RemoveQueued(message_id)).await;
    }
    Ok(())
}

pub async fn enqueue_user_message_for_scheduler(
    state: &Arc<DaemonState>,
    store: &Store,
    session: Session,
    message: Message,
    run_id_header: Option<String>,
) {
    let tx = state
        .session_scheduler_worker_host
        .ensure_scheduler(&state.sessions, session.clone())
        .await;
    let queued = QueuedMessage {
        message: message.clone(),
        enqueued_at: Instant::now(),
        run_id: run_id_header,
    };
    let _ = tx.send(SchedulerCommand::Enqueue(queued)).await;

    if let Ok(count) = store.count_user_messages_for_session(session.id).await {
        if count == 1 {
            let _ =
                schedule_session_title_generation(state.clone(), session, message.content, false)
                    .await;
        }
    }
}

fn session_store_error(error: SessionStoreAccessError) -> SessionSchedulerCommandError {
    match error {
        SessionStoreAccessError::NotFound => SessionSchedulerCommandError::NotFound,
        SessionStoreAccessError::LookupUnavailable(_)
        | SessionStoreAccessError::StoreUnavailable => {
            SessionSchedulerCommandError::StoreUnavailable
        }
    }
}
