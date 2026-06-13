use std::time::Instant;

use ctx_core::ids::{MessageId, SessionId, TurnId};
use ctx_core::models::{Message, MessageAttachment, MessageDelivery, Session};
use ctx_session_message_service::message_admission::{
    post_user_message_record, MessageAdmissionError, PostUserMessageRecordInput,
};
use ctx_store::Store;

use super::command_dispatch;
use crate::daemon::scheduler::{QueuedMessage, SchedulerCommand};
use crate::daemon::SessionMessageCommandHandle;
use crate::daemon::SessionStoreAccessError;

pub struct PostUserMessageInput {
    pub message_id: MessageId,
    pub turn_id: TurnId,
    pub client_supplied_ids: bool,
    pub content: String,
    pub requested_delivery: Option<MessageDelivery>,
    pub attachments: Vec<MessageAttachment>,
    pub queued_messages_enabled: bool,
    pub run_id_header: Option<String>,
}

#[derive(Debug)]
pub enum PostUserMessageError {
    BadRequest(String),
    Conflict(String),
    NotFound(String),
    ServiceUnavailable(String),
    Internal(String),
}

impl SessionMessageCommandHandle {
    pub async fn delete_queued_session_message(
        &self,
        session_id: SessionId,
        message_id: MessageId,
    ) -> Result<(), command_dispatch::SessionSchedulerCommandError> {
        let store = self
            .existing_session_store_for_write(session_id)
            .await
            .map_err(session_store_command_error)?;
        let msg = store
            .get_message(message_id)
            .await
            .map_err(|_| command_dispatch::SessionSchedulerCommandError::StoreUnavailable)?
            .ok_or(command_dispatch::SessionSchedulerCommandError::NotFound)?;
        if msg.session_id != session_id {
            return Err(command_dispatch::SessionSchedulerCommandError::NotFound);
        }

        if !matches!(msg.delivery, MessageDelivery::Queued) || msg.delivered_at.is_some() {
            return Err(command_dispatch::SessionSchedulerCommandError::BadRequest);
        }
        store
            .delete_message(message_id)
            .await
            .map_err(|_| command_dispatch::SessionSchedulerCommandError::StoreUnavailable)?;
        if let Some(turn_id) = msg.turn_id {
            let _ = store.delete_session_turn(msg.session_id, turn_id).await;
        }
        let removed = store
            .append_session_event(
                msg.session_id,
                msg.run_id,
                msg.turn_id,
                ctx_core::models::SessionEventType::MessageQueueRemoved,
                serde_json::json!({
                    "message_id": msg.id.0,
                    "reason": "user_delete",
                }),
            )
            .await
            .map_err(|_| command_dispatch::SessionSchedulerCommandError::StoreUnavailable)?;
        self.publish_event(removed).await;

        if let Some(tx) = self.scheduler_sender(msg.session_id).await {
            let _ = tx.send(SchedulerCommand::RemoveQueued(message_id)).await;
        }
        Ok(())
    }

    pub async fn enqueue_user_message_for_scheduler(
        &self,
        store: &Store,
        session: Session,
        message: Message,
        run_id_header: Option<String>,
    ) {
        let tx = self.ensure_scheduler(session.clone()).await;
        let queued = QueuedMessage {
            message: message.clone(),
            enqueued_at: Instant::now(),
            run_id: run_id_header,
        };
        let _ = tx.send(SchedulerCommand::Enqueue(queued)).await;
        self.maybe_schedule_first_message_title_generation(store, session, &message)
            .await;
    }

    pub async fn post_user_message_for_request(
        &self,
        session_id: SessionId,
        input: PostUserMessageInput,
    ) -> Result<Message, PostUserMessageError> {
        let store = self
            .existing_session_store_for_write(session_id)
            .await
            .map_err(post_message_store_error)?;
        let session = store
            .get_session(session_id)
            .await
            .map_err(|_| PostUserMessageError::Internal("Failed to load session.".to_string()))?
            .ok_or_else(|| PostUserMessageError::NotFound("Session not found.".to_string()))?;
        self.remember_session_meta(&session).await;

        if let Some(reason) = self.post_message_update_drain_reason().await {
            return Err(PostUserMessageError::ServiceUnavailable(format!(
                "Daemon update is in progress; retry after the daemon restarts. ({})",
                reason
            )));
        }

        let order_seq_state = self.session_order_seq_state(&store, session.id).await;
        let admission = post_user_message_record(
            &store,
            &session,
            order_seq_state,
            self,
            &PostUserMessageRecordInput {
                message_id: input.message_id,
                turn_id: input.turn_id,
                client_supplied_ids: input.client_supplied_ids,
                content: input.content,
                requested_delivery: input.requested_delivery,
                attachments: input.attachments,
                queued_messages_enabled: input.queued_messages_enabled,
                session_running: self.is_session_running(session.id).await,
            },
        )
        .await
        .map_err(post_message_admission_error)?;

        for event in admission.appended_events {
            self.publish_event(event).await;
        }
        if admission.action.should_enqueue_scheduler() {
            self.enqueue_user_message_for_scheduler(
                &store,
                session,
                admission.message.clone(),
                input.run_id_header,
            )
            .await;
        }

        Ok(admission.message)
    }
}

fn post_message_store_error(error: SessionStoreAccessError) -> PostUserMessageError {
    match error {
        SessionStoreAccessError::NotFound => {
            PostUserMessageError::NotFound("Session not found.".to_string())
        }
        SessionStoreAccessError::LookupUnavailable(_)
        | SessionStoreAccessError::StoreUnavailable => {
            PostUserMessageError::Internal("workspace store unavailable".to_string())
        }
    }
}

fn session_store_command_error(
    error: SessionStoreAccessError,
) -> command_dispatch::SessionSchedulerCommandError {
    match error {
        SessionStoreAccessError::NotFound => {
            command_dispatch::SessionSchedulerCommandError::NotFound
        }
        SessionStoreAccessError::LookupUnavailable(_)
        | SessionStoreAccessError::StoreUnavailable => {
            command_dispatch::SessionSchedulerCommandError::StoreUnavailable
        }
    }
}

fn post_message_admission_error(error: MessageAdmissionError) -> PostUserMessageError {
    match error {
        MessageAdmissionError::BadRequest(error) => PostUserMessageError::BadRequest(error),
        MessageAdmissionError::Conflict(error) => PostUserMessageError::Conflict(error),
        MessageAdmissionError::Internal(error) => PostUserMessageError::Internal(error),
    }
}
