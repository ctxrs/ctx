use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use ctx_core::ids::{SessionId, TaskId, WorkspaceId};
use ctx_core::models::{
    SessionHeadSnapshot, WorkspaceActiveHeadBatch, WorkspaceActivePage, WorkspaceActiveSnapshot,
    WorkspaceActiveTaskSummary,
};

use crate::entry::WorkspaceActiveSnapshotEntry;
use crate::trim::compact_active_head_snapshot;
use crate::{SessionReplayCursor, WorkspaceActiveSnapshotHub};

impl WorkspaceActiveSnapshotHub {
    pub async fn active_snapshot(
        &self,
        workspace_id: WorkspaceId,
        limit: i64,
    ) -> WorkspaceActiveSnapshot {
        let limit = limit.clamp(1, 200) as usize;
        let (snapshot_rev, archived_rev, total_count, mut tasks) = {
            let guard = self.inner.lock().await;
            match guard.get(&workspace_id) {
                Some(entry) => (
                    entry.snapshot_rev,
                    entry.archived_rev,
                    entry.active_tasks.len() as i64,
                    entry.active_tasks.values().cloned().collect::<Vec<_>>(),
                ),
                None => (0, 0, 0, Vec::new()),
            }
        };
        tasks.sort_by(|a, b| {
            let ord = b.task.created_at.cmp(&a.task.created_at);
            if ord == Ordering::Equal {
                b.task.id.0.cmp(&a.task.id.0)
            } else {
                ord
            }
        });
        if tasks.len() > limit {
            tasks.truncate(limit);
        }
        WorkspaceActiveSnapshot {
            workspace_id,
            snapshot_rev,
            archived_rev,
            active: WorkspaceActivePage { tasks, total_count },
        }
    }

    pub async fn active_task_summary(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
    ) -> Option<WorkspaceActiveTaskSummary> {
        let guard = self.inner.lock().await;
        guard
            .get(&workspace_id)
            .and_then(|entry| entry.active_tasks.get(&task_id).cloned())
    }

    pub async fn active_heads(&self, workspace_id: WorkspaceId) -> WorkspaceActiveHeadBatch {
        let (snapshot_rev, mut heads) = {
            let guard = self.inner.lock().await;
            match guard.get(&workspace_id) {
                Some(entry) => (
                    entry.snapshot_rev,
                    entry.active_heads.values().cloned().collect::<Vec<_>>(),
                ),
                None => (0, Vec::new()),
            }
        };
        heads.sort_by(|a, b| {
            let ord = a.session.created_at.cmp(&b.session.created_at);
            if ord == Ordering::Equal {
                a.session.id.0.cmp(&b.session.id.0)
            } else {
                ord
            }
        });
        WorkspaceActiveHeadBatch {
            workspace_id,
            snapshot_rev,
            heads,
        }
    }

    pub async fn needs_hydration(&self, workspace_id: WorkspaceId) -> bool {
        let mut guard = self.inner.lock().await;
        let entry = guard
            .entry(workspace_id)
            .or_insert_with(WorkspaceActiveSnapshotEntry::new);
        !entry.hydrated
    }

    pub async fn hydrate_snapshot(
        &self,
        workspace_id: WorkspaceId,
        snapshot_rev: i64,
        archived_rev: i64,
        tasks: Vec<WorkspaceActiveTaskSummary>,
        heads: Vec<SessionHeadSnapshot>,
    ) {
        let mut tasks_by_id = HashMap::with_capacity(tasks.len());
        for task in tasks {
            tasks_by_id.insert(task.task.id, task);
        }

        let mut heads_by_id = HashMap::with_capacity(heads.len());
        for head in &heads {
            // Store a compact head for the workspace active snapshot surface to keep
            // WS payloads bounded. The full head remains available via session endpoints.
            heads_by_id.insert(head.session.id, compact_active_head_snapshot(head));
        }

        {
            let mut guard = self.inner.lock().await;
            let entry = guard
                .entry(workspace_id)
                .or_insert_with(WorkspaceActiveSnapshotEntry::new);
            if entry.hydrated {
                return;
            }
            entry.hydrated = true;
            entry.snapshot_rev = entry.snapshot_rev.max(snapshot_rev);
            entry.archived_rev = entry.archived_rev.max(archived_rev);
            entry.active_tasks = tasks_by_id;
            entry.active_heads = heads_by_id;
            let active_session_ids: HashSet<SessionId> =
                entry.active_heads.keys().cloned().collect();
            if !active_session_ids.is_empty() {
                entry
                    .session_replay
                    .retain(|session_id, _| active_session_ids.contains(session_id));
            }
            let replay_seeds: Vec<(SessionId, SessionReplayCursor)> = entry
                .active_heads
                .values()
                .map(|head| (head.session.id, SessionReplayCursor::from_head(head)))
                .collect();
            for (session_id, cursor) in replay_seeds {
                entry.seed_session_replay(session_id, cursor);
            }
        }

        self.seed_cached_session_heads(workspace_id, &heads).await;
    }
}
