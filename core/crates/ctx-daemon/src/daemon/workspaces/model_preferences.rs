use ctx_core::ids::WorkspaceId;
use ctx_core::models::Workspace;
use ctx_provider_runtime::provider_options::service::effective_preferred_model_id_for_workspace_runtime;
use ctx_workspace_config as workspace_config;

use crate::daemon::WorkspaceProviderModelPreferenceHandle;

#[derive(Debug)]
pub enum WorkspaceProviderModelPreferenceError {
    ProviderIdRequired,
    ProviderNotFound { provider_id: String },
    WorkspaceNotFound,
    StoreUnavailable(anyhow::Error),
    ExecutionSettings(anyhow::Error),
}

pub struct WorkspaceProviderModelPreference {
    pub provider_id: String,
    pub preferred_model_id: Option<String>,
}

impl WorkspaceProviderModelPreferenceHandle {
    pub(in crate::daemon) async fn load_workspace_provider_model_preference(
        &self,
        workspace_id: WorkspaceId,
        provider_id: &str,
    ) -> Result<WorkspaceProviderModelPreference, WorkspaceProviderModelPreferenceError> {
        let workspace = self.load_workspace(workspace_id).await?;
        let provider_id = self.require_configurable_provider_id(provider_id).await?;
        let preferred_model_id = self
            .load_workspace_provider_preferred_model_id(workspace_id, &provider_id)
            .await?;
        let preferred_model_id = self
            .effective_preferred_model_id(&workspace, &provider_id, preferred_model_id)
            .await?;
        Ok(WorkspaceProviderModelPreference {
            provider_id,
            preferred_model_id,
        })
    }

    pub(in crate::daemon) async fn update_workspace_provider_model_preference(
        &self,
        workspace_id: WorkspaceId,
        provider_id: &str,
        preferred_model_id: Option<String>,
    ) -> Result<WorkspaceProviderModelPreference, WorkspaceProviderModelPreferenceError> {
        let workspace = self.load_workspace(workspace_id).await?;
        let provider_id = self.require_configurable_provider_id(provider_id).await?;
        self.update_workspace_provider_preferred_model_id(
            workspace_id,
            &provider_id,
            preferred_model_id,
        )
        .await
        .map_err(WorkspaceProviderModelPreferenceError::StoreUnavailable)?;
        let preferred_model_id = self
            .load_workspace_provider_preferred_model_id(workspace_id, &provider_id)
            .await?;
        let preferred_model_id = self
            .effective_preferred_model_id(&workspace, &provider_id, preferred_model_id)
            .await?;
        Ok(WorkspaceProviderModelPreference {
            provider_id,
            preferred_model_id,
        })
    }

    async fn load_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Workspace, WorkspaceProviderModelPreferenceError> {
        self.launch()
            .load_workspace(workspace_id)
            .await
            .map_err(WorkspaceProviderModelPreferenceError::StoreUnavailable)?
            .ok_or(WorkspaceProviderModelPreferenceError::WorkspaceNotFound)
    }

    async fn require_configurable_provider_id(
        &self,
        provider_id: &str,
    ) -> Result<String, WorkspaceProviderModelPreferenceError> {
        let provider_id = provider_id.trim();
        if provider_id.is_empty() {
            return Err(WorkspaceProviderModelPreferenceError::ProviderIdRequired);
        }

        let matrix = self
            .launch()
            .providers()
            .load_provider_matrix(self.launch().data_root())
            .await;
        if self
            .launch()
            .providers()
            .is_configurable_provider_id(&matrix, provider_id)
            .await
        {
            return Ok(provider_id.to_string());
        }

        Err(WorkspaceProviderModelPreferenceError::ProviderNotFound {
            provider_id: provider_id.to_string(),
        })
    }

    async fn load_workspace_provider_preferred_model_id(
        &self,
        workspace_id: WorkspaceId,
        provider_id: &str,
    ) -> Result<Option<String>, WorkspaceProviderModelPreferenceError> {
        let store = self
            .launch()
            .store_for_workspace(workspace_id)
            .await
            .map_err(WorkspaceProviderModelPreferenceError::StoreUnavailable)?;
        workspace_config::load_preferred_new_session_model_id(&store, provider_id)
            .await
            .map_err(WorkspaceProviderModelPreferenceError::StoreUnavailable)
    }

    async fn update_workspace_provider_preferred_model_id(
        &self,
        workspace_id: WorkspaceId,
        provider_id: &str,
        preferred_model_id: Option<String>,
    ) -> anyhow::Result<()> {
        let store = self.launch().store_for_workspace(workspace_id).await?;
        workspace_config::update_preferred_new_session_model_id(
            &store,
            provider_id,
            preferred_model_id,
        )
        .await?;
        ctx_provider_runtime::provider_cache::invalidate_workspace_provider_options_cache(
            self.launch().providers(),
            workspace_id,
            provider_id,
        )
        .await;
        Ok(())
    }

    async fn effective_preferred_model_id(
        &self,
        workspace: &Workspace,
        provider_id: &str,
        preferred_model_id: Option<String>,
    ) -> Result<Option<String>, WorkspaceProviderModelPreferenceError> {
        let install_target = self
            .launch()
            .install_target_for_workspace(workspace.id)
            .await
            .map_err(WorkspaceProviderModelPreferenceError::ExecutionSettings)?;
        Ok(effective_preferred_model_id_for_workspace_runtime(
            self.launch(),
            workspace,
            provider_id,
            install_target,
            preferred_model_id,
        )
        .await)
    }
}
