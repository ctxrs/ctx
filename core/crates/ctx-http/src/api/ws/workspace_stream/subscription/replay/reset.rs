use super::super::super::lifecycle::queue_workspace_stream_reset;
use super::*;

pub(super) async fn queue_failed_replay_reset(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    session_id: SessionId,
    after_seq: i64,
    after_projection_rev: i64,
    labels: &WorkspaceStreamLabels,
    runtime: &mut WorkspaceStreamRuntime,
) -> Result<(), ()> {
    tracing::error!(
        target: "ctx_http.ws_active_snapshot",
        workspace_id = %workspace_id.0,
        session_id = %session_id.0,
        after_seq,
        after_projection_rev,
        "{}",
        labels.replay_failure_log,
    );
    queue_workspace_stream_reset(state, workspace_id, runtime).await
}
