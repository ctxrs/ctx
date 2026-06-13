use ctx_core::ids::{SessionId, TaskId, WorkspaceId};
use ctx_core::models::{
    SessionSnapshotSummary, SessionSummaryDelta, Task, TaskDelta, TaskDeltaKind,
    WorkspaceActiveSnapshotEvent, WorkspaceActiveTaskSummary,
};

use crate::delta::apply_session_summary_delta;
use crate::entry::WorkspaceActiveSnapshotEntry;
use crate::trim::compact_active_head_snapshot;
use crate::{SessionReplayCursor, WorkspaceActiveSnapshotHub};

fn attach_cached_primary_session_head(
    entry: &WorkspaceActiveSnapshotEntry,
    task: &mut WorkspaceActiveTaskSummary,
) {
    let primary_session_id = task
        .task
        .primary_session_id
        .unwrap_or(task.primary_session.session.id);
    if let Some(head) = entry.active_heads.get(&primary_session_id) {
        task.primary_session_head = Some(head.clone());
    }
}

impl WorkspaceActiveSnapshotHub {
    pub async fn publish_active_task_upsert(
        &self,
        workspace_id: WorkspaceId,
        task: WorkspaceActiveTaskSummary,
    ) {
        let mut cached = task;
        let (tx, snapshot_rev, previous_primary_session_id) = {
            let mut guard = self.inner.lock().await;
            let entry = guard
                .entry(workspace_id)
                .or_insert_with(WorkspaceActiveSnapshotEntry::new);
            attach_cached_primary_session_head(entry, &mut cached);
            entry.snapshot_rev += 1;
            let previous_primary_session_id = entry
                .active_tasks
                .get(&cached.task.id)
                .map(WorkspaceActiveSnapshotEntry::primary_session_id_for_task);
            if let Some(previous_session_id) = previous_primary_session_id {
                if previous_session_id
                    != WorkspaceActiveSnapshotEntry::primary_session_id_for_task(&cached)
                {
                    entry.active_heads.remove(&previous_session_id);
                    entry.session_replay.remove(&previous_session_id);
                }
            }
            if let Some(head) = cached.primary_session_head.as_ref() {
                entry
                    .active_heads
                    .insert(head.session.id, compact_active_head_snapshot(head));
                entry.seed_session_replay(head.session.id, SessionReplayCursor::from_head(head));
            }
            entry.active_tasks.insert(cached.task.id, cached.clone());
            (
                entry.tx.clone(),
                entry.snapshot_rev,
                previous_primary_session_id,
            )
        };
        {
            let mut index = self.active_head_index.lock().await;
            if let Some(previous_session_id) = previous_primary_session_id {
                let next_primary_session_id =
                    WorkspaceActiveSnapshotEntry::primary_session_id_for_task(&cached);
                if previous_session_id != next_primary_session_id {
                    index.remove(&previous_session_id);
                }
            }
            if let Some(head) = cached.primary_session_head.as_ref() {
                index.insert(head.session.id, workspace_id);
            }
        }
        self.prune_non_active_session_heads().await;
        let _ = tx.send(WorkspaceActiveSnapshotEvent::ActiveTaskUpsert {
            workspace_id,
            snapshot_rev,
            task: Box::new(cached),
        });
    }

    pub async fn publish_active_task_delete(&self, workspace_id: WorkspaceId, task_id: TaskId) {
        let (tx, snapshot_rev, removed_primary_session_id) = {
            let mut guard = self.inner.lock().await;
            let entry = guard
                .entry(workspace_id)
                .or_insert_with(WorkspaceActiveSnapshotEntry::new);
            entry.snapshot_rev += 1;
            let removed_primary_session_id = entry.remove_active_task_state(task_id);
            (
                entry.tx.clone(),
                entry.snapshot_rev,
                removed_primary_session_id,
            )
        };
        if let Some(session_id) = removed_primary_session_id {
            let mut index = self.active_head_index.lock().await;
            index.remove(&session_id);
            drop(index);
            self.prune_non_active_session_heads().await;
        }
        let _ = tx.send(WorkspaceActiveSnapshotEvent::ActiveTaskDelete {
            workspace_id,
            snapshot_rev,
            task_id,
        });
    }

