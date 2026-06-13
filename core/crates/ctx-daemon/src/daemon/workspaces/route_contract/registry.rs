use ctx_repo_onboarding_service::prepare_workspace_registration;
use ctx_route_contracts::workspaces::{
    CreateWorkspaceRequest, WorkspaceRouteParams, WorkspaceRouteResponse,
};

use super::super::{workspace_store_route_error, WorkspaceRouteError};
use crate::daemon::workspaces::registry::WorkspaceRegistryError;
use crate::daemon::WorkspaceRegistryHandle;

impl WorkspaceRegistryHandle {
    pub async fn list_workspaces_for_route(
        &self,
    ) -> Result<Vec<WorkspaceRouteResponse>, WorkspaceRouteError> {
        let workspaces = self
            .list_registered_workspaces()
            .await
            .map_err(WorkspaceRouteError::internal)?;
        Ok(workspaces.into_iter().map(Into::into).collect())
    }

    pub async fn get_workspace_for_route_params(
        &self,
        params: WorkspaceRouteParams,
    ) -> Result<WorkspaceRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.get_workspace_for_route(workspace_id)
            .await?
            .ok_or_else(|| WorkspaceRouteError::not_found("workspace not found"))
    }

    pub async fn get_workspace_for_route(
        &self,
        workspace_id: ctx_core::ids::WorkspaceId,
    ) -> Result<Option<WorkspaceRouteResponse>, WorkspaceRouteError> {
        let workspace = self
            .load_registered_workspace(workspace_id)
            .await
            .map_err(WorkspaceRouteError::internal)?;
        Ok(workspace.map(Into::into))
    }

    pub async fn create_workspace_for_request(
        &self,
        req: CreateWorkspaceRequest,
    ) -> Result<WorkspaceRouteResponse, WorkspaceRouteError> {
        let candidate = prepare_workspace_registration(&req.root_path)
            .await
            .map_err(|error| WorkspaceRouteError::bad_request(error.message()))?;
        let name = req.name.unwrap_or_else(|| candidate.default_name.clone());
        let workspace = self
            .register_workspace_candidate(name, candidate)
            .await
            .map_err(workspace_registry_route_error)?;
        Ok(workspace.into())
    }
}

fn workspace_registry_route_error(error: WorkspaceRegistryError) -> WorkspaceRouteError {
    match error {
        WorkspaceRegistryError::Store(error) => workspace_store_route_error(error),
        WorkspaceRegistryError::Internal(error) => WorkspaceRouteError::internal(error),
    }
}
