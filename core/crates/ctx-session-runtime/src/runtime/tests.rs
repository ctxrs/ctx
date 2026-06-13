use super::*;

use async_trait::async_trait;
use chrono::Utc;
use ctx_core::ids::{MessageId, SessionEventId, TurnId, WorktreeId};
use ctx_core::models::{
    ExecutionEnvironment, SessionActivityState, SessionHeadDelta, SessionHeadWindow, SessionStatus,
    SessionSummaryDelta, SessionTurn, SessionTurnStatus,
};
use serde_json::json;

#[tokio::test]
async fn pin_state_reports_only_pinned_transitions() {
    let runtime = SessionRuntime::<()>::new(Duration::from_secs(60));
    let session_id = SessionId::new();

    assert_eq!(runtime.set_running(session_id, true).await, Some(true));
    assert_eq!(runtime.set_running(session_id, true).await, None);
    assert_eq!(runtime.attach_session(session_id).await, None);
    assert_eq!(runtime.set_running(session_id, false).await, None);
    assert_eq!(runtime.detach_session(session_id).await, Some(false));
}

#[tokio::test]
async fn lifecycle_host_receives_only_pin_transitions_and_cleanup() {
    let runtime = SessionRuntime::<()>::new(Duration::from_secs(60));
    let host = RecordingLifecycleHost::default();
    let session_id = SessionId::new();

    runtime.set_running_with_host(&host, session_id, true).await;
    runtime.set_running_with_host(&host, session_id, true).await;
    runtime.attach_session_with_host(&host, session_id).await;
    runtime
        .set_running_with_host(&host, session_id, false)
        .await;
    runtime.detach_session_with_host(&host, session_id).await;
    runtime.cleanup_session_with_host(&host, session_id).await;

    assert_eq!(
        host.pin_updates.lock().await.as_slice(),
        &[(session_id, true), (session_id, false)]
    );
    assert_eq!(host.removed_sessions.lock().await.as_slice(), &[session_id]);
    assert!(!runtime.is_running(session_id).await);
    assert!(runtime.session_meta_workspace(session_id).await.is_none());
}

#[tokio::test]
async fn head_refresh_host_updates_found_heads_and_removes_missing_sessions() {
    let runtime = SessionRuntime::<()>::new(Duration::from_secs(60));
    let session = test_session();
    let head = test_head_snapshot(&session);
    let host = RecordingHeadRefreshHost::new(SessionHeadRefreshLoad::Found(Box::new(head.clone())));

    runtime
        .refresh_session_head_cache_with_host(&host, session.id)
        .await;

    let updated_heads = host.updated_heads.lock().await;
    assert_eq!(updated_heads.len(), 1);
    assert_eq!(updated_heads[0].session.id, session.id);
    assert_eq!(updated_heads[0].last_event_seq, head.last_event_seq);
    drop(updated_heads);
    assert!(host.removed_sessions.lock().await.is_empty());

    let missing = RecordingHeadRefreshHost::new(SessionHeadRefreshLoad::Missing);
    runtime
        .refresh_session_head_cache_with_host(&missing, session.id)
        .await;
    assert_eq!(
        missing.removed_sessions.lock().await.as_slice(),
        &[session.id]
    );
}

#[tokio::test]
async fn task_session_creation_lock_reuses_live_lock_and_prunes_dead_entries() {
    let runtime = SessionRuntime::<()>::new(Duration::from_secs(60));
    let task_id = TaskId::new();

    let first = runtime.task_session_creation_lock(task_id).await;
    let second = runtime.task_session_creation_lock(task_id).await;
    assert!(Arc::ptr_eq(&first, &second));

    drop(first);
    drop(second);

    let replacement = runtime.task_session_creation_lock(task_id).await;
    assert_eq!(Arc::strong_count(&replacement), 1);
}

#[tokio::test]
async fn publish_gap_notice_only_updates_realtime_session_channels() {
    let runtime = SessionRuntime::<()>::new(Duration::from_secs(60));
    let host = RecordingPublicationHost::default();
    let session_id = SessionId::new();
    let mut events = runtime.get_broadcaster(session_id).await.subscribe();
    let head = runtime.subscribe_session_event_head(session_id).await;
    let event = test_event(
        session_id,
        SessionEventType::Notice,
        json!({
            "kind": "session_gap",
            "reason": "data_plane_overflow"
        }),
    );

    runtime.publish_event_with_host(&host, event.clone()).await;

    assert_eq!(events.try_recv().expect("published event").id, event.id);
    assert_eq!(*head.borrow(), event.seq);
    assert_eq!(host.load_session_calls.lock().await.len(), 0);
    assert!(host.head_deltas.lock().await.is_empty());
    assert!(host.summary_deltas.lock().await.is_empty());
}

