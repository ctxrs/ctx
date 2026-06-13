use std::path::Path;

use ctx_core::ids::WorkspaceId;
use ctx_repo_onboarding_service::validate_workspace_primary_branch;
use ctx_workspace_config as workspace_config;

use super::route_config::{
    workspace_store_route_error, UpdateWorkspacePrimaryBranchRequest,
    WorkspacePrimaryBranchSnapshot, WorkspaceRouteError,
};
use crate::daemon::WorkspacePrimaryBranchHandle;

impl WorkspacePrimaryBranchHandle {
    pub(in crate::daemon) async fn load_workspace_primary_branch_snapshot(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<WorkspacePrimaryBranchSnapshot, WorkspaceRouteError> {
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        let primary_branch = workspace_config::load_primary_branch(&store)
            .await
            .map_err(WorkspaceRouteError::internal)?
            .ok_or_else(|| {
                WorkspaceRouteError::not_found("workspace primary branch is not configured")
            })?;
        Ok(WorkspacePrimaryBranchSnapshot { primary_branch })
    }

    pub(in crate::daemon) async fn update_workspace_primary_branch_snapshot(
        &self,
        workspace_id: WorkspaceId,
        req: UpdateWorkspacePrimaryBranchRequest,
    ) -> Result<WorkspacePrimaryBranchSnapshot, WorkspaceRouteError> {
        let workspace = self
            .global_store()
            .get_workspace(workspace_id)
            .await
            .map_err(WorkspaceRouteError::internal)?
            .ok_or_else(|| WorkspaceRouteError::not_found("workspace not found"))?;
        let primary_branch =
            validate_workspace_primary_branch(Path::new(&workspace.root_path), &req.primary_branch)
                .await
                .map_err(|error| WorkspaceRouteError::bad_request(error.message()))?;
        let store = self
            .existing_workspace_store(workspace.id)
            .await
            .map_err(workspace_store_route_error)?;
        let primary_branch =
            workspace_config::update_and_load_primary_branch(&store, &primary_branch)
                .await
                .map_err(WorkspaceRouteError::internal)?;
        let worktrees = store
            .list_worktrees(workspace.id)
            .await
            .map_err(WorkspaceRouteError::internal)?;
        for worktree in worktrees {
            let worktree_id = worktree.id;
            if let Err(error) = self.refresh_vcs_snapshot(worktree).await {
                tracing::warn!(
                    workspace_id = %workspace.id.0,
                    worktree_id = %worktree_id.0,
                    "failed to refresh worktree vcs after primary branch update: {error:#}"
                );
            }
        }
        Ok(WorkspacePrimaryBranchSnapshot { primary_branch })
    }
}
