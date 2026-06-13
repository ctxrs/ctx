use ctx_daemon::daemon::WorkspaceVcsStreamHandle;
use ctx_workspace_stream_service::vcs::WorkspaceVcsDemandState;

pub(in crate::api::ws::workspace_vcs) type WorkspaceVcsRuntime = WorkspaceVcsDemandState;

pub(in crate::api::ws::workspace_vcs) async fn release_workspace_vcs_demand(
    state: &WorkspaceVcsStreamHandle,
    runtime: &WorkspaceVcsRuntime,
) {
    state.release_workspace_vcs_demand(runtime).await;
}
