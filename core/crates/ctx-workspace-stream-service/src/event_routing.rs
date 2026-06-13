use std::collections::{HashMap, HashSet};

use ctx_core::ids::{SessionId, TaskId};
use ctx_core::models::{
    SessionEvent, SessionEventType, SessionHeadDelta, WorkspaceActiveSnapshotEvent,
    WorkspaceActiveTaskSummary,
};
use ctx_workspace_active_snapshot::{
    primary_session_id_for_active_task, workspace_stream_event_blocks_pending_replay,
    WorkspaceActiveSubscriptionState,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceStreamHeadLane {
    Foreground,
    Background,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceStreamControlLane {
    Priority,
    Normal,
}

#[derive(Debug)]
pub enum WorkspaceStreamEventRoutePlan {
    Drop,
    HeadDelta {
        snapshot_rev: i64,
        delta: Box<SessionHeadDelta>,
        lane: WorkspaceStreamHeadLane,
    },
    Summary {
        event: WorkspaceActiveSnapshotEvent,
    },
    Control {
        event: WorkspaceActiveSnapshotEvent,
        session_id: Option<SessionId>,
        lane: WorkspaceStreamControlLane,
    },
}

pub fn primary_session_id_for_active_task_event(task: &WorkspaceActiveTaskSummary) -> SessionId {
    primary_session_id_for_active_task(task)
}

pub fn event_snapshot_rev(event: &WorkspaceActiveSnapshotEvent) -> Option<i64> {
    match event {
        WorkspaceActiveSnapshotEvent::Ready { snapshot_rev, .. }
        | WorkspaceActiveSnapshotEvent::ActiveTaskUpsert { snapshot_rev, .. }
        | WorkspaceActiveSnapshotEvent::ActiveTaskDelete { snapshot_rev, .. }
        | WorkspaceActiveSnapshotEvent::TaskDelta { snapshot_rev, .. }
        | WorkspaceActiveSnapshotEvent::SessionSummary { snapshot_rev, .. }
        | WorkspaceActiveSnapshotEvent::SessionSummaryDelta { snapshot_rev, .. }
        | WorkspaceActiveSnapshotEvent::SessionRemoved { snapshot_rev, .. }
        | WorkspaceActiveSnapshotEvent::SessionHeadDelta { snapshot_rev, .. }
        | WorkspaceActiveSnapshotEvent::SessionHeadSeed { snapshot_rev, .. }
        | WorkspaceActiveSnapshotEvent::SessionGap { snapshot_rev, .. }
        | WorkspaceActiveSnapshotEvent::WorktreeBootstrap { snapshot_rev, .. } => {
            Some(*snapshot_rev)
        }
        WorkspaceActiveSnapshotEvent::ArchivedTaskUpsert { .. }
        | WorkspaceActiveSnapshotEvent::ArchivedTaskDelete { .. } => None,
    }
}

pub fn event_blocks_pending_replay(
    event: &WorkspaceActiveSnapshotEvent,
    pending_replay_sessions: &HashSet<SessionId>,
    subscription_state: &WorkspaceActiveSubscriptionState,
) -> bool {
    event_blocks_pending_replay_with_active_task_sessions(
        event,
        pending_replay_sessions,
        &subscription_state.active_task_sessions,
    )
}

pub fn event_blocks_pending_replay_with_active_task_sessions(
    event: &WorkspaceActiveSnapshotEvent,
    pending_replay_sessions: &HashSet<SessionId>,
    active_task_sessions: &HashMap<TaskId, SessionId>,
) -> bool {
    workspace_stream_event_blocks_pending_replay(
        event,
        pending_replay_sessions,
        active_task_sessions,
    )
}

pub fn plan_workspace_stream_event_route(
    subscription_state: &WorkspaceActiveSubscriptionState,
    event: WorkspaceActiveSnapshotEvent,
) -> WorkspaceStreamEventRoutePlan {
    let session_id = event_session_id(&event);
    match event {
        WorkspaceActiveSnapshotEvent::SessionHeadDelta {
            snapshot_rev,
            delta,
            ..
        } => {
            if !should_stream_head_delta(
                &subscription_state.active_task_sessions,
                &subscription_state.explicit_sessions,
                subscription_state.foreground_session_ids.as_ref(),
                delta.session_id,
            ) {
                return WorkspaceStreamEventRoutePlan::Drop;
            }
            let Some(delta) = filter_partial_delta_for_active_tasks(
                *delta,
                subscription_state.foreground_session_ids.as_ref(),
            ) else {
                return WorkspaceStreamEventRoutePlan::Drop;
            };
            let lane = if is_foreground_session(
                subscription_state.foreground_session_ids.as_ref(),
                delta.session_id,
            ) {
                WorkspaceStreamHeadLane::Foreground
            } else {
                WorkspaceStreamHeadLane::Background
            };
            WorkspaceStreamEventRoutePlan::HeadDelta {
                snapshot_rev,
                delta: Box::new(delta),
                lane,
            }
        }
        event @ WorkspaceActiveSnapshotEvent::SessionSummaryDelta { .. } => {
            WorkspaceStreamEventRoutePlan::Summary { event }
        }
        event => {
            let lane = if is_priority_control_event(
                &event,
                subscription_state.foreground_session_ids.as_ref(),
            ) {
                WorkspaceStreamControlLane::Priority
            } else {
                WorkspaceStreamControlLane::Normal
            };
            WorkspaceStreamEventRoutePlan::Control {
                event,
                session_id,
                lane,
            }
        }
    }
}

fn event_session_id(event: &WorkspaceActiveSnapshotEvent) -> Option<SessionId> {
    match event {
        WorkspaceActiveSnapshotEvent::SessionHeadDelta { delta, .. } => Some(delta.session_id),
        WorkspaceActiveSnapshotEvent::SessionHeadSeed { head, .. } => Some(head.session.id),
        WorkspaceActiveSnapshotEvent::SessionGap { session_id, .. } => Some(*session_id),
        WorkspaceActiveSnapshotEvent::SessionSummaryDelta { delta, .. } => Some(delta.session_id),
        WorkspaceActiveSnapshotEvent::SessionRemoved { session_id, .. } => Some(*session_id),
        _ => None,
    }
}

pub fn is_foreground_session(
    foreground_session_ids: Option<&HashSet<SessionId>>,
    session_id: SessionId,
) -> bool {
    allows_partial_for_foreground_session(foreground_session_ids, session_id)
}

pub fn should_stream_head_delta(
    active_task_sessions: &HashMap<TaskId, SessionId>,
    explicit_sessions: &HashSet<SessionId>,
    foreground_session_ids: Option<&HashSet<SessionId>>,
    session_id: SessionId,
) -> bool {
    active_task_sessions
        .values()
        .any(|active_session_id| *active_session_id == session_id)
        || explicit_sessions.contains(&session_id)
        || allows_partial_for_foreground_session(foreground_session_ids, session_id)
}

pub fn is_priority_control_event(
    event: &WorkspaceActiveSnapshotEvent,
    foreground_session_ids: Option<&HashSet<SessionId>>,
) -> bool {
    match event {
        WorkspaceActiveSnapshotEvent::SessionGap { session_id, .. } => {
            is_foreground_session(foreground_session_ids, *session_id)
        }
        WorkspaceActiveSnapshotEvent::SessionHeadSeed { head, .. } => {
            is_foreground_session(foreground_session_ids, head.session.id)
        }
        _ => false,
    }
}

pub fn filter_partial_delta_for_active_tasks(
    mut delta: SessionHeadDelta,
    foreground_session_ids: Option<&HashSet<SessionId>>,
) -> Option<SessionHeadDelta> {
    if let Some(event) = delta.event.as_ref() {
        if is_partial_event(event)
            && !allows_partial_for_foreground_session(foreground_session_ids, delta.session_id)
        {
            delta.event = None;
            if delta.turn.is_none() && delta.message.is_none() && delta.tool_summaries.is_empty() {
                return None;
            }
        }
    }
    Some(delta)
}

pub fn is_partial_event(event: &SessionEvent) -> bool {
    matches!(
        event.event_type,
        SessionEventType::AssistantChunk
            | SessionEventType::ThoughtChunk
            | SessionEventType::ContextWindowUpdate
    )
}

fn allows_partial_for_foreground_session(
    foreground_session_ids: Option<&HashSet<SessionId>>,
    session_id: SessionId,
) -> bool {
    foreground_session_ids
        .map(|session_ids| session_ids.contains(&session_id))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ctx_core::ids::{RunId, SessionEventId, TurnId, WorkspaceId};
    use serde_json::json;

    fn delta(session_id: SessionId, event_type: SessionEventType) -> SessionHeadDelta {
        let transient = matches!(event_type, SessionEventType::AssistantChunk);
        SessionHeadDelta {
            session_id,
            last_event_seq: 5,
            projection_rev: 7,
            state_rev: 7,
            emitted_at_ms: None,
            session: None,
            activity: None,
            event: Some(SessionEvent {
                seq: 5,
                id: SessionEventId::new(),
                session_id,
                run_id: Some(RunId::new()),
                turn_id: Some(TurnId::new()),
                event_type,
                payload_json: json!({ "content_fragment": "partial" }),
                transient,
                created_at: Utc::now(),
            }),
            turn: None,
            message: None,
            tool_summaries: Vec::new(),
        }
    }

    #[test]
    fn active_scope_primary_session_head_deltas_are_routed() {
        let workspace_id = WorkspaceId::new();
        let task_id = TaskId::new();
        let session_id = SessionId::new();
        let mut state = WorkspaceActiveSubscriptionState {
            active_scope: true,
            ..WorkspaceActiveSubscriptionState::default()
        };
        state.active_task_sessions.insert(task_id, session_id);

        let plan = plan_workspace_stream_event_route(
            &state,
            WorkspaceActiveSnapshotEvent::SessionHeadDelta {
                workspace_id,
                snapshot_rev: 10,
                delta: Box::new(delta(session_id, SessionEventType::Notice)),
            },
        );

        assert!(matches!(
            plan,
            WorkspaceStreamEventRoutePlan::HeadDelta {
                lane: WorkspaceStreamHeadLane::Background,
                ..
            }
        ));
    }

    #[test]
    fn explicit_head_session_routes_background_head_deltas() {
        let workspace_id = WorkspaceId::new();
        let session_id = SessionId::new();
        let mut state = WorkspaceActiveSubscriptionState::default();
        state.explicit_sessions.insert(session_id);
        state.replay_sessions.insert(session_id);

        let plan = plan_workspace_stream_event_route(
            &state,
            WorkspaceActiveSnapshotEvent::SessionHeadDelta {
                workspace_id,
                snapshot_rev: 10,
                delta: Box::new(delta(session_id, SessionEventType::Notice)),
            },
        );

        assert!(matches!(
            plan,
            WorkspaceStreamEventRoutePlan::HeadDelta {
                lane: WorkspaceStreamHeadLane::Background,
                ..
            }
        ));
    }

    #[test]
    fn foreground_session_routes_partial_head_deltas() {
        let workspace_id = WorkspaceId::new();
        let session_id = SessionId::new();
        let mut state = WorkspaceActiveSubscriptionState::default();
        state.foreground_session_ids = Some(HashSet::from([session_id]));

        let plan = plan_workspace_stream_event_route(
            &state,
            WorkspaceActiveSnapshotEvent::SessionHeadDelta {
                workspace_id,
                snapshot_rev: 10,
                delta: Box::new(delta(session_id, SessionEventType::AssistantChunk)),
            },
        );

        assert!(matches!(
            plan,
            WorkspaceStreamEventRoutePlan::HeadDelta {
                lane: WorkspaceStreamHeadLane::Foreground,
                ..
            }
        ));
    }

    #[test]
    fn explicit_background_session_drops_partial_only_deltas() {
        let workspace_id = WorkspaceId::new();
        let session_id = SessionId::new();
        let mut state = WorkspaceActiveSubscriptionState::default();
        state.explicit_sessions.insert(session_id);

        let plan = plan_workspace_stream_event_route(
            &state,
            WorkspaceActiveSnapshotEvent::SessionHeadDelta {
                workspace_id,
                snapshot_rev: 10,
                delta: Box::new(delta(session_id, SessionEventType::AssistantChunk)),
            },
        );

        assert!(matches!(plan, WorkspaceStreamEventRoutePlan::Drop));
    }
}
