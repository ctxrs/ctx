use std::sync::Arc;

use async_trait::async_trait;
use ctx_core::ids::{MessageId, RunId, SessionId, TurnId};
use ctx_core::models::{
    Message, MessageAttachment, MessageDelivery, MessageRole, Session, SessionEvent,
    SessionEventType, SessionTurn,
};
use ctx_session_tools::order_seq::OrderSeqState;
use ctx_store::{is_unique_constraint_violation, Store};
use tokio::sync::Mutex;

use crate::message_delivery::{
    build_user_message_turn, delivery_matches, resolve_message_delivery,
    MessageDeliveryResolutionError,
};

#[derive(Debug, Clone)]
pub struct PostUserMessageRecordInput {
    pub message_id: MessageId,
    pub turn_id: TurnId,
    pub client_supplied_ids: bool,
    pub content: String,
    pub requested_delivery: Option<MessageDelivery>,
    pub attachments: Vec<MessageAttachment>,
    pub queued_messages_enabled: bool,
    pub session_running: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostUserMessageAdmissionAction {
    AcceptedForDispatch,
    Replayed,
}

impl PostUserMessageAdmissionAction {
    pub fn should_enqueue_scheduler(self) -> bool {
        matches!(self, Self::AcceptedForDispatch)
    }
}

#[derive(Debug, Clone)]
pub struct PostUserMessageAdmission {
    pub message: Message,
    pub action: PostUserMessageAdmissionAction,
    pub appended_events: Vec<SessionEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageAdmissionError {
    BadRequest(String),
    Conflict(String),
    Internal(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageAttachmentSignatureError {
    BadRequest(String),
    Internal(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageAttachmentSignature {
    pub mime_type: String,
    pub name: Option<String>,
    pub sha256: String,
}

#[async_trait]
pub trait MessageAttachmentSignatureResolver: Sync {
    async fn message_attachment_signature(
        &self,
        attachment: &MessageAttachment,
    ) -> Result<MessageAttachmentSignature, MessageAttachmentSignatureError>;
}

pub async fn post_user_message_record<R>(
    store: &Store,
    session: &Session,
    order_seq_state: Arc<Mutex<OrderSeqState>>,
    attachment_resolver: &R,
    input: &PostUserMessageRecordInput,
) -> Result<PostUserMessageAdmission, MessageAdmissionError>
where
    R: MessageAttachmentSignatureResolver,
{
    if let Some(existing) =
        load_matching_existing_user_message(store, session.id, attachment_resolver, input).await?
    {
        return Ok(PostUserMessageAdmission {
            message: existing,
            action: PostUserMessageAdmissionAction::Replayed,
            appended_events: Vec::new(),
        });
    }

    let persisted =
        persist_user_message_record(store, session, order_seq_state, attachment_resolver, input)
            .await?;
    let appended_events = append_user_message_events(store, session.id, &persisted).await?;

    Ok(PostUserMessageAdmission {
        message: persisted.saved,
        action: PostUserMessageAdmissionAction::AcceptedForDispatch,
        appended_events,
    })
}

async fn load_matching_existing_user_message<R>(
    store: &Store,
    session_id: SessionId,
    attachment_resolver: &R,
    input: &PostUserMessageRecordInput,
) -> Result<Option<Message>, MessageAdmissionError>
where
    R: MessageAttachmentSignatureResolver,
{
    if !input.client_supplied_ids {
        return Ok(None);
    }
    let Some(existing) = store
        .get_message(input.message_id)
        .await
        .map_err(|_| MessageAdmissionError::Internal("Failed to load message.".to_string()))?
    else {
        return Ok(None);
    };
    if message_matches_post_input(&existing, session_id, attachment_resolver, input).await? {
        ensure_existing_message_turn(store, session_id, input.turn_id, &existing).await?;
        return Ok(Some(existing));
    }
    Err(MessageAdmissionError::Conflict(
        "A different message already exists for that client id.".to_string(),
    ))
}

async fn persist_user_message_record<R>(
    store: &Store,
    session: &Session,
    order_seq_state: Arc<Mutex<OrderSeqState>>,
    attachment_resolver: &R,
    input: &PostUserMessageRecordInput,
) -> Result<PersistedPostUserMessage, MessageAdmissionError>
where
    R: MessageAttachmentSignatureResolver,
{
    let delivery = resolve_message_delivery(
        input.requested_delivery.clone(),
        input.session_running,
        input.queued_messages_enabled,
    )
    .map_err(post_message_delivery_error)?;

    let run_id = RunId::new();
    let order_seq = {
        let mut order_seq_state = order_seq_state.lock().await;
        order_seq_state.get_or_assign(format!("message:{}", input.message_id.0), None)
    };
    let msg = Message {
        id: input.message_id,
        session_id: session.id,
        task_id: session.task_id,
        run_id: Some(run_id),
        turn_id: Some(input.turn_id),
        turn_sequence: Some(0),
        order_seq: Some(order_seq),
        role: MessageRole::User,
        content: input.content.clone(),
        attachments: input.attachments.clone(),
        delivery,
        delivered_at: None,
        created_at: chrono::Utc::now(),
    };
    let saved = match store.insert_message(msg).await {
        Ok(saved) => saved,
        Err(err) if input.client_supplied_ids && is_unique_constraint_violation(&err) => {
            load_conflicting_existing_user_message(store, session.id, attachment_resolver, input)
                .await?
        }
        Err(_) => {
            return Err(MessageAdmissionError::Internal(
                "Failed to save message.".to_string(),
            ));
        }
    };
    Ok(PersistedPostUserMessage {
        saved,
        run_id,
        turn_id: input.turn_id,
        order_seq,
    })
}

async fn load_conflicting_existing_user_message<R>(
    store: &Store,
    session_id: SessionId,
    attachment_resolver: &R,
    input: &PostUserMessageRecordInput,
) -> Result<Message, MessageAdmissionError>
where
    R: MessageAttachmentSignatureResolver,
{
    let Some(existing) = store
        .get_message(input.message_id)
        .await
        .map_err(|_| MessageAdmissionError::Internal("Failed to load message.".to_string()))?
    else {
        return Err(MessageAdmissionError::Internal(
            "Message already existed but could not be loaded.".to_string(),
        ));
    };
    if message_matches_post_input(&existing, session_id, attachment_resolver, input).await? {
        Ok(existing)
    } else {
        Err(MessageAdmissionError::Conflict(
            "A different message already exists for that client id.".to_string(),
        ))
    }
}

async fn message_matches_post_input<R>(
    existing: &Message,
    session_id: SessionId,
    attachment_resolver: &R,
    input: &PostUserMessageRecordInput,
) -> Result<bool, MessageAdmissionError>
where
    R: MessageAttachmentSignatureResolver,
{
    Ok(
        message_matches_post_input_except_attachments(existing, session_id, input)
            && message_attachments_match(
                attachment_resolver,
                &existing.attachments,
                &input.attachments,
            )
            .await?,
    )
}

fn message_matches_post_input_except_attachments(
    existing: &Message,
    session_id: SessionId,
    input: &PostUserMessageRecordInput,
) -> bool {
    existing.session_id == session_id
        && existing.turn_id == Some(input.turn_id)
        && matches!(existing.role, MessageRole::User)
        && existing.content == input.content
        && match input.requested_delivery.as_ref() {
            Some(requested_delivery) => delivery_matches(&existing.delivery, requested_delivery),
            None => true,
        }
}

async fn message_attachments_match<R>(
    attachment_resolver: &R,
    existing: &[MessageAttachment],
    requested: &[MessageAttachment],
) -> Result<bool, MessageAdmissionError>
where
    R: MessageAttachmentSignatureResolver,
{
    if existing.len() != requested.len() {
        return Ok(false);
    }
    let mut existing_sig = Vec::with_capacity(existing.len());
    for attachment in existing {
        existing_sig.push(
            attachment_resolver
                .message_attachment_signature(attachment)
                .await?,
        );
    }
    let mut requested_sig = Vec::with_capacity(requested.len());
    for attachment in requested {
        requested_sig.push(
            attachment_resolver
                .message_attachment_signature(attachment)
                .await?,
        );
    }
    Ok(existing_sig == requested_sig)
}

async fn append_user_message_events(
    store: &Store,
    session_id: SessionId,
    persisted: &PersistedPostUserMessage,
) -> Result<Vec<SessionEvent>, MessageAdmissionError> {
    let saved = &persisted.saved;
    let event = store
        .append_session_event(
            session_id,
            Some(persisted.run_id),
            Some(persisted.turn_id),
            SessionEventType::UserMessage,
            serde_json::json!({
                "message_id": saved.id.0,
                "content": saved.content.clone(),
                "delivery": saved.delivery.clone(),
                "attachments": saved.attachments,
                "order_seq": persisted.order_seq,
            }),
        )
        .await
        .map_err(|_| {
            MessageAdmissionError::Internal("Failed to append session event.".to_string())
        })?;
    let start_seq = event.seq;
    let mut appended = vec![event];

    let turn = build_user_message_turn(saved, persisted.turn_id, Some(start_seq));
    ensure_session_turn(store, session_id, persisted.turn_id, saved, turn).await?;

    if matches!(saved.delivery, MessageDelivery::Queued) {
        append_queue_events(store, session_id, persisted, &mut appended).await?;
    }

    Ok(appended)
}

async fn ensure_existing_message_turn(
    store: &Store,
    session_id: SessionId,
    turn_id: TurnId,
    existing: &Message,
) -> Result<(), MessageAdmissionError> {
    let turn = build_user_message_turn(existing, turn_id, None);
    ensure_session_turn(store, session_id, turn_id, existing, turn).await
}

async fn ensure_session_turn(
    store: &Store,
    session_id: SessionId,
    turn_id: TurnId,
    message: &Message,
    turn: SessionTurn,
) -> Result<(), MessageAdmissionError> {
    let existing_turn = store.get_session_turn_by_id(turn_id).await.map_err(|_| {
        MessageAdmissionError::Internal("Failed to inspect session turn.".to_string())
    })?;
    if let Some(existing) = existing_turn {
        let matches =
            existing.session_id == session_id && existing.user_message_id == Some(message.id);
        if !matches {
            return Err(MessageAdmissionError::Conflict(
                "Turn id already belongs to another message.".to_string(),
            ));
        }
        return Ok(());
    }

    if let Err(err) = store.insert_session_turn(turn).await {
        if !is_unique_constraint_violation(&err) {
            return Err(MessageAdmissionError::Internal(
                "Failed to create session turn.".to_string(),
            ));
        }
        let existing = store.get_session_turn_by_id(turn_id).await.map_err(|_| {
            MessageAdmissionError::Internal("Failed to inspect session turn.".to_string())
        })?;
        if let Some(existing) = existing {
            let matches =
                existing.session_id == session_id && existing.user_message_id == Some(message.id);
            if !matches {
                return Err(MessageAdmissionError::Conflict(
                    "Turn id already belongs to another message.".to_string(),
                ));
            }
        } else {
            return Err(MessageAdmissionError::Internal(
                "Session turn insert succeeded but could not be reloaded.".to_string(),
            ));
        }
    }
    Ok(())
}

async fn append_queue_events(
    store: &Store,
    session_id: SessionId,
    persisted: &PersistedPostUserMessage,
    appended: &mut Vec<SessionEvent>,
) -> Result<(), MessageAdmissionError> {
    let saved = &persisted.saved;
    let queued = store
        .append_session_event(
            session_id,
            Some(persisted.run_id),
            Some(persisted.turn_id),
            SessionEventType::InputQueued,
            serde_json::json!({"message_id": saved.id.0}),
        )
        .await
        .map_err(|_| {
            MessageAdmissionError::Internal("Failed to append queued-input event.".to_string())
        })?;
    appended.push(queued);

    let queue_position = store
        .list_queued_messages_for_session(session_id)
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
            session_id,
            Some(persisted.run_id),
            Some(persisted.turn_id),
            SessionEventType::MessageQueueAdded,
            serde_json::json!({
                "message_id": saved.id.0,
                "queue_position": queue_position,
            }),
        )
        .await
        .map_err(|_| {
            MessageAdmissionError::Internal("Failed to append queue event.".to_string())
        })?;
    appended.push(queue_added);

    let turn_queued = store
        .append_session_event(
            session_id,
            Some(persisted.run_id),
            Some(persisted.turn_id),
            SessionEventType::TurnQueued,
            serde_json::json!({
                "message_id": saved.id.0,
                "queue_position": queue_position,
            }),
        )
        .await
        .map_err(|_| {
            MessageAdmissionError::Internal("Failed to append queued turn event.".to_string())
        })?;
    appended.push(turn_queued);

    Ok(())
}

impl From<MessageAttachmentSignatureError> for MessageAdmissionError {
    fn from(error: MessageAttachmentSignatureError) -> Self {
        match error {
            MessageAttachmentSignatureError::BadRequest(message) => Self::BadRequest(message),
            MessageAttachmentSignatureError::Internal(message) => Self::Internal(message),
        }
    }
}

struct PersistedPostUserMessage {
    saved: Message,
    run_id: RunId,
    turn_id: TurnId,
    order_seq: i64,
}

fn post_message_delivery_error(error: MessageDeliveryResolutionError) -> MessageAdmissionError {
    match error {
        MessageDeliveryResolutionError::QueuedMessagesDisabled => {
            MessageAdmissionError::BadRequest(error.message().to_string())
        }
        MessageDeliveryResolutionError::TurnAlreadyRunning => {
            MessageAdmissionError::Conflict(error.message().to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ctx_core::models::{ExecutionEnvironment, SessionTurnStatus, VcsKind};

    use super::*;

    async fn setup_store() -> (tempfile::TempDir, Store, Session, Arc<Mutex<OrderSeqState>>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("db.sqlite");
        let store = Store::open(&db_path).await.expect("open store");
        let workspace = store
            .create_workspace("workspace".into(), "/tmp/workspace".into(), VcsKind::Git)
            .await
            .expect("workspace");
        let task = store
            .create_task(workspace.id, "task".into(), None)
            .await
            .expect("task");
        let worktree = store
            .create_worktree(workspace.id, "/tmp/worktree".into(), "abc123".into(), None)
            .await
            .expect("worktree");
        let session = store
            .create_session(
                task.id,
                workspace.id,
                worktree.id,
                ExecutionEnvironment::Host,
                "fake".into(),
                "model".into(),
                "implementer".into(),
                None,
                None,
                None,
            )
            .await
            .expect("session");
        (
            dir,
            store,
            session,
            Arc::new(Mutex::new(OrderSeqState::new(1))),
        )
    }

    fn input(message_id: MessageId, turn_id: TurnId, content: &str) -> PostUserMessageRecordInput {
        PostUserMessageRecordInput {
            message_id,
            turn_id,
            client_supplied_ids: true,
            content: content.to_string(),
            requested_delivery: Some(MessageDelivery::Immediate),
            attachments: Vec::new(),
            queued_messages_enabled: false,
            session_running: false,
        }
    }

    #[derive(Default)]
    struct FakeAttachmentResolver {
        signatures:
            HashMap<String, Result<MessageAttachmentSignature, MessageAttachmentSignatureError>>,
    }

    impl FakeAttachmentResolver {
        fn with_signature(mut self, blob_id: &str, signature: MessageAttachmentSignature) -> Self {
            self.signatures.insert(blob_id.to_string(), Ok(signature));
            self
        }

        fn with_error(mut self, blob_id: &str, error: MessageAttachmentSignatureError) -> Self {
            self.signatures.insert(blob_id.to_string(), Err(error));
            self
        }
    }

    #[async_trait]
    impl MessageAttachmentSignatureResolver for FakeAttachmentResolver {
        async fn message_attachment_signature(
            &self,
            attachment: &MessageAttachment,
        ) -> Result<MessageAttachmentSignature, MessageAttachmentSignatureError> {
            match attachment {
                MessageAttachment::ImageRef { blob_id, .. } => {
                    self.signatures.get(blob_id).cloned().unwrap_or_else(|| {
                        Err(MessageAttachmentSignatureError::BadRequest(
                            "Image attachment blob was not found.".to_string(),
                        ))
                    })
                }
                MessageAttachment::Image {
                    mime_type, name, ..
                } => Ok(MessageAttachmentSignature {
                    mime_type: mime_type.clone(),
                    name: name.clone(),
                    sha256: "inline".to_string(),
                }),
            }
        }
    }

    #[tokio::test]
    async fn idempotent_replay_returns_existing_message_without_events_or_dispatch() {
        let (_dir, store, session, order_seq_state) = setup_store().await;
        let resolver = FakeAttachmentResolver::default();
        let message_id = MessageId::new();
        let turn_id = TurnId::new();
        let input = input(message_id, turn_id, "hello");

        let created =
            post_user_message_record(&store, &session, order_seq_state.clone(), &resolver, &input)
                .await
                .expect("create message");
        assert_eq!(
            created.action,
            PostUserMessageAdmissionAction::AcceptedForDispatch
        );
        assert!(created.action.should_enqueue_scheduler());
        assert_eq!(created.appended_events.len(), 1);

        let replayed =
            post_user_message_record(&store, &session, order_seq_state, &resolver, &input)
                .await
                .expect("replay message");
        assert_eq!(replayed.message.id, message_id);
        assert_eq!(replayed.action, PostUserMessageAdmissionAction::Replayed);
        assert!(!replayed.action.should_enqueue_scheduler());
        assert!(replayed.appended_events.is_empty());
    }

    #[tokio::test]
    async fn client_id_conflict_rejects_changed_payload() {
        let (_dir, store, session, order_seq_state) = setup_store().await;
        let resolver = FakeAttachmentResolver::default();
        let message_id = MessageId::new();
        let turn_id = TurnId::new();
        let first = input(message_id, turn_id, "hello");
        post_user_message_record(&store, &session, order_seq_state.clone(), &resolver, &first)
            .await
            .expect("create message");

        let changed = input(message_id, turn_id, "changed");
        let error =
            post_user_message_record(&store, &session, order_seq_state, &resolver, &changed)
                .await
                .expect_err("changed payload should conflict");
        assert!(
            matches!(error, MessageAdmissionError::Conflict(message) if message.contains("different message"))
        );
    }

    #[tokio::test]
    async fn turn_id_conflict_is_rejected() {
        let (_dir, store, session, order_seq_state) = setup_store().await;
        let resolver = FakeAttachmentResolver::default();
        let message_id = MessageId::new();
        let turn_id = TurnId::new();
        store
            .insert_session_turn(SessionTurn {
                turn_id,
                session_id: session.id,
                run_id: Some(RunId::new()),
                user_message_id: Some(MessageId::new()),
                status: SessionTurnStatus::Starting,
                start_seq: None,
                end_seq: None,
                started_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                assistant_partial: None,
                thought_partial: None,
                metrics_json: None,
                failure: None,
                tool_total: 0,
                tool_pending: 0,
                tool_running: 0,
                tool_completed: 0,
                tool_failed: 0,
            })
            .await
            .expect("insert conflicting turn");

        let input = input(message_id, turn_id, "hello");
        let error = post_user_message_record(&store, &session, order_seq_state, &resolver, &input)
            .await
            .expect_err("turn conflict");
        assert!(
            matches!(error, MessageAdmissionError::Conflict(message) if message.contains("Turn id"))
        );
    }

    #[tokio::test]
    async fn queued_messages_append_full_queue_event_sequence() {
        let (_dir, store, session, order_seq_state) = setup_store().await;
        let resolver = FakeAttachmentResolver::default();
        let mut input = input(MessageId::new(), TurnId::new(), "queued");
        input.requested_delivery = None;
        input.queued_messages_enabled = true;
        input.session_running = true;

        let outcome =
            post_user_message_record(&store, &session, order_seq_state, &resolver, &input)
                .await
                .expect("queue message");
        assert!(matches!(outcome.message.delivery, MessageDelivery::Queued));
        let event_types: Vec<_> = outcome
            .appended_events
            .iter()
            .map(|event| event.event_type.clone())
            .collect();
        assert!(matches!(
            event_types.as_slice(),
            [
                SessionEventType::UserMessage,
                SessionEventType::InputQueued,
                SessionEventType::MessageQueueAdded,
                SessionEventType::TurnQueued,
            ]
        ));
        assert_eq!(
            outcome.appended_events[2].payload_json["queue_position"],
            serde_json::json!(0)
        );
        assert_eq!(
            outcome.appended_events[3].payload_json["queue_position"],
            serde_json::json!(0)
        );
    }

    #[tokio::test]
    async fn attachment_signatures_allow_equivalent_idempotent_replay() {
        let (_dir, store, session, order_seq_state) = setup_store().await;
        let signature = MessageAttachmentSignature {
            mime_type: "image/png".to_string(),
            name: Some("screenshot.png".to_string()),
            sha256: "abc".to_string(),
        };
        let resolver = FakeAttachmentResolver::default()
            .with_signature("blob-a", signature.clone())
            .with_signature("blob-b", signature);
        let message_id = MessageId::new();
        let turn_id = TurnId::new();
        let mut first = input(message_id, turn_id, "image");
        first.attachments = vec![MessageAttachment::ImageRef {
            blob_id: "blob-a".to_string(),
            mime_type: "image/png".to_string(),
            name: Some("screenshot.png".to_string()),
        }];
        post_user_message_record(&store, &session, order_seq_state.clone(), &resolver, &first)
            .await
            .expect("create message");

        let mut replay = input(message_id, turn_id, "image");
        replay.attachments = vec![MessageAttachment::ImageRef {
            blob_id: "blob-b".to_string(),
            mime_type: "image/png".to_string(),
            name: Some("screenshot.png".to_string()),
        }];
        let outcome =
            post_user_message_record(&store, &session, order_seq_state, &resolver, &replay)
                .await
                .expect("equivalent attachment replay");
        assert_eq!(outcome.action, PostUserMessageAdmissionAction::Replayed);
        assert!(outcome.appended_events.is_empty());
    }

    #[tokio::test]
    async fn attachment_signature_mismatch_rejects_replay() {
        let (_dir, store, session, order_seq_state) = setup_store().await;
        let resolver = FakeAttachmentResolver::default()
            .with_signature(
                "blob-a",
                MessageAttachmentSignature {
                    mime_type: "image/png".to_string(),
                    name: Some("a.png".to_string()),
                    sha256: "abc".to_string(),
                },
            )
            .with_signature(
                "blob-b",
                MessageAttachmentSignature {
                    mime_type: "image/png".to_string(),
                    name: Some("b.png".to_string()),
                    sha256: "def".to_string(),
                },
            );
        let message_id = MessageId::new();
        let turn_id = TurnId::new();
        let mut first = input(message_id, turn_id, "image");
        first.attachments = vec![MessageAttachment::ImageRef {
            blob_id: "blob-a".to_string(),
            mime_type: "image/png".to_string(),
            name: Some("a.png".to_string()),
        }];
        post_user_message_record(&store, &session, order_seq_state.clone(), &resolver, &first)
            .await
            .expect("create message");

        let mut replay = input(message_id, turn_id, "image");
        replay.attachments = vec![MessageAttachment::ImageRef {
            blob_id: "blob-b".to_string(),
            mime_type: "image/png".to_string(),
            name: Some("b.png".to_string()),
        }];
        let error = post_user_message_record(&store, &session, order_seq_state, &resolver, &replay)
            .await
            .expect_err("mismatched attachment replay");
        assert!(matches!(error, MessageAdmissionError::Conflict(_)));
    }

    #[tokio::test]
    async fn attachment_signature_errors_preserve_error_kind() {
        let (_dir, store, session, order_seq_state) = setup_store().await;
        let resolver = FakeAttachmentResolver::default()
            .with_signature(
                "blob-a",
                MessageAttachmentSignature {
                    mime_type: "image/png".to_string(),
                    name: None,
                    sha256: "abc".to_string(),
                },
            )
            .with_error(
                "blob-b",
                MessageAttachmentSignatureError::BadRequest("missing blob".to_string()),
            );
        let message_id = MessageId::new();
        let turn_id = TurnId::new();
        let mut first = input(message_id, turn_id, "image");
        first.attachments = vec![MessageAttachment::ImageRef {
            blob_id: "blob-a".to_string(),
            mime_type: "image/png".to_string(),
            name: None,
        }];
        post_user_message_record(
            &store,
            &session,
            order_seq_state.clone(),
            &FakeAttachmentResolver::default().with_signature(
                "blob-a",
                MessageAttachmentSignature {
                    mime_type: "image/png".to_string(),
                    name: None,
                    sha256: "abc".to_string(),
                },
            ),
            &first,
        )
        .await
        .expect("create message");

        let mut replay = input(message_id, turn_id, "image");
        replay.attachments = vec![MessageAttachment::ImageRef {
            blob_id: "blob-b".to_string(),
            mime_type: "image/png".to_string(),
            name: None,
        }];
        let error = post_user_message_record(&store, &session, order_seq_state, &resolver, &replay)
            .await
            .expect_err("resolver error");
        assert!(
            matches!(error, MessageAdmissionError::BadRequest(message) if message == "missing blob")
        );
    }

    #[tokio::test]
    async fn identical_attachment_json_still_resolves_signatures_on_replay() {
        let (_dir, store, session, order_seq_state) = setup_store().await;
        let good_resolver = FakeAttachmentResolver::default().with_signature(
            "blob-a",
            MessageAttachmentSignature {
                mime_type: "image/png".to_string(),
                name: None,
                sha256: "abc".to_string(),
            },
        );
        let message_id = MessageId::new();
        let turn_id = TurnId::new();
        let mut first = input(message_id, turn_id, "image");
        first.attachments = vec![MessageAttachment::ImageRef {
            blob_id: "blob-a".to_string(),
            mime_type: "image/png".to_string(),
            name: None,
        }];
        post_user_message_record(
            &store,
            &session,
            order_seq_state.clone(),
            &good_resolver,
            &first,
        )
        .await
        .expect("create message");

        let missing_resolver = FakeAttachmentResolver::default().with_error(
            "blob-a",
            MessageAttachmentSignatureError::BadRequest("missing blob".to_string()),
        );
        let error =
            post_user_message_record(&store, &session, order_seq_state, &missing_resolver, &first)
                .await
                .expect_err("same attachment payload must still resolve signatures");
        assert!(
            matches!(error, MessageAdmissionError::BadRequest(message) if message == "missing blob")
        );
    }
}
