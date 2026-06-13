use std::collections::{HashMap, HashSet};
use std::future::Future;

use ctx_core::ids::{SessionId, TaskId, WorkspaceId};
use ctx_core::models::{
    WorkspaceActiveSnapshotClientMessage, WorkspaceActiveSnapshotEvent,
    WorkspaceActiveSnapshotSessionIntent, WorkspaceActiveSnapshotSessionReplay,
    WorkspaceActiveSnapshotSubscribeScope, WorkspaceActiveTaskSummary,
};

use crate::SessionReplayCursor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvedWorkspaceActiveSessionReplay {
    Reset,
    Resume {
        after_seq: i64,
        after_projection_rev: i64,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct ResolvedWorkspaceActiveSessionSubscription {
    pub session_id: SessionId,
    pub intent: WorkspaceActiveSnapshotSessionIntent,
    pub replay: ResolvedWorkspaceActiveSessionReplay,
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceActiveSubscriptionState {
    pub active_scope: bool,
    pub explicit_sessions: HashSet<SessionId>,
    pub replay_sessions: HashSet<SessionId>,
    pub active_task_sessions: HashMap<TaskId, SessionId>,
    pub foreground_session_ids: Option<HashSet<SessionId>>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ResolvedWorkspaceActiveSubscriptions {
    pub sessions: Vec<ResolvedWorkspaceActiveSessionSubscription>,
    pub state: WorkspaceActiveSubscriptionState,
}

pub trait WorkspaceActiveSubscriptionSource {
    fn session_belongs_to_workspace(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
    ) -> impl Future<Output = bool> + Send;

    fn active_tasks(
        &self,
        workspace_id: WorkspaceId,
    ) -> impl Future<Output = Vec<WorkspaceActiveTaskSummary>> + Send;

    fn primary_session_id_for_task(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
    ) -> impl Future<Output = Result<Option<SessionId>, ()>> + Send;

    fn session_replay_cursor(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
    ) -> impl Future<Output = SessionReplayCursor> + Send;
}

pub fn primary_session_id_for_active_task(task: &WorkspaceActiveTaskSummary) -> SessionId {
    task.task
        .primary_session_id
        .unwrap_or(task.primary_session.session.id)
}

pub fn replay_cursor_after_live_progress(
    live_cursor: Option<SessionReplayCursor>,
    requested_cursor: SessionReplayCursor,
) -> Option<SessionReplayCursor> {
    live_cursor.map(|cursor| cursor.cover(requested_cursor))
}

pub fn workspace_stream_event_blocks_pending_replay(
    event: &WorkspaceActiveSnapshotEvent,
    pending_replay_sessions: &HashSet<SessionId>,
    active_task_sessions: &HashMap<TaskId, SessionId>,
) -> bool {
    match event {
        WorkspaceActiveSnapshotEvent::ActiveTaskUpsert { task, .. } => {
            pending_replay_sessions.contains(&primary_session_id_for_active_task(task))
        }
        WorkspaceActiveSnapshotEvent::ActiveTaskDelete { task_id, .. } => active_task_sessions
            .get(task_id)
            .is_some_and(|session_id| pending_replay_sessions.contains(session_id)),
        WorkspaceActiveSnapshotEvent::TaskDelta { delta, .. } => delta
            .task
            .primary_session_id
            .is_some_and(|session_id| pending_replay_sessions.contains(&session_id)),
        WorkspaceActiveSnapshotEvent::SessionSummary { summary, .. } => {
            pending_replay_sessions.contains(&summary.session.id)
        }
        WorkspaceActiveSnapshotEvent::SessionSummaryDelta { delta, .. } => {
            pending_replay_sessions.contains(&delta.session_id)
        }
        WorkspaceActiveSnapshotEvent::SessionRemoved { session_id, .. }
        | WorkspaceActiveSnapshotEvent::SessionGap { session_id, .. } => {
            pending_replay_sessions.contains(session_id)
        }
        WorkspaceActiveSnapshotEvent::SessionHeadDelta { delta, .. } => {
            pending_replay_sessions.contains(&delta.session_id)
        }
        WorkspaceActiveSnapshotEvent::SessionHeadSeed { head, .. } => {
            pending_replay_sessions.contains(&head.session.id)
        }
        WorkspaceActiveSnapshotEvent::Ready { .. }
        | WorkspaceActiveSnapshotEvent::WorktreeBootstrap { .. }
        | WorkspaceActiveSnapshotEvent::ArchivedTaskUpsert { .. }
        | WorkspaceActiveSnapshotEvent::ArchivedTaskDelete { .. } => false,
    }
}

pub async fn resolve_workspace_active_snapshot_subscriptions<S>(
    source: &S,
    workspace_id: WorkspaceId,
    message: WorkspaceActiveSnapshotClientMessage,
    existing: &HashMap<SessionId, SessionReplayCursor>,
) -> Result<ResolvedWorkspaceActiveSubscriptions, ()>
where
    S: WorkspaceActiveSubscriptionSource + Sync,
{
    let WorkspaceActiveSnapshotClientMessage::Subscribe {
        session_ids,
        sessions,
        task_ids,
        foreground_session_id,
        scope,
        ..
    } = message;

    let mut resolved = HashSet::new();
    let mut replay_map: HashMap<SessionId, WorkspaceActiveSnapshotSessionReplay> = HashMap::new();
    let mut intent_map: HashMap<SessionId, WorkspaceActiveSnapshotSessionIntent> = HashMap::new();
    let mut explicit_sessions = HashSet::new();
    let mut active_task_sessions = HashMap::new();
    let mut active_scope = false;
    let mut foreground_session_ids = None;

    for sub in sessions {
        if !source
            .session_belongs_to_workspace(workspace_id, sub.session_id)
            .await
        {
            continue;
        }
        replay_map.insert(sub.session_id, sub.replay);
        intent_map.insert(
            sub.session_id,
            merge_session_intent(
                intent_map.get(&sub.session_id).copied(),
                sub.intent
                    .unwrap_or(WorkspaceActiveSnapshotSessionIntent::Replay),
            ),
        );
        resolved.insert(sub.session_id);
        explicit_sessions.insert(sub.session_id);
    }
    for session_id in session_ids {
        if !source
            .session_belongs_to_workspace(workspace_id, session_id)
            .await
        {
            continue;
        }
        resolved.insert(session_id);
        intent_map.insert(
            session_id,
            merge_session_intent(
                intent_map.get(&session_id).copied(),
                WorkspaceActiveSnapshotSessionIntent::Replay,
            ),
        );
        explicit_sessions.insert(session_id);
    }
    if matches!(scope, Some(WorkspaceActiveSnapshotSubscribeScope::Active)) {
        active_scope = true;
        for task in source.active_tasks(workspace_id).await {
            let session_id = primary_session_id_for_active_task(&task);
            resolved.insert(session_id);
            intent_map
                .entry(session_id)
                .or_insert(WorkspaceActiveSnapshotSessionIntent::Head);
            active_task_sessions.insert(task.task.id, session_id);
        }
    }
    for task_id in task_ids {
        if let Some(primary_session_id) = source
            .primary_session_id_for_task(workspace_id, task_id)
            .await?
        {
            resolved.insert(primary_session_id);
            intent_map.insert(
                primary_session_id,
                merge_session_intent(
                    intent_map.get(&primary_session_id).copied(),
                    WorkspaceActiveSnapshotSessionIntent::Replay,
                ),
            );
            explicit_sessions.insert(primary_session_id);
        }
    }
    if let Some(session_id) = foreground_session_id {
        if source
            .session_belongs_to_workspace(workspace_id, session_id)
            .await
        {
            let mut sessions = HashSet::new();
            sessions.insert(session_id);
            foreground_session_ids = Some(sessions);
            resolved.insert(session_id);
            explicit_sessions.insert(session_id);
            intent_map.insert(
                session_id,
                merge_session_intent(
                    intent_map.get(&session_id).copied(),
                    WorkspaceActiveSnapshotSessionIntent::Replay,
                ),
            );
        }
    }

    let mut next = Vec::with_capacity(resolved.len());
    for session_id in resolved {
        let replay = replay_map.get(&session_id);
        let intent = intent_map
            .get(&session_id)
            .copied()
            .unwrap_or(WorkspaceActiveSnapshotSessionIntent::Replay);
        let existing_last_sent = existing.get(&session_id).copied();
        let current_tail = if matches!(
            replay,
            Some(WorkspaceActiveSnapshotSessionReplay::Auto) | None
        ) && existing_last_sent.is_none()
        {
            source.session_replay_cursor(workspace_id, session_id).await
        } else {
            SessionReplayCursor::default()
        };
        let replay = resolve_session_replay(replay, existing_last_sent, current_tail);
        next.push(ResolvedWorkspaceActiveSessionSubscription {
            session_id,
            intent,
            replay,
        });
    }
    let active_primary_session_ids = active_task_sessions
        .values()
        .copied()
        .collect::<HashSet<_>>();
    next.sort_by_key(|subscription| {
        let session_id = subscription.session_id;
        let replay_rank = if foreground_session_ids
            .as_ref()
            .is_some_and(|foreground| foreground.contains(&session_id))
        {
            0
        } else if active_primary_session_ids.contains(&session_id) {
            1
        } else {
            2
        };
        (replay_rank, session_id.0)
    });
    let subscription_state = WorkspaceActiveSubscriptionState {
        active_scope,
        explicit_sessions,
        replay_sessions: next
            .iter()
            .filter_map(|subscription| {
                (subscription.intent == WorkspaceActiveSnapshotSessionIntent::Replay)
                    .then_some(subscription.session_id)
            })
            .collect(),
        active_task_sessions,
        foreground_session_ids,
    };
    Ok(ResolvedWorkspaceActiveSubscriptions {
        sessions: next,
        state: subscription_state,
    })
}

fn merge_session_intent(
    previous: Option<WorkspaceActiveSnapshotSessionIntent>,
    next: WorkspaceActiveSnapshotSessionIntent,
) -> WorkspaceActiveSnapshotSessionIntent {
    match (previous, next) {
        (Some(WorkspaceActiveSnapshotSessionIntent::Replay), _)
        | (_, WorkspaceActiveSnapshotSessionIntent::Replay) => {
            WorkspaceActiveSnapshotSessionIntent::Replay
        }
        _ => WorkspaceActiveSnapshotSessionIntent::Head,
    }
}

pub fn resolve_session_replay(
    replay: Option<&WorkspaceActiveSnapshotSessionReplay>,
    existing_last_sent: Option<SessionReplayCursor>,
    current_tail: SessionReplayCursor,
) -> ResolvedWorkspaceActiveSessionReplay {
    match replay {
        Some(WorkspaceActiveSnapshotSessionReplay::Resume {
            after_seq,
            after_projection_rev,
        }) => ResolvedWorkspaceActiveSessionReplay::Resume {
            after_seq: *after_seq,
            after_projection_rev: *after_projection_rev,
        },
        Some(WorkspaceActiveSnapshotSessionReplay::Reset) => {
            ResolvedWorkspaceActiveSessionReplay::Reset
        }
        Some(WorkspaceActiveSnapshotSessionReplay::Auto) | None => {
            let cursor = existing_last_sent.unwrap_or(current_tail);
            ResolvedWorkspaceActiveSessionReplay::Resume {
                after_seq: cursor.last_event_seq,
                after_projection_rev: cursor.projection_rev,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use ctx_core::ids::WorktreeId;
    use ctx_core::models::{
        ExecutionEnvironment, SessionActivityState, SessionSnapshotSummary, SessionStatus, Task,
        TaskStatus,
    };
    use uuid::Uuid;

    fn deterministic_session_id(value: u128) -> SessionId {
        SessionId(Uuid::from_u128(value))
    }

    fn active_task_summary(
        workspace_id: WorkspaceId,
        task_id: TaskId,
        session_id: SessionId,
    ) -> WorkspaceActiveTaskSummary {
        let now = Utc.timestamp_opt(0, 0).unwrap();
        let worktree_id = WorktreeId::new();
        let session = ctx_core::models::SessionMetadata {
            id: session_id,
            task_id,
            workspace_id,
            worktree_id,
            execution_environment: ExecutionEnvironment::Host,
            parent_session_id: None,
            relationship: None,
            provider_id: "test".to_string(),
            model_id: "test-model".to_string(),
            reasoning_effort: None,
            title: "session".to_string(),
            agent_role: "assistant".to_string(),
            status: SessionStatus::Active,
            provider_session_ref: None,
            created_at: now,
            updated_at: now,
        };
        let session_summary = SessionSnapshotSummary {
            session,
            last_message_at: None,
            last_message_preview: None,
            last_event_seq: Some(0),
            projection_rev: 0,
            state_rev: 0,
            activity: SessionActivityState::default(),
            unread: None,
        };
        WorkspaceActiveTaskSummary {
            task: Task {
                id: task_id,
                workspace_id,
                title: "task".to_string(),
                description: None,
                status: TaskStatus::Running,
                created_at: now,
                updated_at: now,
                exec_plan_id: None,
                primary_session_id: Some(session_id),
                primary_worktree_id: Some(worktree_id),
                archived_at: None,
                assistant_seen_at: None,
                last_activity_at: None,
                last_assistant_message_at: None,
                has_active_session: true,
            },
            primary_session: session_summary.clone(),
            primary_session_head: None,
            sessions: vec![session_summary],
            sort_at: now,
        }
    }

    struct TestSubscriptionSource {
        workspace_id: WorkspaceId,
        sessions: HashSet<SessionId>,
        active_tasks: Vec<WorkspaceActiveTaskSummary>,
        task_sessions: HashMap<TaskId, SessionId>,
    }

    impl WorkspaceActiveSubscriptionSource for TestSubscriptionSource {
        async fn session_belongs_to_workspace(
            &self,
            workspace_id: WorkspaceId,
            session_id: SessionId,
        ) -> bool {
            workspace_id == self.workspace_id && self.sessions.contains(&session_id)
        }

        async fn active_tasks(&self, workspace_id: WorkspaceId) -> Vec<WorkspaceActiveTaskSummary> {
            if workspace_id == self.workspace_id {
                self.active_tasks.clone()
            } else {
                Vec::new()
            }
        }

        async fn primary_session_id_for_task(
            &self,
            workspace_id: WorkspaceId,
            task_id: TaskId,
        ) -> Result<Option<SessionId>, ()> {
            if workspace_id == self.workspace_id {
                Ok(self.task_sessions.get(&task_id).copied())
            } else {
                Ok(None)
            }
        }

        async fn session_replay_cursor(
            &self,
            _workspace_id: WorkspaceId,
            _session_id: SessionId,
        ) -> SessionReplayCursor {
            SessionReplayCursor::default()
        }
    }

    #[test]
    fn replay_resolution_uses_existing_cursor_for_auto() {
        let existing_last_sent = Some(SessionReplayCursor {
            last_event_seq: 7,
            projection_rev: 11,
        });
        let current_tail = SessionReplayCursor {
            last_event_seq: 99,
            projection_rev: 101,
        };

        assert_eq!(
            resolve_session_replay(
                Some(&WorkspaceActiveSnapshotSessionReplay::Auto),
                existing_last_sent,
                current_tail,
            ),
            ResolvedWorkspaceActiveSessionReplay::Resume {
                after_seq: 7,
                after_projection_rev: 11,
            }
        );
        assert_eq!(
            resolve_session_replay(None, existing_last_sent, current_tail),
            ResolvedWorkspaceActiveSessionReplay::Resume {
                after_seq: 7,
                after_projection_rev: 11,
            }
        );
    }

    #[test]
    fn replay_resolution_keeps_reset_explicit_even_with_existing_cursor() {
        assert_eq!(
            resolve_session_replay(
                Some(&WorkspaceActiveSnapshotSessionReplay::Reset),
                Some(SessionReplayCursor {
                    last_event_seq: 7,
                    projection_rev: 11,
                }),
                SessionReplayCursor {
                    last_event_seq: 12,
                    projection_rev: 18,
                },
            ),
            ResolvedWorkspaceActiveSessionReplay::Reset
        );
    }

    #[test]
    fn replay_resolution_uses_explicit_resume_cursor() {
        assert_eq!(
            resolve_session_replay(
                Some(&WorkspaceActiveSnapshotSessionReplay::Resume {
                    after_seq: 21,
                    after_projection_rev: 34,
                }),
                Some(SessionReplayCursor {
                    last_event_seq: 7,
                    projection_rev: 11,
                }),
                SessionReplayCursor {
                    last_event_seq: 12,
                    projection_rev: 18,
                },
            ),
            ResolvedWorkspaceActiveSessionReplay::Resume {
                after_seq: 21,
                after_projection_rev: 34,
            }
        );
    }

    #[test]
    fn replay_resolution_uses_current_tail_for_auto_without_existing_cursor() {
        assert_eq!(
            resolve_session_replay(
                Some(&WorkspaceActiveSnapshotSessionReplay::Auto),
                None,
                SessionReplayCursor {
                    last_event_seq: 12,
                    projection_rev: 18,
                },
            ),
            ResolvedWorkspaceActiveSessionReplay::Resume {
                after_seq: 12,
                after_projection_rev: 18,
            }
        );
        assert_eq!(
            resolve_session_replay(
                None,
                None,
                SessionReplayCursor {
                    last_event_seq: 14,
                    projection_rev: 20,
                },
            ),
            ResolvedWorkspaceActiveSessionReplay::Resume {
                after_seq: 14,
                after_projection_rev: 20,
            }
        );
    }

    #[tokio::test]
    async fn subscription_resolution_replays_foreground_then_active_then_background_sessions() {
        let workspace_id = WorkspaceId::new();
        let background_session_id = deterministic_session_id(1);
        let foreground_session_id = deterministic_session_id(2);
        let active_session_id = deterministic_session_id(3);
        let active_task_id = TaskId::new();
        let source = TestSubscriptionSource {
            workspace_id,
            sessions: [
                background_session_id,
                foreground_session_id,
                active_session_id,
            ]
            .into_iter()
            .collect(),
            active_tasks: vec![active_task_summary(
                workspace_id,
                active_task_id,
                active_session_id,
            )],
            task_sessions: HashMap::new(),
        };

        let resolved = resolve_workspace_active_snapshot_subscriptions(
            &source,
            workspace_id,
            WorkspaceActiveSnapshotClientMessage::Subscribe {
                session_ids: vec![background_session_id, foreground_session_id],
                sessions: Vec::new(),
                task_ids: Vec::new(),
                foreground_session_id: Some(foreground_session_id),
                scope: Some(WorkspaceActiveSnapshotSubscribeScope::Active),
                include_active_heads: false,
            },
            &HashMap::new(),
        )
        .await
        .expect("subscription resolution succeeds");

        let ordered_session_ids = resolved
            .sessions
            .iter()
            .map(|subscription| subscription.session_id)
            .collect::<Vec<_>>();
        let ordered_intents = resolved
            .sessions
            .iter()
            .map(|subscription| subscription.intent)
            .collect::<Vec<_>>();
        assert_eq!(
            ordered_session_ids,
            vec![
                foreground_session_id,
                active_session_id,
                background_session_id,
            ]
        );
        assert_eq!(
            ordered_intents,
            vec![
                WorkspaceActiveSnapshotSessionIntent::Replay,
                WorkspaceActiveSnapshotSessionIntent::Head,
                WorkspaceActiveSnapshotSessionIntent::Replay,
            ]
        );
    }

    fn replay_cursor(last_event_seq: i64, projection_rev: i64) -> SessionReplayCursor {
        SessionReplayCursor {
            last_event_seq,
            projection_rev,
        }
    }

    #[test]
    fn workspace_stream_event_blocks_pending_replay_for_session_events() {
        let pending_session_id = SessionId::new();
        let other_session_id = SessionId::new();
        let pending_task_id = TaskId::new();
        let pending = HashSet::from([pending_session_id]);
        let active_task_sessions = HashMap::from([(pending_task_id, pending_session_id)]);

        assert!(workspace_stream_event_blocks_pending_replay(
            &WorkspaceActiveSnapshotEvent::SessionGap {
                workspace_id: WorkspaceId::new(),
                snapshot_rev: 1,
                session_id: pending_session_id,
                after_seq: 3,
                reason: None,
                seed_follows: false,
            },
            &pending,
            &HashMap::new(),
        ));
        assert!(workspace_stream_event_blocks_pending_replay(
            &WorkspaceActiveSnapshotEvent::ActiveTaskDelete {
                workspace_id: WorkspaceId::new(),
                snapshot_rev: 1,
                task_id: pending_task_id,
            },
            &pending,
            &active_task_sessions,
        ));
        assert!(!workspace_stream_event_blocks_pending_replay(
            &WorkspaceActiveSnapshotEvent::SessionGap {
                workspace_id: WorkspaceId::new(),
                snapshot_rev: 1,
                session_id: other_session_id,
                after_seq: 3,
                reason: None,
                seed_follows: false,
            },
            &pending,
            &HashMap::new(),
        ));
        assert!(!workspace_stream_event_blocks_pending_replay(
            &WorkspaceActiveSnapshotEvent::Ready {
                workspace_id: WorkspaceId::new(),
                snapshot_rev: 1,
                archived_rev: 0,
            },
            &pending,
            &HashMap::new(),
        ));
    }

    #[test]
    fn replay_cursor_after_live_progress_starts_after_live_cursor_and_skips_removed_sessions() {
        assert_eq!(
            replay_cursor_after_live_progress(Some(replay_cursor(15, 16)), replay_cursor(10, 12)),
            Some(replay_cursor(15, 16)),
        );
        assert_eq!(
            replay_cursor_after_live_progress(Some(replay_cursor(15, 16)), replay_cursor(20, 12)),
            Some(replay_cursor(20, 16)),
        );
        assert_eq!(
            replay_cursor_after_live_progress(None, replay_cursor(10, 12)),
            None,
        );
    }
}
