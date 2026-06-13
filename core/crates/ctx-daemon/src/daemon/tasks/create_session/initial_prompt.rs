use super::*;

use std::sync::Arc;

use ctx_core::ids::{MessageId, TurnId};
use ctx_session_message_service::initial_prompt::{
    seed_initial_prompt_record, InitialPromptOrderSeqSource, InitialPromptSeedError,
    InitialPromptSeedInput, InitialPromptSeedOutcome,
};
use ctx_session_tools::order_seq::OrderSeqState;
use tokio::sync::Mutex;

pub(super) struct InitialPromptSeed {
    pub(super) prompt: Option<String>,
    pub(super) message_id: Option<String>,
    pub(super) turn_id: Option<String>,
    pub(super) run_id_header: Option<String>,
}

pub(super) async fn seed_initial_prompt(
    handles: &TaskSessionHandles,
    store: &Store,
    session: &Session,
    seed: InitialPromptSeed,
) -> Result<(), TaskSessionCreateError> {
    let side_effects = TaskSessionInitialPromptSideEffects { handles };
    seed_initial_prompt_with_side_effects(&side_effects, store, session, seed).await
}

async fn seed_initial_prompt_with_side_effects<S>(
    side_effects: &S,
    store: &Store,
    session: &Session,
    seed: InitialPromptSeed,
) -> Result<(), TaskSessionCreateError>
where
    S: InitialPromptDaemonSideEffects + InitialPromptOrderSeqSource + Sync,
{
    let Some(prompt) = seed.prompt else {
        return Ok(());
    };

    let message_id = parse_initial_prompt_message_id(seed.message_id.as_deref())?;
    let turn_id = parse_initial_prompt_turn_id(seed.turn_id.as_deref())?;
    let outcome = seed_initial_prompt_record(
        store,
        session,
        side_effects,
        InitialPromptSeedInput {
            prompt,
            message_id,
            turn_id,
        },
    )
    .await
    .map_err(map_initial_prompt_seed_error)?;

    apply_initial_prompt_outcome(side_effects, session, seed.run_id_header, outcome).await
}

fn parse_initial_prompt_message_id(
    raw: Option<&str>,
) -> Result<Option<MessageId>, TaskSessionCreateError> {
    parse_initial_prompt_uuid(raw)
        .map(|uuid| uuid.map(MessageId))
        .map_err(|_| TaskSessionCreateError::BadRequest)
}

fn parse_initial_prompt_turn_id(
    raw: Option<&str>,
) -> Result<Option<TurnId>, TaskSessionCreateError> {
    parse_initial_prompt_uuid(raw)
        .map(|uuid| uuid.map(TurnId))
        .map_err(|_| TaskSessionCreateError::BadRequest)
}

fn parse_initial_prompt_uuid(raw: Option<&str>) -> Result<Option<uuid::Uuid>, uuid::Error> {
    raw.map(|value| uuid::Uuid::parse_str(value.trim()))
        .transpose()
}

fn map_initial_prompt_seed_error(error: InitialPromptSeedError) -> TaskSessionCreateError {
    match error {
        InitialPromptSeedError::BadRequest => TaskSessionCreateError::BadRequest,
        InitialPromptSeedError::Conflict => TaskSessionCreateError::Conflict,
        InitialPromptSeedError::Internal(error) => TaskSessionCreateError::Internal(error),
    }
}

#[async_trait::async_trait]
trait InitialPromptDaemonSideEffects {
    async fn publish_event(&self, event: ctx_core::models::SessionEvent);

    async fn enqueue_initial_prompt(
        &self,
        session: &Session,
        message: Message,
        run_id_header: Option<String>,
    );

    async fn schedule_title_generation(&self, session: &Session, prompt: String);
}

struct TaskSessionInitialPromptSideEffects<'a> {
    handles: &'a TaskSessionHandles,
}

