use super::route_config::{
    UpdateWorkspacePrimaryBranchRequest, WorkspacePrimaryBranchSnapshot, WorkspaceRouteError,
};
use crate::daemon::WorkspacePrimaryBranchHandle;
use ctx_route_contracts::workspaces::WorkspaceRouteParams;

impl WorkspacePrimaryBranchHandle {
    pub async fn workspace_primary_branch_for_route_params(
        &self,
        params: WorkspaceRouteParams,
    ) -> Result<WorkspacePrimaryBranchSnapshot, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.load_workspace_primary_branch_snapshot(workspace_id)
            .await
    }

    pub async fn update_workspace_primary_branch_for_route_params(
        &self,
        params: WorkspaceRouteParams,
        request: UpdateWorkspacePrimaryBranchRequest,
    ) -> Result<WorkspacePrimaryBranchSnapshot, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.update_workspace_primary_branch_snapshot(workspace_id, request)
            .await
    }
}
