use super::*;
use ctx_workspace_stream_service::read_model::WorkspaceStreamSnapshotReadModel;

pub(super) fn with_stream_rev(
    message: WorkspaceActiveSnapshotStreamMessage,
    stream_rev: i64,
) -> WorkspaceActiveSnapshotStreamMessage {
    match message {
        WorkspaceActiveSnapshotStreamMessage::Snapshot {
            active_snapshot,
            active_heads,
            ..
        } => WorkspaceActiveSnapshotStreamMessage::Snapshot {
            rev: stream_rev,
            active_snapshot,
            active_heads,
        },
        WorkspaceActiveSnapshotStreamMessage::Event {
            event,
            stream_source,
            ..
        } => WorkspaceActiveSnapshotStreamMessage::Event {
            rev: stream_rev,
            event,
            stream_source,
        },
        WorkspaceActiveSnapshotStreamMessage::HeadsBatch {
            snapshot_rev,
            deltas,
            stream_source,
            ..
        } => WorkspaceActiveSnapshotStreamMessage::HeadsBatch {
            rev: stream_rev,
            snapshot_rev,
            deltas,
            stream_source,
        },
        WorkspaceActiveSnapshotStreamMessage::ResetRequired { latest_rev } => {
            WorkspaceActiveSnapshotStreamMessage::ResetRequired { latest_rev }
        }
    }
}

pub(super) async fn queue_reset_required(
    pending: &StreamQueue<WorkspaceActiveSnapshotStreamMessage>,
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
) -> Result<(), ()> {
    let stream_state = state.initial_stream_state(workspace_id).await;
    if crate::fault_injection::maybe_fail("ctx_http.send_workspace_active_reset").is_err() {
        return Err(());
    }
    push_stream_message(
        pending,
        workspace_id,
        None,
        "reset_required",
        WorkspaceActiveSnapshotStreamMessage::ResetRequired {
            latest_rev: stream_state.snapshot_rev,
        },
    )
    .await
}

pub(super) async fn queue_snapshot_payload(
    pending: &StreamQueue<WorkspaceActiveSnapshotStreamMessage>,
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
) -> Result<WorkspaceStreamSnapshotReadModel, ()> {
    let build_start = Instant::now();
    let read_model = state
        .load_initial_snapshot_read_model(workspace_id)
        .await
        .map_err(|err| {
            tracing::error!(
                target: "ctx_http.ws_active_snapshot",
                workspace_id = %workspace_id.0,
                "workspace snapshot hydration failed before snapshot payload: {err:?}"
            );
        })?;
    let snapshot_rev = read_model.active_snapshot.snapshot_rev;
    let task_count = read_model.active_snapshot.active.tasks.len();
    let head_count = read_model.active_heads.heads.len();
    let build_ms = build_start.elapsed().as_millis();
    if crate::fault_injection::maybe_fail("ctx_http.send_workspace_active_snapshot").is_err() {
        return Err(());
    }
    push_stream_message(
        pending,
        workspace_id,
        None,
        "snapshot",
        WorkspaceActiveSnapshotStreamMessage::Snapshot {
            rev: 0,
            active_snapshot: read_model.active_snapshot.clone(),
            active_heads: Some(read_model.active_heads.clone()),
        },
    )
    .await?;
    tracing::info!(
        target: "ctx_http.ws_active_snapshot",
        workspace_id = %workspace_id.0,
        snapshot_rev,
        active_tasks = task_count,
        active_heads = head_count,
        snapshot_build_ms = build_ms,
        "workspace snapshot queued",
    );
    Ok(read_model)
}
