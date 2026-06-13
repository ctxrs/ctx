use std::collections::{HashMap, HashSet};

use ctx_core::ids::{SessionId, TaskId};
use ctx_core::models::{TaskDeltaKind, WorkspaceActiveSnapshotEvent};
use ctx_workspace_active_snapshot::{SessionReplayCursor, WorkspaceActiveSubscriptionState};

use crate::cursor_acceptance::{accept_session_delta_cursor, accept_session_head_cursor};
use crate::event_routing::{
    plan_workspace_stream_event_route, primary_session_id_for_active_task_event,
    WorkspaceStreamEventRoutePlan,
};

use super::planning::{workspace_stream_session_pin_changes, WorkspaceStreamSessionPinChanges};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceStreamSubscriptionEventApplication {
    pub state: WorkspaceActiveSubscriptionState,
    pub subscriptions: HashMap<SessionId, SessionReplayCursor>,
    pub pin_changes: WorkspaceStreamSessionPinChanges,
    pub should_route: bool,
}

#[derive(Debug)]
pub struct WorkspaceStreamLiveEventApplication {
    pub state: WorkspaceActiveSubscriptionState,
    pub subscriptions: HashMap<SessionId, SessionReplayCursor>,
    pub pin_changes: WorkspaceStreamSessionPinChanges,
    pub route_plan: WorkspaceStreamEventRoutePlan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceStreamActiveTaskCursorSeed {
    pub session_id: SessionId,
    pub cursor: SessionReplayCursor,
}

#[derive(Debug)]
pub struct WorkspaceStreamActiveTaskCursorMissing {
    pub state: WorkspaceActiveSubscriptionState,
    pub subscriptions: HashMap<SessionId, SessionReplayCursor>,
    pub session_id: SessionId,
    previous_subscriptions: HashSet<SessionId>,
}

pub fn active_task_cursor_seed_session(
    subscription_state: &WorkspaceActiveSubscriptionState,
    subscriptions: &HashMap<SessionId, SessionReplayCursor>,
    event: &WorkspaceActiveSnapshotEvent,
) -> Option<SessionId> {
    if !subscription_state.active_scope {
        return None;
    }
    let WorkspaceActiveSnapshotEvent::ActiveTaskUpsert { task, .. } = event else {
        return None;
    };
    let session_id = primary_session_id_for_active_task_event(task);
    (!subscriptions.contains_key(&session_id)).then_some(session_id)
}

pub fn apply_workspace_stream_subscription_event(
    mut subscription_state: WorkspaceActiveSubscriptionState,
    mut subscriptions: HashMap<SessionId, SessionReplayCursor>,
    event: &WorkspaceActiveSnapshotEvent,
    active_task_cursor_seed: Option<WorkspaceStreamActiveTaskCursorSeed>,
) -> Result<WorkspaceStreamSubscriptionEventApplication, Box<WorkspaceStreamActiveTaskCursorMissing>>
{
    let previous_subscriptions = subscriptions.keys().copied().collect::<HashSet<_>>();
    if let WorkspaceActiveSnapshotEvent::SessionRemoved { session_id, .. } = event {
        let removed_explicit = subscription_state.explicit_sessions.remove(session_id);
        let removed_foreground = subscription_state
            .foreground_session_ids
            .as_mut()
            .map(|foreground| foreground.remove(session_id))
            .unwrap_or(false);
        if subscription_state
            .foreground_session_ids
            .as_ref()
            .is_some_and(HashSet::is_empty)
        {
            subscription_state.foreground_session_ids = None;
        }
        subscription_state.replay_sessions.remove(session_id);
        let removed_subscription = subscriptions.remove(session_id).is_some();
        return Ok(subscription_event_application(
            subscription_state,
            subscriptions,
            previous_subscriptions,
            removed_explicit || removed_foreground || removed_subscription,
        ));
    }

    if !subscription_state.active_scope {
        return Ok(subscription_event_application(
            subscription_state,
            subscriptions,
            previous_subscriptions,
            true,
        ));
    }

    match event {
        WorkspaceActiveSnapshotEvent::ActiveTaskUpsert { task, .. } => {
            let session_id = primary_session_id_for_active_task_event(task);
            subscription_state
                .active_task_sessions
                .insert(task.task.id, session_id);
            if let std::collections::hash_map::Entry::Vacant(entry) =
                subscriptions.entry(session_id)
            {
                let Some(seed) =
                    active_task_cursor_seed.filter(|seed| seed.session_id == session_id)
                else {
                    return Err(Box::new(WorkspaceStreamActiveTaskCursorMissing {
                        state: subscription_state,
                        subscriptions,
                        session_id,
                        previous_subscriptions,
                    }));
                };
                entry.insert(seed.cursor);
            }
        }
        WorkspaceActiveSnapshotEvent::ActiveTaskDelete { task_id, .. } => {
            remove_active_task_subscription_if_unused(
                &mut subscription_state,
                &mut subscriptions,
                *task_id,
            );
        }
        WorkspaceActiveSnapshotEvent::TaskDelta { delta, .. }
            if matches!(delta.kind, TaskDeltaKind::Archived) =>
        {
            remove_active_task_subscription_if_unused(
                &mut subscription_state,
                &mut subscriptions,
                delta.task.id,
            );
        }
        _ => {}
    }
    Ok(subscription_event_application(
        subscription_state,
        subscriptions,
        previous_subscriptions,
        true,
    ))
}

pub fn apply_missing_active_task_cursor(
    mut missing: Box<WorkspaceStreamActiveTaskCursorMissing>,
    cursor: SessionReplayCursor,
) -> WorkspaceStreamSubscriptionEventApplication {
    missing.subscriptions.insert(missing.session_id, cursor);
    subscription_event_application(
        missing.state,
        missing.subscriptions,
        missing.previous_subscriptions,
        true,
    )
}

pub fn apply_workspace_stream_live_event(
    subscription_state: WorkspaceActiveSubscriptionState,
    subscriptions: HashMap<SessionId, SessionReplayCursor>,
    event: WorkspaceActiveSnapshotEvent,
    active_task_cursor_seed: Option<WorkspaceStreamActiveTaskCursorSeed>,
) -> Result<WorkspaceStreamLiveEventApplication, Box<WorkspaceStreamActiveTaskCursorMissing>> {
    let application = apply_workspace_stream_subscription_event(
        subscription_state,
        subscriptions,
        &event,
        active_task_cursor_seed,
    )?;
    Ok(route_workspace_stream_live_event(application, event))
}

pub fn route_workspace_stream_live_event(
    application: WorkspaceStreamSubscriptionEventApplication,
    event: WorkspaceActiveSnapshotEvent,
) -> WorkspaceStreamLiveEventApplication {
    let WorkspaceStreamSubscriptionEventApplication {
        state: next_state,
        mut subscriptions,
        pin_changes,
        should_route,
    } = application;
    if !should_route {
        return WorkspaceStreamLiveEventApplication {
            state: next_state,
            subscriptions,
            pin_changes,
            route_plan: WorkspaceStreamEventRoutePlan::Drop,
        };
    }

    let accepted = match &event {
        WorkspaceActiveSnapshotEvent::SessionHeadDelta { delta, .. } => {
            let Some(cursor) = subscriptions.get_mut(&delta.session_id) else {
                return WorkspaceStreamLiveEventApplication {
                    state: next_state,
                    subscriptions,
                    pin_changes,
                    route_plan: WorkspaceStreamEventRoutePlan::Drop,
                };
            };
            let accepted = accept_session_delta_cursor(*cursor, delta);
            if !accepted.accepted {
                false
            } else {
                *cursor = accepted.next_cursor;
                true
            }
        }
        WorkspaceActiveSnapshotEvent::SessionHeadSeed { head, .. } => {
            let Some(cursor) = subscriptions.get_mut(&head.session.id) else {
                return WorkspaceStreamLiveEventApplication {
                    state: next_state,
                    subscriptions,
                    pin_changes,
                    route_plan: WorkspaceStreamEventRoutePlan::Drop,
                };
            };
            let accepted = accept_session_head_cursor(*cursor, head);
            if !accepted.accepted {
                false
            } else {
                *cursor = accepted.next_cursor;
                true
            }
        }
        _ => true,
    };
    let route_plan = if accepted {
        plan_workspace_stream_event_route(&next_state, event)
    } else {
        WorkspaceStreamEventRoutePlan::Drop
    };

    WorkspaceStreamLiveEventApplication {
        state: next_state,
        subscriptions,
        pin_changes,
        route_plan,
    }
}

fn remove_active_task_subscription_if_unused(
    subscription_state: &mut WorkspaceActiveSubscriptionState,
    subscriptions: &mut HashMap<SessionId, SessionReplayCursor>,
    task_id: TaskId,
) {
    if let Some(session_id) = subscription_state.active_task_sessions.remove(&task_id) {
        let still_active = subscription_state
            .active_task_sessions
            .values()
            .any(|id| *id == session_id);
        if !still_active && !subscription_state.explicit_sessions.contains(&session_id) {
            subscriptions.remove(&session_id);
        }
    }
}

fn subscription_event_application(
    state: WorkspaceActiveSubscriptionState,
    subscriptions: HashMap<SessionId, SessionReplayCursor>,
    previous_subscriptions: HashSet<SessionId>,
    should_route: bool,
) -> WorkspaceStreamSubscriptionEventApplication {
    let next_subscriptions = subscriptions.keys().copied().collect::<HashSet<_>>();
    let pin_changes =
        workspace_stream_session_pin_changes(previous_subscriptions, next_subscriptions);
    WorkspaceStreamSubscriptionEventApplication {
        state,
        subscriptions,
        pin_changes,
        should_route,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ctx_core::ids::{TaskId, WorkspaceId, WorktreeId};
    use ctx_core::models::{
        ExecutionEnvironment, SessionActivityState, SessionMetadata, SessionSnapshotSummary,
        SessionStatus, Task, TaskStatus, WorkspaceActiveTaskSummary,
    };

    fn cursor(last_event_seq: i64, projection_rev: i64) -> SessionReplayCursor {
        SessionReplayCursor {
            last_event_seq,
            projection_rev,
        }
    }

    fn task(workspace_id: WorkspaceId, session_id: SessionId, task_id: TaskId) -> Task {
        Task {
            id: task_id,
            workspace_id,
            title: "test".to_string(),
            description: None,
            status: TaskStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            exec_plan_id: None,
            primary_session_id: Some(session_id),
            primary_worktree_id: None,
            archived_at: None,
            assistant_seen_at: None,
            last_activity_at: None,
            last_assistant_message_at: None,
            has_active_session: true,
        }
    }

    fn session_summary(workspace_id: WorkspaceId, session_id: SessionId) -> SessionSnapshotSummary {
        SessionSnapshotSummary {
            session: SessionMetadata {
                id: session_id,
                task_id: TaskId::new(),
                workspace_id,
                worktree_id: WorktreeId::new(),
                execution_environment: ExecutionEnvironment::Host,
                parent_session_id: None,
                relationship: None,
                provider_id: "fake".to_string(),
                model_id: "fake-model".to_string(),
                reasoning_effort: None,
                title: "test".to_string(),
                agent_role: "assistant".to_string(),
                status: SessionStatus::Active,
                provider_session_ref: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            last_message_at: None,
            last_message_preview: None,
            last_event_seq: None,
            projection_rev: 0,
            state_rev: 0,
            activity: SessionActivityState::default(),
            unread: None,
        }
    }

    fn active_task_summary(
        workspace_id: WorkspaceId,
        task_id: TaskId,
        session_id: SessionId,
    ) -> WorkspaceActiveTaskSummary {
        WorkspaceActiveTaskSummary {
            task: task(workspace_id, session_id, task_id),
            primary_session: session_summary(workspace_id, session_id),
            primary_session_head: None,
            sessions: Vec::new(),
            sort_at: Utc::now(),
        }
    }

    #[test]
    fn session_removed_updates_state_and_pin_changes() {
        let workspace_id = WorkspaceId::new();
        let session_id = SessionId::new();
        let mut subscription_state = WorkspaceActiveSubscriptionState::default();
        subscription_state.explicit_sessions.insert(session_id);
        subscription_state.replay_sessions.insert(session_id);
        subscription_state.foreground_session_ids = Some(HashSet::from([session_id]));

        let applied = apply_workspace_stream_subscription_event(
            subscription_state,
            HashMap::from([(session_id, cursor(10, 11))]),
            &WorkspaceActiveSnapshotEvent::SessionRemoved {
                workspace_id,
                snapshot_rev: 1,
                session_id,
            },
            None,
        )
        .unwrap();

        assert!(applied.should_route);
        assert!(applied.state.explicit_sessions.is_empty());
        assert!(applied.state.replay_sessions.is_empty());
        assert!(applied.state.foreground_session_ids.is_none());
        assert!(applied.subscriptions.is_empty());
        assert_eq!(applied.pin_changes.detach, vec![session_id]);
    }

    #[test]
    fn active_task_upsert_requests_seed_cursor_for_missing_subscription() {
        let workspace_id = WorkspaceId::new();
        let task_id = TaskId::new();
        let session_id = SessionId::new();
        let mut subscription_state = WorkspaceActiveSubscriptionState::default();
        subscription_state.active_scope = true;
        let event = WorkspaceActiveSnapshotEvent::ActiveTaskUpsert {
            workspace_id,
            snapshot_rev: 1,
            task: Box::new(active_task_summary(workspace_id, task_id, session_id)),
        };

        assert_eq!(
            active_task_cursor_seed_session(&subscription_state, &HashMap::new(), &event),
            Some(session_id),
        );
        let missing = apply_workspace_stream_subscription_event(
            subscription_state,
            HashMap::new(),
            &event,
            None,
        )
        .unwrap_err();
        assert_eq!(missing.session_id, session_id);

        let applied = apply_missing_active_task_cursor(missing, cursor(12, 13));
        assert_eq!(
            applied.state.active_task_sessions.get(&task_id).copied(),
            Some(session_id),
        );
        assert_eq!(
            applied.subscriptions.get(&session_id).copied(),
            Some(cursor(12, 13)),
        );
        assert_eq!(applied.pin_changes.attach, vec![session_id]);
    }

    #[test]
    fn active_task_upsert_preserves_existing_cursor() {
        let workspace_id = WorkspaceId::new();
        let task_id = TaskId::new();
        let session_id = SessionId::new();
        let existing_cursor = cursor(20, 21);
        let mut subscription_state = WorkspaceActiveSubscriptionState::default();
        subscription_state.active_scope = true;
        let event = WorkspaceActiveSnapshotEvent::ActiveTaskUpsert {
            workspace_id,
            snapshot_rev: 1,
            task: Box::new(active_task_summary(workspace_id, task_id, session_id)),
        };

        assert_eq!(
            active_task_cursor_seed_session(
                &subscription_state,
                &HashMap::from([(session_id, existing_cursor)]),
                &event,
            ),
            None,
        );
        let applied = apply_workspace_stream_subscription_event(
            subscription_state,
            HashMap::from([(session_id, existing_cursor)]),
            &event,
            None,
        )
        .unwrap();

        assert_eq!(
            applied.subscriptions.get(&session_id).copied(),
            Some(existing_cursor),
        );
        assert!(applied.pin_changes.attach.is_empty());
    }
}