#[tokio::test]
async fn publish_user_message_materializes_head_summary_and_task_delta() {
    let runtime = SessionRuntime::<()>::new(Duration::from_secs(60));
    let session = test_session();
    let host = RecordingPublicationHost::new(session.clone());
    *host.projection_rev.lock().await = Some(42);
    let mut event = test_event(session.id, SessionEventType::UserMessage, json!({}));
    let turn_id = TurnId::new();
    let message_id = MessageId::new();
    event.turn_id = Some(turn_id);
    event.seq = 12;
    event.payload_json = json!({
        "message_id": message_id.0.to_string(),
        "content": "first line\nsecond line",
        "order_seq": 3
    });

    runtime.publish_event_with_host(&host, event).await;

    let head_deltas = host.head_deltas.lock().await;
    assert_eq!(head_deltas.len(), 1);
    let published = &head_deltas[0];
    assert_eq!(published.workspace_id, session.workspace_id);
    assert!(published.durable);
    assert_eq!(published.delta.last_event_seq, 12);
    assert_eq!(published.delta.projection_rev, 42);
    assert_eq!(
        published.delta.message.as_ref().map(|message| message.id),
        Some(message_id)
    );
    assert_eq!(
        published
            .delta
            .turn
            .as_ref()
            .map(|turn| turn.status.clone()),
        Some(SessionTurnStatus::Starting)
    );
    drop(head_deltas);

    let summary_deltas = host.summary_deltas.lock().await;
    assert_eq!(summary_deltas.len(), 1);
    assert_eq!(
        summary_deltas[0].last_message_preview.as_deref(),
        Some("first line")
    );
    drop(summary_deltas);

    wait_for_task_delta(&host.task_delta_refresh_host).await;
    assert_eq!(
        host.task_delta_refresh_host
            .task_ids
            .lock()
            .await
            .as_slice(),
        &[session.task_id]
    );
}

#[tokio::test]
async fn publish_turn_interrupted_materializes_immediate_terminal_head_state() {
    let runtime = SessionRuntime::<()>::new(Duration::from_secs(60));
    let session = test_session();
    let host = RecordingPublicationHost::new(session.clone());
    *host.projection_rev.lock().await = Some(44);
    let turn_id = TurnId::new();
    *host.turn.lock().await = Some(SessionTurn {
        turn_id,
        session_id: session.id,
        run_id: None,
        user_message_id: None,
        status: SessionTurnStatus::Running,
        start_seq: Some(12),
        end_seq: None,
        started_at: Utc::now(),
        updated_at: Utc::now(),
        assistant_partial: None,
        thought_partial: None,
        metrics_json: None,
        failure: None,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
    });
    let mut event = test_event(
        session.id,
        SessionEventType::TurnInterrupted,
        json!({"reason": "user"}),
    );
    event.turn_id = Some(turn_id);
    event.seq = 45;

    runtime.publish_event_with_host(&host, event).await;

    let head_deltas = host.head_deltas.lock().await;
    assert_eq!(head_deltas.len(), 1);
    let published = &head_deltas[0];
    assert!(published.durable);
    assert_eq!(published.delta.last_event_seq, 45);
    assert_eq!(published.delta.projection_rev, 44);
    assert_eq!(
        published
            .delta
            .turn
            .as_ref()
            .map(|turn| turn.status.clone()),
        Some(SessionTurnStatus::Interrupted)
    );
    assert_eq!(
        published.delta.turn.as_ref().and_then(|turn| turn.end_seq),
        Some(45)
    );
    assert_eq!(
        published
            .delta
            .activity
            .as_ref()
            .map(|activity| activity.last_turn_status.clone()),
        Some(Some(SessionTurnStatus::Interrupted))
    );
    assert_eq!(
        published
            .delta
            .activity
            .as_ref()
            .map(|activity| activity.is_working),
        Some(false)
    );
    drop(head_deltas);

    let summary_deltas = host.summary_deltas.lock().await;
    assert_eq!(summary_deltas.len(), 1);
    let activity = summary_deltas[0]
        .activity
        .as_ref()
        .expect("summary activity");
    assert!(!activity.is_working);
    assert_eq!(
        activity.last_turn_status,
        Some(SessionTurnStatus::Interrupted)
    );
}

