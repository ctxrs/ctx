use ctx_core::ids::WorkspaceId;
use ctx_core::models::{Workspace, WorkspaceAttachment};

use crate::daemon::WorkspaceAttachmentsHandle;

use super::super::route_config::{workspace_store_route_error, WorkspaceRouteError};

impl WorkspaceAttachmentsHandle {
    pub(in crate::daemon) async fn workspace_for_attachment_route(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Workspace, WorkspaceRouteError> {
        self.existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        self.global_store()
            .get_workspace(workspace_id)
            .await
            .map_err(WorkspaceRouteError::internal)?
            .ok_or_else(|| WorkspaceRouteError::not_found("workspace not found"))
    }

    pub(in crate::daemon) async fn list_workspace_attachments_for_route_domain(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<WorkspaceAttachment>, WorkspaceRouteError> {
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        store
            .list_workspace_attachments(workspace_id)
            .await
            .map_err(WorkspaceRouteError::internal)
    }
}
