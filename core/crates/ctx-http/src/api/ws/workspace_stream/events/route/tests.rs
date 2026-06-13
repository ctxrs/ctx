use super::*;
use chrono::Utc;
use ctx_daemon::test_support::TestDaemon;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicI64;
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

fn test_runtime(
    subscription_state: WorkspaceActiveSubscriptionState,
    control_limit: usize,
) -> WorkspaceStreamRuntime {
    WorkspaceStreamRuntime {
        priority_control: Arc::new(StreamQueue::new(
            WORKSPACE_STREAM_QUEUE_LIMIT,
            WORKSPACE_STREAM_QUEUE_MAX_AGE,
        )),
        control: Arc::new(StreamQueue::new(
            control_limit,
            WORKSPACE_STREAM_QUEUE_MAX_AGE,
        )),
        foreground_head_buffer: Arc::new(HeadBatchBuffer::new()),
        background_head_buffer: Arc::new(HeadBatchBuffer::new()),
        summary_buffer: Arc::new(SummaryBatchBuffer::new(HEAD_BATCH_TOTAL_LIMIT)),
        send_control: Arc::new(StreamSendControl::new()),
        subscriptions: HashMap::new(),
        last_subscription_fingerprint: None,
        subscription_state,
        reset_queued: false,
        latest_snapshot_rev: Arc::new(AtomicI64::new(0)),
    }
}

fn foreground_state(session_id: SessionId) -> WorkspaceActiveSubscriptionState {
    let mut state = WorkspaceActiveSubscriptionState::default();
    state.foreground_session_ids = Some(HashSet::from([session_id]));
    state
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

async fn test_workspace_stream() -> (tempfile::TempDir, WorkspaceStreamHandle) {
    let root = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(root.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon should start");
    (root, daemon.workspace_stream_handle_for_test())
}

#[tokio::test]
async fn live_route_tags_head_batches_as_live() {
    let (_root, state) = test_workspace_stream().await;
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let mut runtime = test_runtime(foreground_state(session_id), WORKSPACE_STREAM_QUEUE_LIMIT);

    let result = push_workspace_stream_event_route_plan(
        &state,
        workspace_id,
        WorkspaceStreamEventRoutePlan::HeadDelta {
            snapshot_rev: 11,
            delta: Box::new(partial_delta(session_id)),
            lane: WorkspaceStreamHeadLane::Foreground,
        },
        &mut runtime,
        &labels(),
    )
    .await;

    assert!(result.is_ok());
    let drain = runtime.foreground_head_buffer.take_with_meta().await;
    assert_eq!(drain.snapshot_rev, 11);
    assert_eq!(
        drain.stream_source,
        WorkspaceActiveSnapshotStreamSource::Live
    );
    assert_eq!(drain.deltas.len(), 1);
    assert!(runtime.background_head_buffer.is_empty().await);
}

#[tokio::test]
async fn live_route_leaves_control_events_without_replay_source() {
    let (_root, state) = test_workspace_stream().await;
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let mut runtime = test_runtime(foreground_state(session_id), WORKSPACE_STREAM_QUEUE_LIMIT);

    let result = push_workspace_stream_event_route_plan(
        &state,
        workspace_id,
        WorkspaceStreamEventRoutePlan::Control {
            event: WorkspaceActiveSnapshotEvent::SessionGap {
                workspace_id,
                snapshot_rev: 12,
                session_id,
                after_seq: 8,
                reason: Some("gap".to_string()),
                seed_follows: false,
            },
            session_id: Some(session_id),
            lane: WorkspaceStreamControlLane::Priority,
        },
        &mut runtime,
        &labels(),
    )
    .await;

    assert!(result.is_ok());
    let Some(entry) = runtime.priority_control.pop().await else {
        panic!("expected priority control message");
    };
    let (_, message) = entry.into_parts();
    let WorkspaceActiveSnapshotStreamMessage::Event {
        event,
        stream_source,
        ..
    } = message
    else {
        panic!("expected event message");
    };
    assert!(stream_source.is_none());
    assert!(matches!(
        *event,
        WorkspaceActiveSnapshotEvent::SessionGap { session_id: routed, .. } if routed == session_id
    ));
    assert!(runtime.control.is_empty().await);
}

#[tokio::test]
async fn live_route_queues_reset_when_control_queue_is_full() {
    let (_root, state) = test_workspace_stream().await;
    let workspace_id = WorkspaceId::new();
    let mut runtime = test_runtime(WorkspaceActiveSubscriptionState::default(), 0);

    let result = push_workspace_stream_event_route_plan(
        &state,
        workspace_id,
        WorkspaceStreamEventRoutePlan::Control {
            event: WorkspaceActiveSnapshotEvent::Ready {
                workspace_id,
                snapshot_rev: 13,
                archived_rev: 0,
            },
            session_id: None,
            lane: WorkspaceStreamControlLane::Normal,
        },
        &mut runtime,
        &labels(),
    )
    .await;

    assert!(result.is_ok());
    assert!(runtime.reset_queued);
    let Some(entry) = runtime.priority_control.pop().await else {
        panic!("expected reset-required message");
    };
    let (_, message) = entry.into_parts();
    assert!(matches!(
        message,
        WorkspaceActiveSnapshotStreamMessage::ResetRequired { .. }
    ));
}
