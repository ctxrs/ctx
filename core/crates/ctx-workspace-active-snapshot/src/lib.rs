use std::collections::HashMap;

use tokio::sync::{broadcast, Mutex};

use cache::CachedSessionHead;
use ctx_core::ids::{SessionId, WorkspaceId};
use ctx_core::models::WorkspaceActiveSnapshotEvent;
use entry::WorkspaceActiveSnapshotEntry;
use replay_state::SessionReplayResult;

#[cfg(test)]
use ctx_core::ids::{TaskId, WorktreeId};
#[cfg(test)]
use ctx_core::models::{
    Message, Session, SessionActivityState, SessionEvent, SessionEventType, SessionHeadDelta,
    SessionHeadSnapshot, SessionMetadata, SessionSnapshotSummary, SessionSummaryDelta, SessionTurn,
    Task, WorkspaceActiveTaskSummary,
};
#[cfg(test)]
use replay_state::SessionReplayState;
#[cfg(test)]
use trim::compact_active_head_snapshot;

mod active_tasks;
mod cache;
mod delta;
mod entry;
mod projection;
mod replay_state;
mod session_heads;
mod stats;
mod subscriptions;
mod trim;
mod workspace_events;

pub use cache::{WorkspaceActiveHeadCacheEntry, WorkspaceActiveSnapshotCacheEntry};
pub use replay_state::{
    is_transient_session_delta, SessionReplayCursor, WorkspaceSessionReplay,
    WorkspaceSessionReplayItem,
};
pub use stats::WorkspaceActiveSnapshotStats;
pub use subscriptions::{
    primary_session_id_for_active_task, replay_cursor_after_live_progress, resolve_session_replay,
    resolve_workspace_active_snapshot_subscriptions, workspace_stream_event_blocks_pending_replay,
    ResolvedWorkspaceActiveSessionReplay, ResolvedWorkspaceActiveSessionSubscription,
    ResolvedWorkspaceActiveSubscriptions, WorkspaceActiveSubscriptionSource,
    WorkspaceActiveSubscriptionState,
};
pub use trim::session_metadata_from_session;

pub struct WorkspaceActiveSnapshotHub {
    inner: Mutex<HashMap<WorkspaceId, WorkspaceActiveSnapshotEntry>>,
    session_heads: Mutex<HashMap<SessionId, CachedSessionHead>>,
    active_head_index: Mutex<HashMap<SessionId, WorkspaceId>>,
    session_head_limit: usize,
}

impl WorkspaceActiveSnapshotHub {
    const DEFAULT_SESSION_HEAD_LIMIT: usize = 256;

    pub fn new() -> Self {
        Self::with_session_head_limit(Self::DEFAULT_SESSION_HEAD_LIMIT)
    }

    fn with_session_head_limit(session_head_limit: usize) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            session_heads: Mutex::new(HashMap::new()),
            active_head_index: Mutex::new(HashMap::new()),
            session_head_limit: session_head_limit.max(1),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_with_session_head_limit(session_head_limit: usize) -> Self {
        Self::with_session_head_limit(session_head_limit)
    }

    async fn ensure_entry(
        &self,
        workspace_id: WorkspaceId,
    ) -> broadcast::Sender<WorkspaceActiveSnapshotEvent> {
        let mut guard = self.inner.lock().await;
        guard
            .entry(workspace_id)
            .or_insert_with(WorkspaceActiveSnapshotEntry::new)
            .tx
            .clone()
    }

    pub async fn subscribe(
        &self,
        workspace_id: WorkspaceId,
    ) -> broadcast::Receiver<WorkspaceActiveSnapshotEvent> {
        self.ensure_entry(workspace_id).await.subscribe()
    }

    pub async fn snapshot_state(&self, workspace_id: WorkspaceId) -> (i64, i64) {
        let guard = self.inner.lock().await;
        guard
            .get(&workspace_id)
            .map(|entry| (entry.snapshot_rev, entry.archived_rev))
            .unwrap_or((0, 0))
    }

    pub async fn session_last_event_seq(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
    ) -> i64 {
        let mut guard = self.inner.lock().await;
        let entry = guard
            .entry(workspace_id)
            .or_insert_with(WorkspaceActiveSnapshotEntry::new);
        entry.session_last_event_seq(session_id)
    }

    pub async fn session_replay_cursor(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
    ) -> SessionReplayCursor {
        let mut guard = self.inner.lock().await;
        let entry = guard
            .entry(workspace_id)
            .or_insert_with(WorkspaceActiveSnapshotEntry::new);
        entry.session_replay_cursor(session_id)
    }

    pub async fn replay_session_head_deltas(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
        after_seq: i64,
        after_projection_rev: i64,
        limit: usize,
    ) -> SessionReplayResult {
        let mut guard = self.inner.lock().await;
        let entry = guard
            .entry(workspace_id)
            .or_insert_with(WorkspaceActiveSnapshotEntry::new);
        entry.replay_session(session_id, after_seq, after_projection_rev, limit)
    }

    pub async fn replay_session_stream(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
        after_seq: i64,
        after_projection_rev: i64,
        limit: usize,
    ) -> WorkspaceSessionReplay {
        let replay = self
            .replay_session_head_deltas(
                workspace_id,
                session_id,
                after_seq,
                after_projection_rev,
                limit,
            )
            .await;
        match replay {
            SessionReplayResult::Replay { deltas, last_sent } => WorkspaceSessionReplay::Replay {
                items: deltas
                    .into_iter()
                    .map(|delta| WorkspaceSessionReplayItem::Delta(Box::new(delta)))
                    .collect(),
                last_sent,
            },
            SessionReplayResult::Gap {
                last_known_seq,
                reason,
            } => {
                if after_seq <= 0 {
                    if let Some(head) = self.get_session_head(session_id).await {
                        let last_sent = SessionReplayCursor::from_head(&head);
                        return WorkspaceSessionReplay::Replay {
                            items: vec![WorkspaceSessionReplayItem::Seed(Box::new(head))],
                            last_sent,
                        };
                    }
                    let last_sent = self.session_replay_cursor(workspace_id, session_id).await;
                    return WorkspaceSessionReplay::Replay {
                        items: Vec::new(),
                        last_sent: SessionReplayCursor {
                            last_event_seq: last_sent
                                .last_event_seq
                                .max(last_known_seq.max(after_seq)),
                            projection_rev: last_sent
                                .projection_rev
                                .max(after_projection_rev.max(0)),
                        },
                    };
                }
                let mut items = vec![WorkspaceSessionReplayItem::Gap {
                    session_id,
                    after_seq,
                    reason,
                }];
                let cursor = self.session_replay_cursor(workspace_id, session_id).await;
                let mut last_sent = SessionReplayCursor {
                    last_event_seq: cursor.last_event_seq.max(last_known_seq.max(after_seq)),
                    projection_rev: cursor.projection_rev.max(after_projection_rev.max(0)),
                };
                if let Some(head) = self.get_session_head(session_id).await {
                    last_sent = last_sent.cover(SessionReplayCursor::from_head(&head));
                    items.push(WorkspaceSessionReplayItem::Seed(Box::new(head)));
                }
                WorkspaceSessionReplay::Replay { items, last_sent }
            }
            SessionReplayResult::ResetRequired => WorkspaceSessionReplay::ResetRequired,
        }
    }
}

impl Default for WorkspaceActiveSnapshotHub {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
