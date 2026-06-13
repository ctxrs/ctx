use ctx_core::ids::WorkspaceId;
use ctx_route_contracts::workspaces::{
    WorkspaceActiveHeadBatchRouteResponse, WorkspaceActiveSnapshotRouteResponse,
    WorkspaceRouteParams,
};

use super::super::{WorkspaceHydrationError, WorkspaceRouteError};
use super::common::workspace_hydration_route_error;
use crate::daemon::WorkspaceActiveHandle;

impl WorkspaceActiveHandle {
    pub async fn workspace_active_snapshot_for_route(
        &self,
        params: WorkspaceRouteParams,
    ) -> Result<WorkspaceActiveSnapshotRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.load_workspace_active_snapshot_for_route(workspace_id)
            .await
            .map_err(workspace_hydration_route_error)
    }

    pub async fn load_workspace_active_snapshot_for_route(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<WorkspaceActiveSnapshotRouteResponse, WorkspaceHydrationError> {
        self.load_workspace_active_snapshot(workspace_id)
            .await
            .map(Into::into)
    }

    pub async fn workspace_active_heads_for_route(
        &self,
        params: WorkspaceRouteParams,
    ) -> Result<WorkspaceActiveHeadBatchRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.load_workspace_active_heads_for_route(workspace_id)
            .await
            .map_err(workspace_hydration_route_error)
    }

    pub async fn load_workspace_active_heads_for_route(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<WorkspaceActiveHeadBatchRouteResponse, WorkspaceHydrationError> {
        self.load_workspace_active_heads(workspace_id)
            .await
            .map(Into::into)
    }
}
