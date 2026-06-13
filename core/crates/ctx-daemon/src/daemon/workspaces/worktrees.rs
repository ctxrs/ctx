use async_trait::async_trait;
use ctx_core::ids::{WorkspaceId, WorktreeId};
use ctx_core::models::{Workspace, Worktree};
use ctx_store::Store;
use ctx_worktree_data_plane::WorktreeDataPlaneHost;

use crate::daemon::{WorkspaceStoreAccessError, WorkspaceWorktreeHandle};

impl WorkspaceWorktreeHandle {
    pub(in crate::daemon) async fn load_workspace_for_worktree_data_plane(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Option<Workspace>> {
        self.global_store().get_workspace(workspace_id).await
    }

    pub(in crate::daemon) async fn workspace_store_for_worktree_data_plane(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Store> {
        self.store_for_workspace(workspace_id).await
    }

    pub(in crate::daemon) async fn loaded_worktree_with_live_root(
        &self,
        worktree_id: WorktreeId,
    ) -> anyhow::Result<Option<Worktree>> {
        let Some(store) = self.worktree_store_or_none(worktree_id).await? else {
            return Ok(None);
        };
        let Some(mut worktree) = store.get_worktree(worktree_id).await? else {
            return Ok(None);
        };
        worktree.root_path =
            ctx_worktree_data_plane::resolve_worktree_data_plane_with_host(self, &worktree)
                .await?
                .live_worktree_root
                .to_string_lossy()
                .to_string();
        Ok(Some(worktree))
    }

    pub(in crate::daemon) async fn worktree_bootstrap_log_path(
        &self,
        worktree_id: WorktreeId,
    ) -> anyhow::Result<Option<String>> {
        let Some(store) = self.worktree_store_or_none(worktree_id).await? else {
            return Ok(None);
        };
        let Some(worktree) = store.get_worktree(worktree_id).await? else {
            return Ok(None);
        };
        Ok(worktree.bootstrap_log_path)
    }

    async fn worktree_store_or_none(
        &self,
        worktree_id: WorktreeId,
    ) -> anyhow::Result<Option<Store>> {
        let Some(workspace_id) = self
            .global_store()
            .get_workspace_id_for_worktree(worktree_id)
            .await?
        else {
            return Ok(None);
        };
        match self.existing_workspace_store(workspace_id).await {
            Ok(store) => Ok(Some(store)),
            Err(WorkspaceStoreAccessError::NotFound) => Ok(None),
            Err(WorkspaceStoreAccessError::Unavailable(error)) => Err(error),
        }
    }
}

#[async_trait]
impl WorktreeDataPlaneHost for WorkspaceWorktreeHandle {
    async fn get_workspace(
        handle: &Self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Option<Workspace>> {
        handle
            .load_workspace_for_worktree_data_plane(workspace_id)
            .await
    }

    async fn workspace_store(handle: &Self, workspace_id: WorkspaceId) -> anyhow::Result<Store> {
        handle
            .workspace_store_for_worktree_data_plane(workspace_id)
            .await
    }
}
