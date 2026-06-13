use super::*;
use chrono::Utc;
use ctx_core::ids::{
    MessageId, RunId, SessionEventId, SessionId, TaskId, TurnId, WorkspaceId, WorktreeId,
};
use ctx_core::models::{
    ExecutionEnvironment, Message, MessageDelivery, MessageRole, SessionActivityState,
    SessionEvent, SessionEventType, SessionHeadDelta, SessionHeadSnapshot, SessionHeadWindow,
    SessionMetadata, SessionStatus, SessionSummaryDelta, WorkspaceActiveSnapshotEvent,
};
use ctx_workspace_active_snapshot::WorkspaceActiveSubscriptionState;
use std::collections::{HashMap, HashSet};

fn test_session_metadata(workspace_id: WorkspaceId, session_id: SessionId) -> SessionMetadata {
    SessionMetadata {
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
    }
}

fn test_head(workspace_id: WorkspaceId, session_id: SessionId) -> SessionHeadSnapshot {
    SessionHeadSnapshot {
        session: test_session_metadata(workspace_id, session_id),
        turns: Vec::new(),
        tool_summaries: Vec::new(),
        events: Vec::new(),
        messages: Vec::new(),
        last_event_seq: 1,
        projection_rev: 1,
        state_rev: 1,
        activity: SessionActivityState::default(),
        has_more_turns: false,
        history_cursor: None,
        has_more_history: false,
        summary_checkpoint: None,
        head_window: SessionHeadWindow::default(),
    }
}

fn partial_delta(session_id: SessionId) -> SessionHeadDelta {
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
            event_type: SessionEventType::AssistantChunk,
            payload_json: serde_json::json!({ "content_fragment": "partial" }),
            transient: true,
            created_at: Utc::now(),
        }),
        turn: None,
        message: None,
        tool_summaries: Vec::new(),
    }
}

fn durable_message(session_id: SessionId) -> Message {
    Message {
        id: MessageId::new(),
        session_id,
        task_id: TaskId::new(),
        run_id: None,
        turn_id: Some(TurnId::new()),
        turn_sequence: Some(1),
        order_seq: Some(1),
        role: MessageRole::Assistant,
        content: "done".to_string(),
        attachments: Vec::new(),
        delivery: MessageDelivery::Immediate,
        delivered_at: Some(Utc::now()),
        created_at: Utc::now(),
    }
}

fn head_delta_event(
    workspace_id: WorkspaceId,
    delta: SessionHeadDelta,
) -> WorkspaceActiveSnapshotEvent {
    WorkspaceActiveSnapshotEvent::SessionHeadDelta {
        workspace_id,
        snapshot_rev: 42,
        delta: Box::new(delta),
    }
}

fn subscribed_state(session_id: SessionId) -> WorkspaceActiveSubscriptionState {
    let mut state = WorkspaceActiveSubscriptionState::default();
    state.explicit_sessions = HashSet::from([session_id]);
    state
}

fn foreground_state(session_id: SessionId) -> WorkspaceActiveSubscriptionState {
    let mut state = WorkspaceActiveSubscriptionState::default();
    state.foreground_session_ids = Some(HashSet::from([session_id]));
    state
}

#[test]
fn route_plan_drops_unsubscribed_head_delta() {
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();

    let plan = plan_workspace_stream_event_route(
        &WorkspaceActiveSubscriptionState::default(),
        head_delta_event(workspace_id, partial_delta(session_id)),
    );

    assert!(matches!(plan, WorkspaceStreamEventRoutePlan::Drop));
}

#[test]
fn route_plan_drops_background_partial_without_durable_payload() {
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();

    let plan = plan_workspace_stream_event_route(
        &subscribed_state(session_id),
        head_delta_event(workspace_id, partial_delta(session_id)),
    );

    assert!(matches!(plan, WorkspaceStreamEventRoutePlan::Drop));
}