#[async_trait::async_trait]
impl InitialPromptDaemonSideEffects for TaskSessionInitialPromptSideEffects<'_> {
    async fn publish_event(&self, event: ctx_core::models::SessionEvent) {
        self.handles.admission.publish_event(event).await;
    }

    async fn enqueue_initial_prompt(
        &self,
        session: &Session,
        message: Message,
        run_id_header: Option<String>,
    ) {
        let tx = self
            .handles
            .admission
            .ensure_scheduler(session.clone())
            .await;
        let queued = crate::daemon::scheduler::QueuedMessage {
            message,
            enqueued_at: std::time::Instant::now(),
            run_id: run_id_header,
        };
        let _ = tx.send(SchedulerCommand::Enqueue(queued)).await;
    }

    async fn schedule_title_generation(&self, session: &Session, prompt: String) {
        let _ = self
            .handles
            .admission
            .schedule_session_title_generation(session.clone(), prompt, false)
            .await;
    }
}

#[async_trait::async_trait]
impl InitialPromptOrderSeqSource for TaskSessionInitialPromptSideEffects<'_> {
    async fn initial_prompt_order_seq_state(
        &self,
        store: &Store,
        session_id: SessionId,
    ) -> Arc<Mutex<OrderSeqState>> {
        self.handles
            .admission
            .session_order_seq_state(store, session_id)
            .await
    }
}

