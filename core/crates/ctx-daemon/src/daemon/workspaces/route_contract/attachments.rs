use ctx_core::ids::WorkspaceId;
use ctx_route_contracts::workspaces::{
    CreateWorkspaceAttachmentRouteRequest, DeleteWorkspaceAttachmentRouteRequest,
    SyncWorkspaceAttachmentsRouteRequest, WorkspaceAttachmentCreateRouteSpec,
    WorkspaceAttachmentRouteResponse, WorkspaceRouteParams,
};
use ctx_workspace_attachments::AttachmentConfig;

use crate::daemon::WorkspaceAttachmentsHandle;

use super::super::WorkspaceRouteError;

fn attachment_config_from_route_spec(spec: WorkspaceAttachmentCreateRouteSpec) -> AttachmentConfig {
    AttachmentConfig {
        kind: spec.kind,
        name: spec.name,
        source: spec.source,
        revision: spec.revision,
        subpath: spec.subpath,
        mount_relpath: spec.mount_relpath,
        mode: spec.mode,
        update_policy: spec.update_policy,
    }
}

impl WorkspaceAttachmentsHandle {
    pub async fn list_workspace_attachments_for_route(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<WorkspaceAttachmentRouteResponse>, WorkspaceRouteError> {
        let attachments = self
            .list_workspace_attachments_for_route_domain(workspace_id)
            .await?;
        Ok(attachments.into_iter().map(Into::into).collect())
    }

    pub async fn sync_workspace_attachments_for_route(
        &self,
        workspace_id: WorkspaceId,
        request: SyncWorkspaceAttachmentsRouteRequest,
    ) -> Result<Vec<WorkspaceAttachmentRouteResponse>, WorkspaceRouteError> {
        let workspace = self.workspace_for_attachment_route(workspace_id).await?;
        let attachments = self
            .runtime()
            .sync_workspace_attachments(&workspace, request.refresh())
            .await
            .map_err(WorkspaceRouteError::internal)?;
        Ok(attachments.into_iter().map(Into::into).collect())
    }

    pub async fn create_and_sync_workspace_attachment_for_route(
        &self,
        workspace_id: WorkspaceId,
        request: CreateWorkspaceAttachmentRouteRequest,
    ) -> Result<Vec<WorkspaceAttachmentRouteResponse>, WorkspaceRouteError> {
        let cfg = attachment_config_from_route_spec(request.into_spec()?);
        let workspace = self.workspace_for_attachment_route(workspace_id).await?;
        self.runtime()
            .upsert_workspace_attachment(workspace_id, cfg)
            .await
            .map_err(WorkspaceRouteError::bad_request)?;
        let attachments = self
            .runtime()
            .sync_workspace_attachments(&workspace, true)
            .await
            .map_err(WorkspaceRouteError::internal)?;
        Ok(attachments.into_iter().map(Into::into).collect())
    }

    pub async fn delete_and_sync_workspace_attachment_for_route(
        &self,
        workspace_id: WorkspaceId,
        request: DeleteWorkspaceAttachmentRouteRequest,
    ) -> Result<Vec<WorkspaceAttachmentRouteResponse>, WorkspaceRouteError> {
        let spec = request.into_spec()?;
        let workspace = self.workspace_for_attachment_route(workspace_id).await?;
        let removed = self
            .runtime()
            .delete_workspace_attachment(workspace_id, spec.kind, &spec.name)
            .await
            .map_err(WorkspaceRouteError::bad_request)?;
        if !removed {
            return Err(WorkspaceRouteError::not_found("attachment not found"));
        }
        let attachments = self
            .runtime()
            .sync_workspace_attachments(&workspace, false)
            .await
            .map_err(WorkspaceRouteError::internal)?;
        Ok(attachments.into_iter().map(Into::into).collect())
    }

    pub async fn list_workspace_attachments_for_route_params(
        &self,
        params: WorkspaceRouteParams,
    ) -> Result<Vec<WorkspaceAttachmentRouteResponse>, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.list_workspace_attachments_for_route(workspace_id)
            .await
    }

    pub async fn sync_workspace_attachments_for_route_params(
        &self,
        params: WorkspaceRouteParams,
        request: SyncWorkspaceAttachmentsRouteRequest,
    ) -> Result<Vec<WorkspaceAttachmentRouteResponse>, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.sync_workspace_attachments_for_route(workspace_id, request)
            .await
    }

    pub async fn create_and_sync_workspace_attachment_for_route_params(
        &self,
        params: WorkspaceRouteParams,
        request: CreateWorkspaceAttachmentRouteRequest,
    ) -> Result<Vec<WorkspaceAttachmentRouteResponse>, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.create_and_sync_workspace_attachment_for_route(workspace_id, request)
            .await
    }

    pub async fn delete_and_sync_workspace_attachment_for_route_params(
        &self,
        params: WorkspaceRouteParams,
        request: DeleteWorkspaceAttachmentRouteRequest,
    ) -> Result<Vec<WorkspaceAttachmentRouteResponse>, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.delete_and_sync_workspace_attachment_for_route(workspace_id, request)
            .await
    }
}
