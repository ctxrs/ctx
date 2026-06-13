use super::*;
use serde_json::json;

pub(crate) async fn initialize_workspace_stream(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    ready_queue_label: &'static str,
) -> Option<(
    WorkspaceStreamRuntime,
    tokio::sync::broadcast::Receiver<WorkspaceActiveSnapshotEvent>,
)> {
    let rx = state
        .subscribe_workspace_active_snapshot(workspace_id)
        .await;
    let priority_control = Arc::new(StreamQueue::new(
        WORKSPACE_STREAM_QUEUE_LIMIT,
        WORKSPACE_STREAM_QUEUE_MAX_AGE,
    ));
    let control = Arc::new(StreamQueue::new(
        WORKSPACE_STREAM_QUEUE_LIMIT,
        WORKSPACE_STREAM_QUEUE_MAX_AGE,
    ));
    let foreground_head_buffer = Arc::new(HeadBatchBuffer::new());
    let background_head_buffer = Arc::new(HeadBatchBuffer::new());
    let summary_buffer = Arc::new(SummaryBatchBuffer::new(HEAD_BATCH_TOTAL_LIMIT));
    let send_control = Arc::new(StreamSendControl::new());
    let stream_state = state.initial_stream_state(workspace_id).await;
    let ready = WorkspaceActiveSnapshotEvent::Ready {
        workspace_id,
        snapshot_rev: stream_state.snapshot_rev,
        archived_rev: stream_state.archived_rev,
    };
    if push_stream_message(
        &control,
        workspace_id,
        None,
        ready_queue_label,
        WorkspaceActiveSnapshotStreamMessage::Event {
            rev: 0,
            event: Box::new(ready),
            stream_source: None,
        },
    )
    .await
    .is_err()
    {
        return None;
    }

    Some((
        WorkspaceStreamRuntime {
            priority_control,
            control,
            foreground_head_buffer,
            background_head_buffer,
            summary_buffer,
            send_control,
            subscriptions: HashMap::new(),
            last_subscription_fingerprint: None,
            subscription_state: WorkspaceActiveSubscriptionState::default(),
            reset_queued: false,
            latest_snapshot_rev: Arc::new(AtomicI64::new(stream_state.snapshot_rev)),
        },
        rx,
    ))
}

pub(crate) async fn notify_workspace_stream_shutdown(runtime: &WorkspaceStreamRuntime) {
    runtime.send_control.set_disconnect_after_flush();
    runtime.priority_control.wake();
    runtime.control.wake();
    runtime.foreground_head_buffer.wake();
    runtime.background_head_buffer.wake();
    runtime.summary_buffer.wake();
}

pub(crate) async fn release_workspace_stream(
    state: &WorkspaceStreamHandle,
    runtime: &WorkspaceStreamRuntime,
) {
    release_workspace_stream_session_pins(state, runtime.subscriptions.keys().copied()).await;
}

pub(super) async fn clear_runtime_queues(runtime: &WorkspaceStreamRuntime) {
    runtime.priority_control.clear().await;
    runtime.control.clear().await;
    runtime.foreground_head_buffer.clear().await;
    runtime.background_head_buffer.clear().await;
    runtime.summary_buffer.clear().await;
}

pub(super) async fn queue_workspace_stream_reset(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    runtime: &mut WorkspaceStreamRuntime,
) -> Result<(), ()> {
    clear_runtime_queues(runtime).await;
    if queue_reset_required(&runtime.priority_control, state, workspace_id)
        .await
        .is_err()
    {
        return Err(());
    }
    emit_workspace_stream_incident(
        state,
        "workspace_stream_reset_queued",
        workspace_id,
        &[(
            "latest_snapshot_rev",
            json!(runtime.latest_snapshot_rev.load(Ordering::Relaxed)),
        )],
    )
    .await;
    runtime.reset_queued = true;
    runtime.send_control.set_disconnect_after_flush();
    Ok(())
}
