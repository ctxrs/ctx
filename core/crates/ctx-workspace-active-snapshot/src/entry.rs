use std::collections::HashMap;

use tokio::sync::broadcast;

use ctx_core::ids::{SessionId, TaskId};
use ctx_core::models::{
    SessionHeadDelta, SessionHeadSnapshot, WorkspaceActiveSnapshotEvent, WorkspaceActiveTaskSummary,
};

use crate::replay_state::{SessionReplayResult, SessionReplayState};
use crate::SessionReplayCursor;

// The workspace stream fanout is upstream of per-socket coalescing. It must absorb
// a remote soak sized burst long enough for socket tasks to drain into their
// foreground/background queues without broadcast receiver loss.
pub(crate) const WORKSPACE_ACTIVE_SNAPSHOT_STREAM_BUFFER_CAPACITY: usize = 4096;

pub(super) struct WorkspaceActiveSnapshotEntry {
    pub(super) tx: broadcast::Sender<WorkspaceActiveSnapshotEvent>,
    pub(super) snapshot_rev: i64,
    pub(super) archived_rev: i64,
    pub(super) hydrated: bool,
    pub(super) active_tasks: HashMap<TaskId, WorkspaceActiveTaskSummary>,
    pub(super) active_heads: HashMap<SessionId, SessionHeadSnapshot>,
    pub(super) session_replay: HashMap<SessionId, SessionReplayState>,
}

impl WorkspaceActiveSnapshotEntry {
    pub(super) fn new() -> Self {
        let (tx, _) = broadcast::channel(WORKSPACE_ACTIVE_SNAPSHOT_STREAM_BUFFER_CAPACITY);
        Self {
            tx,
            snapshot_rev: 0,
            archived_rev: 0,
            hydrated: false,
            active_tasks: HashMap::new(),
            active_heads: HashMap::new(),
            session_replay: HashMap::new(),
        }
    }

    pub(super) fn primary_session_id_for_task(task: &WorkspaceActiveTaskSummary) -> SessionId {
        task.primary_session_head
            .as_ref()
            .map(|head| head.session.id)
            .unwrap_or(task.primary_session.session.id)
    }

    pub(super) fn remove_active_task_state(&mut self, task_id: TaskId) -> Option<SessionId> {
        let removed = self.active_tasks.remove(&task_id)?;
        let session_id = Self::primary_session_id_for_task(&removed);
        self.active_heads.remove(&session_id);
        self.session_replay.remove(&session_id);
        Some(session_id)
    }

    pub(super) fn is_primary_session(&self, session_id: SessionId) -> bool {
        self.active_tasks.values().any(|summary| {
            let primary_id = summary
                .task
                .primary_session_id
                .unwrap_or(summary.primary_session.session.id);
            primary_id == session_id
        })
    }

    pub(super) fn session_last_event_seq(&self, session_id: SessionId) -> i64 {
        self.session_replay_cursor(session_id).last_event_seq
    }

    pub(super) fn session_replay_cursor(&self, session_id: SessionId) -> SessionReplayCursor {
        self.session_replay
            .get(&session_id)
            .map(|state| state.last_cursor)
            .unwrap_or_default()
    }

    pub(super) fn record_session_delta(&mut self, delta: &SessionHeadDelta) {
        let state = self.session_replay.entry(delta.session_id).or_default();
        state.record(delta);
    }

    pub(super) fn seed_session_replay(
        &mut self,
        session_id: SessionId,
        cursor: SessionReplayCursor,
    ) {
        let state = self.session_replay.entry(session_id).or_default();
        state.seed(cursor);
    }

    pub(super) fn replay_session(
        &self,
        session_id: SessionId,
        after_seq: i64,
        after_projection_rev: i64,
        limit: usize,
    ) -> SessionReplayResult {
        let after_cursor = SessionReplayCursor {
            last_event_seq: after_seq.max(0),
            projection_rev: after_projection_rev.max(0),
        };
        match self.session_replay.get(&session_id) {
            Some(state) => state.replay(after_cursor, limit),
            None => {
                if after_cursor <= SessionReplayCursor::default() {
                    SessionReplayResult::Replay {
                        deltas: Vec::new(),
                        last_sent: after_cursor,
                    }
                } else {
                    let last_known_seq = self
                        .active_heads
                        .get(&session_id)
                        .map(|head| head.last_event_seq)
                        .unwrap_or(after_cursor.last_event_seq)
                        .max(after_cursor.last_event_seq);
                    SessionReplayResult::Gap {
                        last_known_seq,
                        reason: Some("missing_replay_state".to_string()),
                    }
                }
            }
        }
    }
}
