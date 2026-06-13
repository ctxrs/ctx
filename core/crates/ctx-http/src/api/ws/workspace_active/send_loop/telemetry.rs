use ctx_core::ids::WorkspaceId;
use ctx_core::models::WorkspaceActiveSnapshotStreamMessage;

pub(super) fn log_workspace_snapshot_sent(
    workspace_id: WorkspaceId,
    payload_bytes: usize,
    queued_ms: u128,
    encode_ms: u128,
    send_ms: u128,
    message: &WorkspaceActiveSnapshotStreamMessage,
) {
    let (task_count, head_count) = match message {
        WorkspaceActiveSnapshotStreamMessage::Snapshot {
            active_snapshot,
            active_heads,
            ..
        } => (
            active_snapshot.active.tasks.len(),
            active_heads.as_ref().map(|h| h.heads.len()).unwrap_or(0),
        ),
        _ => (0, 0),
    };
    tracing::info!(
        target: "ctx_http.ws_active_snapshot",
        workspace_id = %workspace_id.0,
        snapshot_bytes = payload_bytes,
        snapshot_queue_ms = queued_ms,
        snapshot_encode_ms = encode_ms,
        snapshot_send_ms = send_ms,
        active_tasks = task_count,
        active_heads = head_count,
        "workspace snapshot sent",
    );
}

pub(super) fn log_workspace_heads_batch_sent(
    workspace_id: WorkspaceId,
    lane: &'static str,
    delta_count: usize,
    payload_bytes: usize,
    oldest_queued_ms: u128,
    encode_ms: u128,
    send_ms: u128,
) {
    tracing::info!(
        target: "ctx_http.ws_active_snapshot",
        workspace_id = %workspace_id.0,
        lane = lane,
        head_batch_deltas = delta_count,
        head_batch_bytes = payload_bytes,
        head_batch_oldest_queue_ms = oldest_queued_ms,
        head_batch_encode_ms = encode_ms,
        head_batch_send_ms = send_ms,
        "workspace heads batch sent",
    );
}