async fn apply_initial_prompt_outcome<S>(
    side_effects: &S,
    session: &Session,
    run_id_header: Option<String>,
    outcome: InitialPromptSeedOutcome,
) -> Result<(), TaskSessionCreateError>
where
    S: InitialPromptDaemonSideEffects + Sync,
{
    let InitialPromptSeedOutcome::Created { message, event } = outcome else {
        return Ok(());
    };

    side_effects.publish_event(event).await;
    let prompt = message.content.clone();
    side_effects
        .enqueue_initial_prompt(session, message, run_id_header)
        .await;
    side_effects
        .schedule_title_generation(session, prompt)
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ctx_core::models::{ExecutionEnvironment, SessionEvent, VcsKind};

    use super::*;

    #[test]
    fn parses_initial_prompt_client_ids_in_daemon_layer() {
        let message_id = MessageId::new();
        let turn_id = TurnId::new();

        assert_eq!(
            parse_initial_prompt_message_id(Some(&message_id.0.to_string())).expect("message id"),
            Some(message_id)
        );
        assert_eq!(
            parse_initial_prompt_turn_id(Some(&turn_id.0.to_string())).expect("turn id"),
            Some(turn_id)
        );
        assert!(matches!(
            parse_initial_prompt_message_id(Some("not-a-uuid")),
            Err(TaskSessionCreateError::BadRequest)
        ));
    }

    #[tokio::test]
    async fn seed_initial_prompt_replay_skips_daemon_side_effects() {
        let (_dir, store, session) = setup_session().await;
        let side_effects = TestInitialPromptSideEffects::new();
        let message_id = MessageId::new();
        let turn_id = TurnId::new();

        seed_initial_prompt_with_side_effects(
            &side_effects,
            &store,
            &session,
            initial_prompt_seed(message_id, turn_id),
        )
        .await
        .expect("first seed should create prompt");

        let first = side_effects.snapshot().await;
        assert_eq!(first.published_events.len(), 1);
        assert_eq!(first.enqueued_messages.len(), 1);
        assert_eq!(first.title_prompts, vec!["hello".to_string()]);
        assert_eq!(first.enqueued_messages[0].message_id, message_id);
        assert_eq!(
            first.enqueued_messages[0].run_id_header.as_deref(),
            Some("run-header")
        );
        assert_eq!(side_effects.order_seq_state_requests(), 1);

        seed_initial_prompt_with_side_effects(
            &side_effects,
            &store,
            &session,
            initial_prompt_seed(message_id, turn_id),
        )
        .await
        .expect("matching replay should be idempotent");

        let replayed = side_effects.snapshot().await;
        assert_eq!(replayed.published_events.len(), 1);
        assert_eq!(replayed.enqueued_messages.len(), 1);
        assert_eq!(replayed.title_prompts.len(), 1);
        assert_eq!(side_effects.order_seq_state_requests(), 1);

        let persisted_events = store
            .list_session_events(session.id)
            .await
            .expect("list events");
        assert_eq!(persisted_events.len(), 1);
    }

    fn initial_prompt_seed(message_id: MessageId, turn_id: TurnId) -> InitialPromptSeed {
        InitialPromptSeed {
            prompt: Some("hello".to_string()),
            message_id: Some(message_id.0.to_string()),
            turn_id: Some(turn_id.0.to_string()),
            run_id_header: Some("run-header".to_string()),
        }
    }

    async fn setup_session() -> (tempfile::TempDir, Store, Session) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("db.sqlite");
        let store = Store::open(&db_path).await.expect("open store");
        let workspace = store
            .create_workspace(
                "workspace".to_string(),
                "/tmp/workspace".to_string(),
                VcsKind::Git,
            )
            .await
            .expect("workspace");
        let task = store
            .create_task(workspace.id, "task".to_string(), None)
            .await
            .expect("task");
        let worktree = store
            .create_worktree(
                workspace.id,
                "/tmp/worktree".to_string(),
                "base".to_string(),
                None,
            )
            .await
            .expect("worktree");
        let session = store
            .create_session(
                task.id,
                workspace.id,
                worktree.id,
                ExecutionEnvironment::Host,
                "fake".to_string(),
                "model".to_string(),
                "implementer".to_string(),
                None,
                None,
                None,
            )
            .await
            .expect("session");
        (dir, store, session)
    }

    #[derive(Clone, Debug)]
    struct RecordedEnqueue {
        message_id: MessageId,
        run_id_header: Option<String>,
    }

    #[derive(Clone, Debug, Default)]
    struct RecordedInitialPromptEffects {
        published_events: Vec<SessionEvent>,
        enqueued_messages: Vec<RecordedEnqueue>,
        title_prompts: Vec<String>,
    }

    struct TestInitialPromptSideEffects {
        order_seq_state: Arc<Mutex<OrderSeqState>>,
        order_seq_state_requests: Arc<AtomicUsize>,
        records: Arc<Mutex<RecordedInitialPromptEffects>>,
    }

    impl TestInitialPromptSideEffects {
        fn new() -> Self {
            Self {
                order_seq_state: Arc::new(Mutex::new(OrderSeqState::new(1))),
                order_seq_state_requests: Arc::new(AtomicUsize::new(0)),
                records: Arc::new(Mutex::new(RecordedInitialPromptEffects::default())),
            }
        }

        async fn snapshot(&self) -> RecordedInitialPromptEffects {
            self.records.lock().await.clone()
        }

        fn order_seq_state_requests(&self) -> usize {
            self.order_seq_state_requests.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl InitialPromptDaemonSideEffects for TestInitialPromptSideEffects {
        async fn publish_event(&self, event: SessionEvent) {
            self.records.lock().await.published_events.push(event);
        }

        async fn enqueue_initial_prompt(
            &self,
            _session: &Session,
            message: Message,
            run_id_header: Option<String>,
        ) {
            self.records
                .lock()
                .await
                .enqueued_messages
                .push(RecordedEnqueue {
                    message_id: message.id,
                    run_id_header,
                });
        }

        async fn schedule_title_generation(&self, _session: &Session, prompt: String) {
            self.records.lock().await.title_prompts.push(prompt);
        }
    }

    #[async_trait::async_trait]
    impl InitialPromptOrderSeqSource for TestInitialPromptSideEffects {
        async fn initial_prompt_order_seq_state(
            &self,
            _store: &Store,
            _session_id: SessionId,
        ) -> Arc<Mutex<OrderSeqState>> {
            self.order_seq_state_requests.fetch_add(1, Ordering::SeqCst);
            Arc::clone(&self.order_seq_state)
        }
    }
}
