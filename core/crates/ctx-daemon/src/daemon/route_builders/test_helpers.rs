use super::*;

#[cfg(test)]
use crate::daemon::workspace_route_handles::WorkspacePrimaryBranchRefreshEffect;
#[cfg(test)]
use crate::daemon::workspace_stream_route_handles::WorkspaceVcsStreamRefreshEffect;

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn workspace_attachments_runtime_from_state(
    state: &Arc<DaemonState>,
) -> Arc<crate::daemon::workspaces::attachments::WorkspaceAttachmentsRuntime> {
    RouteBuilder::new(Arc::clone(state))
        .workspace_route_deps()
        .workspace_attachments_runtime()
}

#[cfg(test)]
pub(crate) fn workspace_primary_branch_with_refresh_effect_from_state(
    state: &Arc<DaemonState>,
    refresh_vcs_snapshot: WorkspacePrimaryBranchRefreshEffect,
) -> WorkspacePrimaryBranchHandle {
    RouteBuilder::new(Arc::clone(state))
        .workspace_route_deps()
        .workspace_primary_branch_with_refresh_effect(refresh_vcs_snapshot)
}

#[cfg(test)]
pub(crate) fn workspace_vcs_stream_with_refresh_effect_from_state(
    state: &Arc<DaemonState>,
    refresh_worktree_vcs: WorkspaceVcsStreamRefreshEffect,
) -> WorkspaceVcsStreamHandle {
    RouteBuilder::new(Arc::clone(state))
        .workspace_route_deps()
        .workspace_vcs_stream_with_refresh_effect(refresh_worktree_vcs)
}
