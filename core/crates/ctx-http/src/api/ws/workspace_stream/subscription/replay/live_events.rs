use super::*;
use std::collections::HashSet;

pub(in crate::api::ws::workspace_stream::subscription) fn replay_should_stop(
    runtime: &WorkspaceStreamRuntime,
) -> bool {
    runtime.reset_queued || runtime.send_control.should_disconnect_after_flush()
}

pub(in crate::api::ws::workspace_stream::subscription) async fn drain_live_events_blocking_pending_replay(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    live_rx: &mut tokio::sync::broadcast::Receiver<WorkspaceActiveSnapshotEvent>,
    runtime: &mut WorkspaceStreamRuntime,
    labels: &WorkspaceStreamLabels,
    deferred_live_events: &mut Vec<WorkspaceActiveSnapshotEvent>,
    pending_replay_sessions: &HashSet<SessionId>,
) -> Result<(), ()> {
    let subscription_state = runtime.subscription_state.clone();
    drain_pending_workspace_stream_receiver_burst_deferring(
        state,
        workspace_id,
        live_rx,
        runtime,
        labels,
        deferred_live_events,
        |event| {
            state.event_blocks_pending_replay(event, pending_replay_sessions, &subscription_state)
        },
    )
    .await
}

pub(super) async fn flush_replay_ready_deferred_live_events(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    runtime: &mut WorkspaceStreamRuntime,
    labels: &WorkspaceStreamLabels,
    deferred_live_events: &mut Vec<WorkspaceActiveSnapshotEvent>,
    pending_replay_sessions: &HashSet<SessionId>,
) -> Result<(), ()> {
    let subscription_state = runtime.subscription_state.clone();
    flush_deferred_workspace_stream_receiver_events(
        state,
        workspace_id,
        runtime,
        labels,
        deferred_live_events,
        |event| {
            state.event_blocks_pending_replay(event, pending_replay_sessions, &subscription_state)
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
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
    ) -> WorkspaceStreamRuntime {
        WorkspaceStreamRuntime {
            priority_control: Arc::new(StreamQueue::new(
                WORKSPACE_STREAM_QUEUE_LIMIT,
                WORKSPACE_STREAM_QUEUE_MAX_AGE,
            )),
            control: Arc::new(StreamQueue::new(
                WORKSPACE_STREAM_QUEUE_LIMIT,
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

    #[tokio::test]
    async fn replay_live_event_drain_defers_active_task_delete_for_pending_session() {
        let root = tempfile::tempdir().expect("tempdir");
        let daemon =
            TestDaemon::new_for_test(root.path().to_path_buf(), "http://127.0.0.1:0".to_string())
                .await
                .expect("test daemon should start");
        let state = daemon.workspace_stream_handle_for_test();
        let workspace_id = WorkspaceId::new();
        let task_id = TaskId::new();
        let session_id = SessionId::new();
        let mut runtime = test_runtime(WorkspaceActiveSubscriptionState {
            active_task_sessions: HashMap::from([(task_id, session_id)]),
            ..WorkspaceActiveSubscriptionState::default()
        });
        let pending_replay_sessions = HashSet::from([session_id]);
        let (tx, mut rx) = tokio::sync::broadcast::channel(4);
        tx.send(WorkspaceActiveSnapshotEvent::ActiveTaskDelete {
            workspace_id,
            snapshot_rev: 1,
            task_id,
        })
        .expect("receiver should be open");
        let mut deferred = Vec::new();

        drain_live_events_blocking_pending_replay(
            &state,
            workspace_id,
            &mut rx,
            &mut runtime,
            &labels(),
            &mut deferred,
            &pending_replay_sessions,
        )
        .await
        .expect("drain should succeed");

        assert_eq!(deferred.len(), 1);
        assert!(matches!(
            deferred[0],
            WorkspaceActiveSnapshotEvent::ActiveTaskDelete {
                task_id: deferred_task,
                ..
            } if deferred_task == task_id
        ));
        assert!(runtime.control.is_empty().await);
    }
}