#[test]
fn route_plan_keeps_background_partial_when_durable_payload_remains() {
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let mut delta = partial_delta(session_id);
    delta.message = Some(durable_message(session_id));

    let plan = plan_workspace_stream_event_route(
        &subscribed_state(session_id),
        head_delta_event(workspace_id, delta),
    );

    let WorkspaceStreamEventRoutePlan::HeadDelta {
        snapshot_rev,
        delta,
        lane,
    } = plan
    else {
        panic!("expected routed head delta");
    };
    assert_eq!(snapshot_rev, 42);
    assert_eq!(lane, WorkspaceStreamHeadLane::Background);
    assert!(delta.event.is_none());
    assert!(delta.message.is_some());
}

#[test]
fn route_plan_routes_foreground_partials_to_foreground_head_lane() {
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();

    let plan = plan_workspace_stream_event_route(
        &foreground_state(session_id),
        head_delta_event(workspace_id, partial_delta(session_id)),
    );

    let WorkspaceStreamEventRoutePlan::HeadDelta { delta, lane, .. } = plan else {
        panic!("expected foreground head delta");
    };
    assert_eq!(lane, WorkspaceStreamHeadLane::Foreground);
    assert!(delta.event.is_some());
}

#[test]
fn route_plan_routes_summary_deltas_to_summary_lane() {
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();

    let plan = plan_workspace_stream_event_route(
        &WorkspaceActiveSubscriptionState::default(),
        WorkspaceActiveSnapshotEvent::SessionSummaryDelta {
            workspace_id,
            snapshot_rev: 3,
            delta: Box::new(SessionSummaryDelta {
                session_id,
                task_id: TaskId::new(),
                activity: None,
                last_message_at: None,
                last_message_preview: Some("summary".to_string()),
                last_event_seq: Some(5),
                projection_rev: Some(7),
                state_rev: Some(7),
                emitted_at_ms: None,
            }),
        },
    );

    let WorkspaceStreamEventRoutePlan::Summary { event } = plan else {
        panic!("expected summary route");
    };
    assert!(matches!(
        event,
        WorkspaceActiveSnapshotEvent::SessionSummaryDelta { .. }
    ));
}

#[test]
fn route_plan_marks_foreground_gap_and_seed_as_priority_control() {
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let state = foreground_state(session_id);

    for event in [
        WorkspaceActiveSnapshotEvent::SessionGap {
            workspace_id,
            snapshot_rev: 4,
            session_id,
            after_seq: 10,
            reason: Some("gap".to_string()),
            seed_follows: true,
        },
        WorkspaceActiveSnapshotEvent::SessionHeadSeed {
            workspace_id,
            snapshot_rev: 5,
            head: Box::new(test_head(workspace_id, session_id)),
        },
    ] {
        let WorkspaceStreamEventRoutePlan::Control {
            session_id: planned_session_id,
            lane,
            ..
        } = plan_workspace_stream_event_route(&state, event)
        else {
            panic!("expected control route");
        };
        assert_eq!(planned_session_id, Some(session_id));
        assert_eq!(lane, WorkspaceStreamControlLane::Priority);
    }
}

#[test]
fn route_plan_preserves_session_id_for_normal_control_events() {
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();

    let WorkspaceStreamEventRoutePlan::Control {
        session_id: planned_session_id,
        lane,
        ..
    } = plan_workspace_stream_event_route(
        &WorkspaceActiveSubscriptionState {
            active_task_sessions: HashMap::new(),
            explicit_sessions: HashSet::new(),
            foreground_session_ids: Some(HashSet::new()),
            ..WorkspaceActiveSubscriptionState::default()
        },
        WorkspaceActiveSnapshotEvent::SessionRemoved {
            workspace_id,
            snapshot_rev: 6,
            session_id,
        },
    )
    else {
        panic!("expected normal control route");
    };

    assert_eq!(planned_session_id, Some(session_id));
    assert_eq!(lane, WorkspaceStreamControlLane::Normal);
}
