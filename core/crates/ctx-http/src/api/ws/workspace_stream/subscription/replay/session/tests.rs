use super::*;
use ctx_core::ids::{SessionId, TaskId, WorkspaceId};
use ctx_core::models::{SessionHeadDelta, SessionSummaryDelta, WorkspaceActiveSnapshotEvent};
use std::sync::Arc;

fn labels() -> WorkspaceStreamLabels {
    WorkspaceStreamLabels {
        ready_queue_label: "test_ready",
        subscribe_resolution_log: "test_subscribe",
        replay_list_metric: "test_replay_list",
        replay_send_metric: Some("test_replay_send"),
        replay_queue_label: "test_replay_queue",
        replay_failure_log: "test_replay_failure",
        lagged_log: "test_lagged",
        event_queue_label: "test_event_queue",
    }
}

fn queues() -> (
    Arc<StreamQueue<WorkspaceActiveSnapshotStreamMessage>>,
    Arc<StreamQueue<WorkspaceActiveSnapshotStreamMessage>>,
    Arc<HeadBatchBuffer>,
    Arc<HeadBatchBuffer>,
    Arc<SummaryBatchBuffer>,
) {
    (
        Arc::new(StreamQueue::new(
            WORKSPACE_STREAM_QUEUE_LIMIT,
            WORKSPACE_STREAM_QUEUE_MAX_AGE,
        )),
        Arc::new(StreamQueue::new(
            WORKSPACE_STREAM_QUEUE_LIMIT,
            WORKSPACE_STREAM_QUEUE_MAX_AGE,
        )),
        Arc::new(HeadBatchBuffer::new()),
        Arc::new(HeadBatchBuffer::new()),
        Arc::new(SummaryBatchBuffer::new(HEAD_BATCH_TOTAL_LIMIT)),
    )
}

fn replay_route_sinks<'a>(
    control: &'a Arc<StreamQueue<WorkspaceActiveSnapshotStreamMessage>>,
    priority_control: &'a Arc<StreamQueue<WorkspaceActiveSnapshotStreamMessage>>,
    foreground_head_buffer: &'a Arc<HeadBatchBuffer>,
    background_head_buffer: &'a Arc<HeadBatchBuffer>,
    summary_buffer: &'a Arc<SummaryBatchBuffer>,
) -> ReplayRouteSinks<'a> {
    ReplayRouteSinks {
        background_head_buffer: background_head_buffer.as_ref(),
        control: control.as_ref(),
        foreground_head_buffer: foreground_head_buffer.as_ref(),
        priority_control: priority_control.as_ref(),
        summary_buffer: summary_buffer.as_ref(),
    }
}

fn head_delta(session_id: SessionId) -> SessionHeadDelta {
    SessionHeadDelta {
        session_id,
        last_event_seq: 5,
        projection_rev: 7,
        state_rev: 7,
        emitted_at_ms: None,
        session: None,
        activity: None,
        event: None,
        turn: None,
        message: None,
        tool_summaries: Vec::new(),
    }
}

#[tokio::test]
async fn replay_route_plan_tags_head_batches_as_replay() {
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let (control, priority_control, foreground_head_buffer, background_head_buffer, summary_buffer) =
        queues();
    let sinks = replay_route_sinks(
        &control,
        &priority_control,
        &foreground_head_buffer,
        &background_head_buffer,
        &summary_buffer,
    );

    let result = push_replay_event_route_plan(
        workspace_id,
        &labels(),
        &sinks,
        WorkspaceStreamEventRoutePlan::HeadDelta {
            snapshot_rev: 21,
            delta: Box::new(head_delta(session_id)),
            lane: WorkspaceStreamHeadLane::Foreground,
        },
    )
    .await;

    assert!(result.is_ok());
    let drain = foreground_head_buffer.take_with_meta().await;
    assert_eq!(drain.snapshot_rev, 21);
    assert_eq!(
        drain.stream_source,
        WorkspaceActiveSnapshotStreamSource::Replay
    );
    assert_eq!(drain.deltas.len(), 1);
    assert!(background_head_buffer.is_empty().await);
}

#[tokio::test]
async fn replay_route_plan_tags_summary_batches_as_replay() {
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let (control, priority_control, foreground_head_buffer, background_head_buffer, summary_buffer) =
        queues();
    let sinks = replay_route_sinks(
        &control,
        &priority_control,
        &foreground_head_buffer,
        &background_head_buffer,
        &summary_buffer,
    );

    let result = push_replay_event_route_plan(
        workspace_id,
        &labels(),
        &sinks,
        WorkspaceStreamEventRoutePlan::Summary {
            event: WorkspaceActiveSnapshotEvent::SessionSummaryDelta {
                workspace_id,
                snapshot_rev: 22,
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
        },
    )
    .await;

    assert!(result.is_ok());
    let events = summary_buffer.take().await;
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].stream_source,
        WorkspaceActiveSnapshotStreamSource::Replay
    );
}

#[tokio::test]
async fn replay_route_plan_tags_control_events_as_replay_on_planned_lane() {
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let (control, priority_control, foreground_head_buffer, background_head_buffer, summary_buffer) =
        queues();
    let sinks = replay_route_sinks(
        &control,
        &priority_control,
        &foreground_head_buffer,
        &background_head_buffer,
        &summary_buffer,
    );

    let result = push_replay_event_route_plan(
        workspace_id,
        &labels(),
        &sinks,
        WorkspaceStreamEventRoutePlan::Control {
            event: WorkspaceActiveSnapshotEvent::SessionGap {
                workspace_id,
                snapshot_rev: 23,
                session_id,
                after_seq: 9,
                reason: Some("gap".to_string()),
                seed_follows: false,
            },
            session_id: Some(session_id),
            lane: WorkspaceStreamControlLane::Priority,
        },
    )
    .await;

    assert!(result.is_ok());
    assert!(control.is_empty().await);
    let Some(entry) = priority_control.pop().await else {
        panic!("expected priority replay event");
    };
    let (_, message) = entry.into_parts();
    let WorkspaceActiveSnapshotStreamMessage::Event {
        event,
        stream_source,
        ..
    } = message
    else {
        panic!("expected replay event");
    };
    assert_eq!(
        stream_source,
        Some(WorkspaceActiveSnapshotStreamSource::Replay)
    );
    assert!(matches!(
        *event,
        WorkspaceActiveSnapshotEvent::SessionGap { session_id: routed, .. } if routed == session_id
    ));
}