    pub async fn publish_task_delta(
        &self,
        workspace_id: WorkspaceId,
        task: Task,
        kind: TaskDeltaKind,
    ) -> bool {
        let task_id = task.id;
        let delta_task = task.clone();
        let delta_kind = kind.clone();
        let (tx, snapshot_rev, changed, removed_primary_session_id) = {
            let mut guard = self.inner.lock().await;
            let entry = guard
                .entry(workspace_id)
                .or_insert_with(WorkspaceActiveSnapshotEntry::new);
            let mut changed = false;
            let mut removed_primary_session_id = None;
            match kind {
                TaskDeltaKind::Archived => {
                    if let Some(session_id) = entry.remove_active_task_state(task_id) {
                        changed = true;
                        removed_primary_session_id = Some(session_id);
                    }
                }
                TaskDeltaKind::Updated | TaskDeltaKind::Unarchived => {
                    if let Some(active_task) = entry.active_tasks.get_mut(&task_id) {
                        active_task.task = task.clone();
                        changed = true;
                    }
                }
            }
            if changed {
                entry.snapshot_rev += 1;
            }
            (
                entry.tx.clone(),
                entry.snapshot_rev,
                changed,
                removed_primary_session_id,
            )
        };
        if !changed {
            return false;
        }
        if let Some(session_id) = removed_primary_session_id {
            let mut index = self.active_head_index.lock().await;
            index.remove(&session_id);
            drop(index);
            self.prune_non_active_session_heads().await;
        }
        let _ = tx.send(WorkspaceActiveSnapshotEvent::TaskDelta {
            workspace_id,
            snapshot_rev,
            delta: Box::new(TaskDelta {
                task: delta_task,
                kind: delta_kind,
            }),
        });
        true
    }

    pub async fn publish_session_summary_delta(
        &self,
        workspace_id: WorkspaceId,
        delta: SessionSummaryDelta,
    ) -> bool {
        let mut delta = delta;
        if delta.emitted_at_ms.is_none() {
            delta.emitted_at_ms = Some(Self::now_ms());
        }
        let session_id = delta.session_id;
        let task_id = delta.task_id;
        let delta_for_event = delta.clone();
        let (tx, snapshot_rev, changed) = {
            let mut guard = self.inner.lock().await;
            let entry = guard
                .entry(workspace_id)
                .or_insert_with(WorkspaceActiveSnapshotEntry::new);
            let mut changed = false;
            if let Some(active_task) = entry.active_tasks.get_mut(&task_id) {
                if active_task.primary_session.session.id == session_id
                    && apply_session_summary_delta(&mut active_task.primary_session, &delta)
                {
                    changed = true;
                }
                if let Some(idx) = active_task
                    .sessions
                    .iter()
                    .position(|session| session.session.id == session_id)
                {
                    if apply_session_summary_delta(&mut active_task.sessions[idx], &delta) {
                        changed = true;
                    }
                }
            }
            if changed {
                entry.snapshot_rev += 1;
            }
            (entry.tx.clone(), entry.snapshot_rev, changed)
        };
        if !changed {
            return false;
        }
        let _ = tx.send(WorkspaceActiveSnapshotEvent::SessionSummaryDelta {
            workspace_id,
            snapshot_rev,
            delta: Box::new(delta_for_event),
        });
        true
    }

    pub async fn publish_session_summary(
        &self,
        workspace_id: WorkspaceId,
        summary: SessionSnapshotSummary,
    ) {
        let session_id = summary.session.id;
        let task_id = summary.session.task_id;
        let summary_for_task = summary.clone();
        let (tx, snapshot_rev, task_update) = {
            let mut guard = self.inner.lock().await;
            let entry = guard
                .entry(workspace_id)
                .or_insert_with(WorkspaceActiveSnapshotEntry::new);
            entry.snapshot_rev += 1;
            let mut update = None;
            if let Some(active_task) = entry.active_tasks.get_mut(&task_id) {
                let mut changed = false;
                if active_task.primary_session.session.id == session_id {
                    active_task.primary_session = summary_for_task.clone();
                    changed = true;
                }
                if let Some(idx) = active_task
                    .sessions
                    .iter()
                    .position(|session| session.session.id == session_id)
                {
                    active_task.sessions[idx] = summary_for_task;
                    changed = true;
                }
                if changed {
                    update = Some(active_task.clone());
                }
            }
            (entry.tx.clone(), entry.snapshot_rev, update)
        };
        if let Some(task) = task_update {
            let _ = tx.send(WorkspaceActiveSnapshotEvent::ActiveTaskUpsert {
                workspace_id,
                snapshot_rev,
                task: Box::new(task),
            });
        }
        let _ = tx.send(WorkspaceActiveSnapshotEvent::SessionSummary {
            workspace_id,
            snapshot_rev,
            summary: Box::new(summary),
        });
    }