#[tokio::test]
async fn stream_only_event_uses_replay_cursor_and_stays_transient() {
    let runtime = SessionRuntime::<()>::new(Duration::from_secs(60));
    let session = test_session();
    let host = RecordingPublicationHost::new(session.clone());
    *host.replay_cursor.lock().await = SessionReplayCursor {
        last_event_seq: 25,
        projection_rev: 9,
    };
    let mut event = test_event(
        session.id,
        SessionEventType::AssistantChunk,
        json!({"delta": "partial"}),
    );
    event.seq = 30;

    runtime.publish_event_with_host(&host, event).await;

    let head_deltas = host.head_deltas.lock().await;
    assert_eq!(head_deltas.len(), 1);
    assert!(!head_deltas[0].durable);
    assert_eq!(head_deltas[0].delta.last_event_seq, 25);
    assert_eq!(head_deltas[0].delta.projection_rev, 9);
    drop(head_deltas);
    assert_eq!(*host.projection_rev_loads.lock().await, 0);
}

#[tokio::test]
async fn task_delta_refresh_debounces_to_latest_generation() {
    let runtime = SessionRuntime::<()>::new(Duration::from_secs(60));
    let task_id = TaskId::new();
    let host = Arc::new(RecordingTaskDeltaRefreshHost::default());

    runtime
        .queue_task_delta_refresh_with_debounce(
            Arc::clone(&host),
            task_id,
            Duration::from_millis(1),
        )
        .await;
    runtime
        .queue_task_delta_refresh_with_debounce(
            Arc::clone(&host),
            task_id,
            Duration::from_millis(1),
        )
        .await;

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if !host.task_ids.lock().await.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("task delta refresh should fire");

    assert_eq!(host.task_ids.lock().await.as_slice(), &[task_id]);
    assert!(runtime.active_task_refreshes.lock().await.is_empty());
}

async fn wait_for_task_delta(host: &RecordingTaskDeltaRefreshHost) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if !host.task_ids.lock().await.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("task delta refresh should fire");
}

fn test_session() -> Session {
    let now = Utc::now();
    Session {
        id: SessionId::new(),
        task_id: TaskId::new(),
        workspace_id: WorkspaceId::new(),
        worktree_id: WorktreeId::new(),
        execution_environment: ExecutionEnvironment::Host,
        parent_session_id: None,
        relationship: None,
        provider_id: "fake".to_string(),
        model_id: "fake-model".to_string(),
        reasoning_effort: None,
        title: "Test".to_string(),
        agent_role: "assistant".to_string(),
        status: SessionStatus::Active,
        provider_session_ref: None,
        created_at: now,
        updated_at: now,
    }
}

fn test_event(
    session_id: SessionId,
    event_type: SessionEventType,
    payload_json: serde_json::Value,
) -> SessionEvent {
    SessionEvent {
        seq: 7,
        id: SessionEventId::new(),
        session_id,
        run_id: None,
        turn_id: None,
        event_type,
        payload_json,
        transient: false,
        created_at: Utc::now(),
    }
}

fn test_head_snapshot(session: &Session) -> SessionHeadSnapshot {
    SessionHeadSnapshot {
        session: crate::head_projection::session_metadata_from_session(session),
        turns: Vec::new(),
        tool_summaries: Vec::new(),
        events: Vec::new(),
        messages: Vec::new(),
        last_event_seq: 5,
        projection_rev: 5,
        state_rev: 5,
        activity: SessionActivityState::default(),
        has_more_turns: false,
        history_cursor: None,
        has_more_history: false,
        summary_checkpoint: None,
        head_window: SessionHeadWindow::default(),
    }
}

#[derive(Default)]
struct RecordingPublicationHost {
    session: Mutex<Option<Session>>,
    turn: Mutex<Option<SessionTurn>>,
    task_delta_refresh_host: Arc<RecordingTaskDeltaRefreshHost>,
    replay_cursor: Mutex<SessionReplayCursor>,
    projection_rev: Mutex<Option<i64>>,
    projection_rev_loads: Mutex<usize>,
    load_session_calls: Mutex<Vec<SessionId>>,
    head_deltas: Mutex<Vec<PublishedHeadDelta>>,
    summary_deltas: Mutex<Vec<SessionSummaryDelta>>,
}

