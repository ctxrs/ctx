use ctx_core::models::Workspace;
use ctx_observability::telemetry::TelemetryEvent;
use ctx_repo_onboarding_service::WorkspaceRegistrationCandidate;
use ctx_workspace_config as workspace_config;

use crate::daemon::{WorkspaceRegistryHandle, WorkspaceStoreAccessError};

impl WorkspaceRegistryHandle {
    pub(in crate::daemon) async fn list_registered_workspaces(
        &self,
    ) -> anyhow::Result<Vec<Workspace>> {
        self.global_store().list_workspaces().await
    }

    pub(in crate::daemon) async fn load_registered_workspace(
        &self,
        workspace_id: ctx_core::ids::WorkspaceId,
    ) -> anyhow::Result<Option<Workspace>> {
        let workspace = self.global_store().get_workspace(workspace_id).await?;
        if workspace.is_some() {
            self.telemetry()
                .emit(TelemetryEvent::workspace_opened())
                .await;
        }
        Ok(workspace)
    }

    pub(in crate::daemon) async fn register_workspace_candidate(
        &self,
        name: String,
        candidate: WorkspaceRegistrationCandidate,
    ) -> Result<Workspace, WorkspaceRegistryError> {
        let workspace = self
            .global_store()
            .create_workspace(
                name,
                candidate.root_path.to_string_lossy().to_string(),
                candidate.vcs_kind,
            )
            .await
            .map_err(WorkspaceRegistryError::Internal)?;
        let store = self
            .existing_workspace_store(workspace.id)
            .await
            .map_err(WorkspaceRegistryError::Store)?;
        workspace_config::update_primary_branch(&store, &candidate.primary_branch)
            .await
            .map_err(WorkspaceRegistryError::Internal)?;
        self.telemetry()
            .emit(TelemetryEvent::workspace_registered())
            .await;
        Ok(workspace)
    }
}

pub(in crate::daemon) enum WorkspaceRegistryError {
    Store(WorkspaceStoreAccessError),
    Internal(anyhow::Error),
}
