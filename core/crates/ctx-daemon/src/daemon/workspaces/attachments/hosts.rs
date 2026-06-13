use anyhow::Result;
use chrono::Utc;
use ctx_core::ids::{WorkspaceAttachmentId, WorkspaceId};
use ctx_core::models::{Workspace, WorkspaceAttachment, WorkspaceAttachmentStatus, Worktree};
use ctx_workspace_attachments as workspace_attachments;
use ctx_worktree_data_plane::resolve_worktree_data_plane_with_host as resolve_worktree_data_plane;

use super::mounts::ensure_workspace_attachments_for_worktrees_with_attachments;
use super::runtime::WorkspaceAttachmentsRuntime;

#[async_trait::async_trait]
impl workspace_attachments::WorkspaceAttachmentsHost for WorkspaceAttachmentsRuntime {
    fn data_root(&self) -> &std::path::Path {
        self.data_root()
    }

    async fn list_workspace_attachments(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<WorkspaceAttachment>> {
        let store = self.store_for_workspace(workspace_id).await?;
        store.list_workspace_attachments(workspace_id).await
    }

    async fn get_workspace_attachment(
        &self,
        workspace_id: WorkspaceId,
        attachment_id: WorkspaceAttachmentId,
    ) -> Result<Option<WorkspaceAttachment>> {
        let store = self.store_for_workspace(workspace_id).await?;
        store.get_workspace_attachment(attachment_id).await
    }

    async fn upsert_workspace_attachment(&self, attachment: &WorkspaceAttachment) -> Result<()> {
        let store = self.store_for_workspace(attachment.workspace_id).await?;
        store.upsert_workspace_attachment(attachment).await
    }

    async fn update_workspace_attachment_status(
        &self,
        workspace_id: WorkspaceId,
        attachment_id: WorkspaceAttachmentId,
        status: WorkspaceAttachmentStatus,
        last_sync_at: Option<chrono::DateTime<Utc>>,
        error_message: Option<String>,
        updated_at: chrono::DateTime<Utc>,
    ) -> Result<()> {
        let store = self.store_for_workspace(workspace_id).await?;
        store
            .update_workspace_attachment_status(
                attachment_id,
                status,
                last_sync_at,
                error_message,
                updated_at,
            )
            .await
    }

    async fn delete_workspace_attachment_record(
        &self,
        workspace_id: WorkspaceId,
        attachment_id: WorkspaceAttachmentId,
    ) -> Result<()> {
        let store = self.store_for_workspace(workspace_id).await?;
        store.delete_workspace_attachment(attachment_id).await
    }

    async fn attachment_became_ready(
        &self,
        workspace: &Workspace,
        attachment: &WorkspaceAttachment,
    ) -> Result<()> {
        ensure_workspace_attachments_for_worktrees_with_attachments(
            self,
            workspace,
            std::slice::from_ref(attachment),
            false,
            false,
        )
        .await
    }

    async fn cleanup_removed_attachment(&self, attachment: &WorkspaceAttachment) -> Result<()> {
        ctx_workspace_attachments::cleanup_removed_attachment(self, attachment).await
    }
}

#[async_trait::async_trait]
impl ctx_workspace_attachments::WorkspaceAttachmentMountHost for WorkspaceAttachmentsRuntime {
    fn data_root(&self) -> &std::path::Path {
        self.data_root()
    }

    fn daemon_url(&self) -> &str {
        self.daemon_url()
    }

    async fn get_worktree(
        &self,
        worktree_id: ctx_core::ids::WorktreeId,
    ) -> Result<Option<Worktree>> {
        let store = self.store_for_worktree(worktree_id).await?;
        store.get_worktree(worktree_id).await
    }

    async fn workspace_store(&self, workspace_id: WorkspaceId) -> Result<ctx_store::Store> {
        self.store_for_workspace(workspace_id).await
    }

    async fn resolve_worktree_data_plane(
        &self,
        worktree: &Worktree,
    ) -> Result<ctx_worktree_data_plane::WorktreeDataPlane> {
        resolve_worktree_data_plane(self, worktree).await
    }

    async fn effective_execution_settings(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<ctx_settings_model::ExecutionSettings> {
        let workspace_store = self.store_for_workspace(workspace_id).await?;
        ctx_settings_service::effective_execution_settings(self.global_store(), &workspace_store)
            .await
    }

    async fn ensure_workspace_container_for_worktree(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
        settings: &ctx_settings_model::ExecutionSettings,
    ) -> Result<()> {
        self.harness()
            .ensure_workspace_container_for_worktree(
                workspace,
                worktree,
                settings,
                self.daemon_url(),
            )
            .await
    }
}

#[async_trait::async_trait]
impl ctx_worktree_data_plane::WorktreeDataPlaneHost for WorkspaceAttachmentsRuntime {
    async fn get_workspace(state: &Self, workspace_id: WorkspaceId) -> Result<Option<Workspace>> {
        state.global_store().get_workspace(workspace_id).await
    }

    async fn workspace_store(state: &Self, workspace_id: WorkspaceId) -> Result<ctx_store::Store> {
        state.store_for_workspace(workspace_id).await
    }
}