    pub async fn remove_subagent_session_from_active_task(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
        session_id: SessionId,
    ) -> bool {
        let (tx, snapshot_rev, task_update) = {
            let mut guard = self.inner.lock().await;
            let entry = guard
                .entry(workspace_id)
                .or_insert_with(WorkspaceActiveSnapshotEntry::new);
            let mut task_update = None;
            if let Some(active_task) = entry.active_tasks.get_mut(&task_id) {
                let before_len = active_task.sessions.len();
                active_task
                    .sessions
                    .retain(|summary| summary.session.id != session_id);
                if active_task.sessions.len() != before_len {
                    entry.snapshot_rev += 1;
                    task_update = Some(active_task.clone());
                }
            }
            (entry.tx.clone(), entry.snapshot_rev, task_update)
        };
        let Some(task) = task_update else {
            return false;
        };
        let _ = tx.send(WorkspaceActiveSnapshotEvent::ActiveTaskUpsert {
            workspace_id,
            snapshot_rev,
            task: Box::new(task),
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use ctx_core::ids::{MessageId, TurnId, WorktreeId};
    use ctx_core::models::{
        ExecutionEnvironment, Message, MessageDelivery, MessageRole, Session, SessionActivityState,
        SessionHeadDelta, SessionSnapshotSummary, SessionStatus, SessionTurn, SessionTurnStatus,
        Task, TaskStatus, WorkspaceActiveSnapshotEvent,
    };

    fn test_session(workspace_id: WorkspaceId, task_id: TaskId, session_id: SessionId) -> Session {
        let now = Utc.timestamp_opt(0, 0).unwrap();
        Session {
            id: session_id,
            task_id,
            workspace_id,
            worktree_id: WorktreeId::new(),
            execution_environment: ExecutionEnvironment::Host,
            parent_session_id: None,
            relationship: None,
            provider_id: "fake".to_string(),
            model_id: "fake-model".to_string(),
            reasoning_effort: None,
            title: "session".to_string(),
            agent_role: "assistant".to_string(),
            status: SessionStatus::Active,
            provider_session_ref: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn active_task_summary(session: &Session) -> WorkspaceActiveTaskSummary {
        let now = Utc.timestamp_opt(0, 0).unwrap();
        let summary = SessionSnapshotSummary {
            session: crate::session_metadata_from_session(session),
            last_message_at: None,
            last_message_preview: None,
            last_event_seq: None,
            projection_rev: 0,
            state_rev: 0,
            activity: SessionActivityState::default(),
            unread: None,
        };
        WorkspaceActiveTaskSummary {
            task: Task {
                id: session.task_id,
                workspace_id: session.workspace_id,
                title: "task".to_string(),
                description: None,
                status: TaskStatus::Running,
                created_at: now,
                updated_at: now,
                exec_plan_id: None,
                primary_session_id: Some(session.id),
                primary_worktree_id: Some(session.worktree_id),
                archived_at: None,
                assistant_seen_at: None,
                last_activity_at: None,
                last_assistant_message_at: None,
                has_active_session: true,
            },
            primary_session: summary.clone(),
            primary_session_head: None,
            sessions: vec![summary],
            sort_at: now,
        }
    }

    fn assistant_message_delta(session: &Session) -> SessionHeadDelta {
        let now = Utc.timestamp_opt(0, 0).unwrap();
        let turn_id = TurnId::new();
        SessionHeadDelta {
            session_id: session.id,
            last_event_seq: 10,
            projection_rev: 10,
            state_rev: 10,
            emitted_at_ms: None,
            session: None,
            activity: None,
            event: None,
            turn: Some(SessionTurn {
                turn_id,
                session_id: session.id,
                run_id: None,
                user_message_id: None,
                status: SessionTurnStatus::Completed,
                start_seq: Some(10),
                end_seq: Some(10),
                started_at: now,
                updated_at: now,
                assistant_partial: None,
                thought_partial: None,
                metrics_json: None,
                failure: None,
                tool_total: 0,
                tool_pending: 0,
                tool_running: 0,
                tool_completed: 0,
                tool_failed: 0,
            }),
            message: Some(Message {
                id: MessageId::new(),
                session_id: session.id,
                task_id: session.task_id,
                run_id: None,
                turn_id: Some(turn_id),
                turn_sequence: Some(1),
                order_seq: Some(2),
                role: MessageRole::Assistant,
                content: "done: hello".to_string(),
                attachments: Vec::new(),
                delivery: MessageDelivery::Immediate,
                delivered_at: Some(now),
                created_at: now,
            }),
            tool_summaries: Vec::new(),
        }
    }

    #[tokio::test]
    async fn active_task_upsert_attaches_head_deltas_that_arrived_before_task_visibility() {
        let hub = WorkspaceActiveSnapshotHub::new();
        let workspace_id = WorkspaceId::new();
        let task_id = TaskId::new();
        let session_id = SessionId::new();
        let session = test_session(workspace_id, task_id, session_id);
        let mut rx = hub.subscribe(workspace_id).await;

        hub.publish_session_head_delta(
            workspace_id,
            &session,
            assistant_message_delta(&session),
            true,
        )
        .await;
        let _ = rx.recv().await.expect("head delta should publish");

        hub.publish_active_task_upsert(workspace_id, active_task_summary(&session))
            .await;

        let event = rx.recv().await.expect("active task upsert should publish");
        let WorkspaceActiveSnapshotEvent::ActiveTaskUpsert { task, .. } = event else {
            panic!("expected active task upsert");
        };
        let head = task
            .primary_session_head
            .as_ref()
            .expect("upsert should carry cached primary session head");
        assert_eq!(head.session.id, session_id);
        assert!(head
            .messages
            .iter()
            .any(|message| message.content == "done: hello"));

        let snapshot = hub.active_snapshot(workspace_id, 10).await;
        let snapshot_head = snapshot.active.tasks[0]
            .primary_session_head
            .as_ref()
            .expect("stored active task should retain cached primary session head");
        assert!(snapshot_head
            .messages
            .iter()
            .any(|message| message.content == "done: hello"));
    }
}
