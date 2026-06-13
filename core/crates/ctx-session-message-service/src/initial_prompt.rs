use std::sync::Arc;

use async_trait::async_trait;
use ctx_core::ids::SessionId;
use ctx_core::ids::{MessageId, RunId, TurnId};
use ctx_core::models::{
    Message, MessageDelivery, MessageRole, Session, SessionEvent, SessionEventType, SessionTurn,
    SessionTurnStatus,
};
use ctx_session_tools::order_seq::OrderSeqState;
use ctx_store::{is_unique_constraint_violation, Store};
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct InitialPromptSeedInput {
    pub prompt: String,
    pub message_id: Option<MessageId>,
    pub turn_id: Option<TurnId>,
}

#[derive(Debug, Clone)]
pub enum InitialPromptSeedOutcome {
    Created {
        message: Message,
        event: SessionEvent,
    },
    Replayed {
        message: Message,
    },
}

impl InitialPromptSeedOutcome {
    pub fn created_parts(self) -> Option<(Message, SessionEvent)> {
        match self {
            Self::Created { message, event } => Some((message, event)),
            Self::Replayed { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum InitialPromptSeedError {
    BadRequest,
    Conflict,
    Internal(anyhow::Error),
}

#[async_trait]
pub trait InitialPromptOrderSeqSource: Sync {
    async fn initial_prompt_order_seq_state(
        &self,
        store: &Store,
        session_id: SessionId,
    ) -> Arc<Mutex<OrderSeqState>>;
}

#[derive(Clone, Copy)]
struct InitialPromptIds {
    message_id: MessageId,
    turn_id: TurnId,
}

pub async fn seed_initial_prompt_record(
    store: &Store,
    session: &Session,
    order_seq_source: &impl InitialPromptOrderSeqSource,
    input: InitialPromptSeedInput,
) -> Result<InitialPromptSeedOutcome, InitialPromptSeedError> {
    let ids = initial_prompt_ids(input.message_id, input.turn_id)?;
    let delivery = MessageDelivery::Immediate;

    if let Some(existing) = store
        .get_message(ids.message_id)
        .await
        .map_err(InitialPromptSeedError::Internal)?
    {
        if existing_initial_prompt_message_matches(&existing, session, ids.turn_id, &input.prompt) {
            ensure_session_turn_for_initial_prompt(store, session.id, ids.turn_id, &existing)
                .await?;
            return Ok(InitialPromptSeedOutcome::Replayed { message: existing });
        }
        return Err(InitialPromptSeedError::Conflict);
    }

    let prompt_for_idempotency = input.prompt.clone();
    let run_id = RunId::new();
    let order_seq_state = order_seq_source
        .initial_prompt_order_seq_state(store, session.id)
        .await;
    let order_seq = {
        let mut order_seq_state = order_seq_state.lock().await;
        order_seq_state.get_or_assign(format!("message:{}", ids.message_id.0), None)
    };
    let msg = new_initial_prompt_message(session, ids, run_id, input.prompt, order_seq, delivery);

    let saved = match store.insert_message(msg).await {
        Ok(saved) => saved,
        Err(err) if is_unique_constraint_violation(&err) => {
            let Some(existing) = store
                .get_message(ids.message_id)
                .await
                .map_err(InitialPromptSeedError::Internal)?
            else {
                return Err(InitialPromptSeedError::Internal(anyhow::anyhow!(
                    "message insert conflicted but message row is missing"
                )));
            };
            if existing_initial_prompt_message_matches(
                &existing,
                session,
                ids.turn_id,
                &prompt_for_idempotency,
            ) {
                ensure_session_turn_for_initial_prompt(store, session.id, ids.turn_id, &existing)
                    .await?;
                return Ok(InitialPromptSeedOutcome::Replayed { message: existing });
            }
            return Err(InitialPromptSeedError::Conflict);
        }
        Err(error) => return Err(InitialPromptSeedError::Internal(error)),
    };

    let event = store
        .append_session_event(
            session.id,
            Some(run_id),
            Some(ids.turn_id),
            SessionEventType::UserMessage,
            initial_prompt_user_event_payload(&saved, order_seq),
        )
        .await
        .map_err(InitialPromptSeedError::Internal)?;
    let start_seq = event.seq;
    let turn = initial_prompt_turn(session, ids, run_id, &saved, start_seq);
    ensure_session_turn_for_initial_prompt_with_turn(store, session.id, ids.turn_id, &saved, turn)
        .await?;

    Ok(InitialPromptSeedOutcome::Created {
        message: saved,
        event,
    })
}

fn initial_prompt_ids(
    message_id: Option<MessageId>,
    turn_id: Option<TurnId>,
) -> Result<InitialPromptIds, InitialPromptSeedError> {
    match (message_id, turn_id) {
        (Some(message_id), Some(turn_id)) => Ok(InitialPromptIds {
            message_id,
            turn_id,
        }),
        _ => Err(InitialPromptSeedError::BadRequest),
    }
}

fn existing_initial_prompt_message_matches(
    existing: &Message,
    session: &Session,
    turn_id: TurnId,
    prompt: &str,
) -> bool {
    existing.session_id == session.id
        && existing.turn_id == Some(turn_id)
        && matches!(existing.role, MessageRole::User)
        && existing.content == prompt
        && existing.attachments.is_empty()
        && matches!(existing.delivery, MessageDelivery::Immediate)
}

fn new_initial_prompt_message(
    session: &Session,
    ids: InitialPromptIds,
    run_id: RunId,
    prompt: String,
    order_seq: i64,
    delivery: MessageDelivery,
) -> Message {
    Message {
        id: ids.message_id,
        session_id: session.id,
        task_id: session.task_id,
        run_id: Some(run_id),
        turn_id: Some(ids.turn_id),
        turn_sequence: Some(0),
        order_seq: Some(order_seq),
        role: MessageRole::User,
        content: prompt,
        attachments: Vec::new(),
        delivery,
        delivered_at: None,
        created_at: chrono::Utc::now(),
    }
}

fn initial_prompt_user_event_payload(saved: &Message, order_seq: i64) -> serde_json::Value {
    serde_json::json!({
        "message_id": saved.id.0,
        "content": saved.content.clone(),
        "delivery": saved.delivery.clone(),
        "attachments": saved.attachments.clone(),
        "order_seq": order_seq,
    })
}

fn initial_prompt_turn(
    session: &Session,
    ids: InitialPromptIds,
    run_id: RunId,
    saved: &Message,
    start_seq: i64,
) -> SessionTurn {
    SessionTurn {
        turn_id: ids.turn_id,
        session_id: session.id,
        run_id: Some(run_id),
        user_message_id: Some(saved.id),
        status: SessionTurnStatus::Starting,
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
    }
}

async fn ensure_session_turn_for_initial_prompt(
    store: &Store,
    session_id: SessionId,
    turn_id: TurnId,
    message: &Message,
) -> Result<(), InitialPromptSeedError> {
    let turn = crate::message_delivery::build_user_message_turn(message, turn_id, None);
    ensure_session_turn_for_initial_prompt_with_turn(store, session_id, turn_id, message, turn)
        .await
}

async fn ensure_session_turn_for_initial_prompt_with_turn(
    store: &Store,
    session_id: SessionId,
    turn_id: TurnId,
    message: &Message,
    turn: SessionTurn,
) -> Result<(), InitialPromptSeedError> {
    let existing_turn = store
        .get_session_turn_by_id(turn_id)
        .await
        .map_err(InitialPromptSeedError::Internal)?;
    if let Some(existing) = existing_turn {
        let matches =
            existing.session_id == session_id && existing.user_message_id == Some(message.id);
        if !matches {
            return Err(InitialPromptSeedError::Conflict);
        }
        return Ok(());
    }

    if let Err(err) = store.insert_session_turn(turn).await {
        if !is_unique_constraint_violation(&err) {
            return Err(InitialPromptSeedError::Internal(err));
        }
        let existing = store
            .get_session_turn_by_id(turn_id)
            .await
            .map_err(InitialPromptSeedError::Internal)?;
        if let Some(existing) = existing {
            let matches =
                existing.session_id == session_id && existing.user_message_id == Some(message.id);
            if !matches {
                return Err(InitialPromptSeedError::Conflict);
            }
        } else {
            return Err(InitialPromptSeedError::Internal(anyhow::anyhow!(
                "session turn insert conflicted but turn row is missing"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ctx_core::models::{ExecutionEnvironment, VcsKind};

    use super::*;

    async fn setup_store() -> (tempfile::TempDir, Store, Session, TestOrderSeqSource) {
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
            TestOrderSeqSource::new(OrderSeqState::new(1)),
        )
    }

    fn input(prompt: &str, message_id: MessageId, turn_id: TurnId) -> InitialPromptSeedInput {
        InitialPromptSeedInput {
            prompt: prompt.to_string(),
            message_id: Some(message_id),
            turn_id: Some(turn_id),
        }
    }

    #[tokio::test]
    async fn creates_message_event_and_turn_for_initial_prompt() {
        let (_dir, store, session, order_seq_source) = setup_store().await;
        let message_id = MessageId::new();
        let turn_id = TurnId::new();

        let outcome = seed_initial_prompt_record(
            &store,
            &session,
            &order_seq_source,
            input("hello", message_id, turn_id),
        )
        .await
        .expect("seed prompt");

        let InitialPromptSeedOutcome::Created { message, event } = outcome else {
            panic!("expected created outcome");
        };
        assert_eq!(message.id, message_id);
        assert_eq!(message.turn_id, Some(turn_id));
        assert_eq!(message.content, "hello");
        assert_eq!(message.order_seq, Some(1));
        assert_eq!(event.session_id, session.id);
        assert_eq!(event.turn_id, Some(turn_id));
        assert!(matches!(event.event_type, SessionEventType::UserMessage));
        assert_eq!(
            event.payload_json["message_id"],
            serde_json::json!(message_id.0)
        );
        assert_eq!(event.payload_json["content"], serde_json::json!("hello"));
        assert_eq!(event.payload_json["order_seq"], serde_json::json!(1));

        let turn = store
            .get_session_turn_by_id(turn_id)
            .await
            .expect("load turn")
            .expect("turn");
        assert_eq!(turn.user_message_id, Some(message_id));
        assert_eq!(turn.start_seq, Some(event.seq));
        assert_eq!(turn.status, SessionTurnStatus::Starting);
    }

    #[tokio::test]
    async fn idempotent_replay_returns_existing_without_new_event() {
        let (_dir, store, session, order_seq_source) = setup_store().await;
        let message_id = MessageId::new();
        let turn_id = TurnId::new();
        let input = input("hello", message_id, turn_id);

        let first = seed_initial_prompt_record(&store, &session, &order_seq_source, input.clone())
            .await
            .expect("first seed");
        assert!(matches!(first, InitialPromptSeedOutcome::Created { .. }));

        let replayed = seed_initial_prompt_record(&store, &session, &order_seq_source, input)
            .await
            .expect("replay seed");
        let InitialPromptSeedOutcome::Replayed { message } = replayed else {
            panic!("expected replayed outcome");
        };
        assert_eq!(message.id, message_id);
        let events = store
            .list_session_events(session.id)
            .await
            .expect("list events");
        assert_eq!(events.len(), 1);
        assert_eq!(order_seq_source.calls(), 1);
    }

    #[tokio::test]
    async fn replay_rejects_changed_payload() {
        let (_dir, store, session, order_seq_source) = setup_store().await;
        let message_id = MessageId::new();
        let turn_id = TurnId::new();
        seed_initial_prompt_record(
            &store,
            &session,
            &order_seq_source,
            input("hello", message_id, turn_id),
        )
        .await
        .expect("first seed");

        let error = seed_initial_prompt_record(
            &store,
            &session,
            &order_seq_source,
            input("changed", message_id, turn_id),
        )
        .await
        .expect_err("changed payload should conflict");
        assert!(matches!(error, InitialPromptSeedError::Conflict));
    }

    #[tokio::test]
    async fn missing_typed_ids_are_bad_request() {
        let (_dir, store, session, order_seq_source) = setup_store().await;
        let error = seed_initial_prompt_record(
            &store,
            &session,
            &order_seq_source,
            InitialPromptSeedInput {
                prompt: "hello".to_string(),
                message_id: Some(MessageId::new()),
                turn_id: None,
            },
        )
        .await
        .expect_err("missing turn id should fail");
        assert!(matches!(error, InitialPromptSeedError::BadRequest));
        assert_eq!(order_seq_source.calls(), 0);
    }

    #[tokio::test]
    async fn existing_turn_mismatch_conflicts() {
        let (_dir, store, session, order_seq_source) = setup_store().await;
        let message_id = MessageId::new();
        let turn_id = TurnId::new();
        let conflicting_turn = SessionTurn {
            turn_id,
            session_id: session.id,
            run_id: None,
            user_message_id: None,
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
        };
        store
            .insert_session_turn(conflicting_turn)
            .await
            .expect("conflicting turn");

        let error = seed_initial_prompt_record(
            &store,
            &session,
            &order_seq_source,
            input("hello", message_id, turn_id),
        )
        .await
        .expect_err("turn mismatch should conflict");
        assert!(matches!(error, InitialPromptSeedError::Conflict));
    }

    #[derive(Clone)]
    struct TestOrderSeqSource {
        state: Arc<Mutex<OrderSeqState>>,
        calls: Arc<AtomicUsize>,
    }

    impl TestOrderSeqSource {
        fn new(state: OrderSeqState) -> Self {
            Self {
                state: Arc::new(Mutex::new(state)),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl InitialPromptOrderSeqSource for TestOrderSeqSource {
        async fn initial_prompt_order_seq_state(
            &self,
            _store: &Store,
            _session_id: SessionId,
        ) -> Arc<Mutex<OrderSeqState>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Arc::clone(&self.state)
        }
    }
}
