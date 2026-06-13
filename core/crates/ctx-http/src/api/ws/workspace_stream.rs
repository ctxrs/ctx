use super::*;

mod events;
mod lifecycle;
mod subscription;

pub(super) use events::{
    drain_pending_workspace_stream_receiver_burst_deferring,
    flush_deferred_workspace_stream_receiver_events, handle_workspace_stream_lagged,
    handle_workspace_stream_receiver_burst, take_workspace_stream_receiver_burst,
};
pub(super) use lifecycle::{
    initialize_workspace_stream, notify_workspace_stream_shutdown, release_workspace_stream,
};
pub(super) use subscription::handle_workspace_stream_subscription;

pub(super) struct WorkspaceStreamRuntime {
    pub(super) priority_control: Arc<StreamQueue<WorkspaceActiveSnapshotStreamMessage>>,
    pub(super) control: Arc<StreamQueue<WorkspaceActiveSnapshotStreamMessage>>,
    pub(super) foreground_head_buffer: Arc<HeadBatchBuffer>,
    pub(super) background_head_buffer: Arc<HeadBatchBuffer>,
    pub(super) summary_buffer: Arc<SummaryBatchBuffer>,
    pub(super) send_control: Arc<StreamSendControl>,
    pub(super) subscriptions: HashMap<SessionId, SessionCursor>,
    pub(super) last_subscription_fingerprint: Option<String>,
    pub(super) subscription_state: WorkspaceActiveSubscriptionState,
    pub(super) reset_queued: bool,
    pub(super) latest_snapshot_rev: Arc<AtomicI64>,
}

pub(super) struct WorkspaceStreamLabels {
    pub(super) ready_queue_label: &'static str,
    pub(super) subscribe_resolution_log: &'static str,
    pub(super) replay_list_metric: &'static str,
    pub(super) replay_send_metric: Option<&'static str>,
    pub(super) replay_queue_label: &'static str,
    pub(super) replay_failure_log: &'static str,
    pub(super) lagged_log: &'static str,
    pub(super) event_queue_label: &'static str,
}

async fn emit_workspace_stream_incident(
    state: &WorkspaceStreamHandle,
    event_name: &'static str,
    _workspace_id: WorkspaceId,
    labels: &[(&'static str, serde_json::Value)],
) {
    state
        .emit_workspace_stream_incident(event_name, labels)
        .await;
}
