use ctx_core::ids::WorkspaceId;
use ctx_core::models::Workspace;
use ctx_settings_service::EffectiveExecutionSettingsError;
use ctx_workspace_container::WorkspaceContainerStatus;

use crate::daemon::WorkspaceHarnessContainerHandle;

#[derive(Debug)]
pub enum WorkspaceHarnessContainerError {
    NotFound,
    Internal(anyhow::Error),
    ExecutionSettings(EffectiveExecutionSettingsError),
    Ensure(anyhow::Error),
}

impl WorkspaceHarnessContainerHandle {
    pub(in crate::daemon) async fn workspace_harness_container_status(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Option<WorkspaceContainerStatus>, WorkspaceHarnessContainerError> {
        self.ensure_workspace_exists(workspace_id).await?;
        self.harness()
            .container_status(workspace_id)
            .await
            .map_err(WorkspaceHarnessContainerError::Internal)
    }

    pub(in crate::daemon) async fn stop_workspace_harness_container(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<(), WorkspaceHarnessContainerError> {
        self.ensure_workspace_exists(workspace_id).await?;
        let stopped = self
            .harness()
            .stop_container(workspace_id)
            .await
            .map_err(WorkspaceHarnessContainerError::Internal)?;
        if stopped {
            Ok(())
        } else {
            Err(WorkspaceHarnessContainerError::NotFound)
        }
    }

    pub(in crate::daemon) async fn ensure_workspace_harness_container(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<(), WorkspaceHarnessContainerError> {
        let workspace = self.ensure_workspace_exists(workspace_id).await?;
        let workspace_store = self
            .store_for_workspace(workspace_id)
            .await
            .map_err(|error| {
                WorkspaceHarnessContainerError::ExecutionSettings(
                    EffectiveExecutionSettingsError::Internal(error),
                )
            })?;
        let execution_settings = ctx_settings_service::effective_execution_settings_classified(
            self.global_store(),
            &workspace_store,
        )
        .await
        .map_err(WorkspaceHarnessContainerError::ExecutionSettings)?;
        self.harness()
            .ensure_workspace_container(&workspace, &execution_settings, self.daemon_url())
            .await
            .map_err(WorkspaceHarnessContainerError::Ensure)
    }

    async fn ensure_workspace_exists(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Workspace, WorkspaceHarnessContainerError> {
        self.global_store()
            .get_workspace(workspace_id)
            .await
            .map_err(WorkspaceHarnessContainerError::Internal)?
            .ok_or(WorkspaceHarnessContainerError::NotFound)
    }
}