impl RecordingPublicationHost {
    fn new(session: Session) -> Self {
        Self {
            session: Mutex::new(Some(session)),
            ..Self::default()
        }
    }
}

struct PublishedHeadDelta {
    workspace_id: WorkspaceId,
    delta: SessionHeadDelta,
    durable: bool,
}

#[async_trait]
impl SessionEventPublicationHost for RecordingPublicationHost {
    type TaskDeltaRefreshHost = RecordingTaskDeltaRefreshHost;

    fn task_delta_refresh_host(&self) -> Arc<Self::TaskDeltaRefreshHost> {
        Arc::clone(&self.task_delta_refresh_host)
    }

    async fn load_session(&self, session_id: SessionId) -> Option<Session> {
        self.load_session_calls.lock().await.push(session_id);
        self.session.lock().await.clone()
    }

    async fn list_turn_tool_summaries_for_turn(
        &self,
        _session_id: SessionId,
        _turn_id: TurnId,
    ) -> Vec<SessionTurnToolSummary> {
        Vec::new()
    }

    async fn cached_turn_for_read(
        &self,
        _session_id: SessionId,
        _turn_id: TurnId,
    ) -> Option<SessionTurn> {
        self.turn.lock().await.clone()
    }

    async fn load_turn(&self, _session_id: SessionId, _turn_id: TurnId) -> Option<SessionTurn> {
        self.turn.lock().await.clone()
    }

    async fn session_replay_cursor(
        &self,
        _workspace_id: WorkspaceId,
        _session_id: SessionId,
    ) -> SessionReplayCursor {
        *self.replay_cursor.lock().await
    }

    async fn load_projection_rev(&self, _session_id: SessionId) -> Option<i64> {
        *self.projection_rev_loads.lock().await += 1;
        *self.projection_rev.lock().await
    }

    async fn publish_session_head_delta(
        &self,
        workspace_id: WorkspaceId,
        _session: &Session,
        delta: SessionHeadDelta,
        durable: bool,
    ) {
        self.head_deltas.lock().await.push(PublishedHeadDelta {
            workspace_id,
            delta,
            durable,
        });
    }

    async fn publish_session_summary_delta(
        &self,
        _workspace_id: WorkspaceId,
        delta: SessionSummaryDelta,
    ) {
        self.summary_deltas.lock().await.push(delta);
    }
}

#[derive(Default)]
struct RecordingTaskDeltaRefreshHost {
    task_ids: Mutex<Vec<TaskId>>,
}

#[async_trait]
impl SessionTaskDeltaRefreshHost for RecordingTaskDeltaRefreshHost {
    async fn emit_task_delta_refresh(&self, task_id: TaskId) {
        self.task_ids.lock().await.push(task_id);
    }
}

#[derive(Default)]
struct RecordingLifecycleHost {
    pin_updates: Mutex<Vec<(SessionId, bool)>>,
    removed_sessions: Mutex<Vec<SessionId>>,
}

#[async_trait]
impl SessionLifecycleHost for RecordingLifecycleHost {
    async fn set_provider_session_pinned(&self, session_id: SessionId, pinned: bool) {
        self.pin_updates.lock().await.push((session_id, pinned));
    }

    async fn remove_workspace_active_session(&self, session_id: SessionId) {
        self.removed_sessions.lock().await.push(session_id);
    }
}

struct RecordingHeadRefreshHost {
    load: Mutex<Option<SessionHeadRefreshLoad>>,
    updated_heads: Mutex<Vec<SessionHeadSnapshot>>,
    removed_sessions: Mutex<Vec<SessionId>>,
}

impl RecordingHeadRefreshHost {
    fn new(load: SessionHeadRefreshLoad) -> Self {
        Self {
            load: Mutex::new(Some(load)),
            updated_heads: Mutex::new(Vec::new()),
            removed_sessions: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl SessionHeadRefreshHost for RecordingHeadRefreshHost {
    async fn load_active_snapshot_head(&self, _session_id: SessionId) -> SessionHeadRefreshLoad {
        self.load
            .lock()
            .await
            .take()
            .expect("load should be called once")
    }

    async fn update_compact_session_head(&self, head: SessionHeadSnapshot) {
        self.updated_heads.lock().await.push(head);
    }

    async fn remove_session_from_active_head_cache(&self, session_id: SessionId) {
        self.removed_sessions.lock().await.push(session_id);
    }
}
