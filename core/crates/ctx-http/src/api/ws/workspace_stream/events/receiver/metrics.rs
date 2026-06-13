use super::*;

pub(super) async fn record_workspace_stream_receiver_drain(
    state: &WorkspaceStreamHandle,
    labels: &WorkspaceStreamLabels,
    event_count: usize,
    hit_limit: bool,
) {
    if event_count <= 1 && !hit_limit {
        return;
    }
    state
        .record_workspace_stream_receiver_drain(labels.event_queue_label, event_count, hit_limit)
        .await;
}
