use async_trait::async_trait;
use ctx_core::ids::WorkspaceId;
use ctx_core::models::{Workspace, Worktree};
use ctx_store::Store;
use ctx_worktree_data_plane::{
    resolve_worktree_data_plane_with_host, WorktreeDataPlane, WorktreeDataPlaneHost,
};

use super::TerminalLaunchHost;

pub(super) async fn resolve_terminal_worktree_data_plane(
    host: &TerminalLaunchHost,
    worktree: &Worktree,
) -> anyhow::Result<WorktreeDataPlane> {
    resolve_worktree_data_plane_with_host(host, worktree).await
}

#[async_trait]
impl WorktreeDataPlaneHost for TerminalLaunchHost {
    async fn get_workspace(
        host: &Self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Option<Workspace>> {
        host.global_store.get_workspace(workspace_id).await
    }

    async fn workspace_store(host: &Self, workspace_id: WorkspaceId) -> anyhow::Result<Store> {
        host.workspace_stores
            .store_for_workspace(workspace_id)
            .await
    }
}
