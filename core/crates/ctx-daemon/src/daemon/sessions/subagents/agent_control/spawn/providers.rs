use std::collections::{HashMap, HashSet};

use ctx_core::ids::WorkspaceId;
use ctx_core::models::{ExecutionEnvironment, Workspace};
use ctx_provider_install::install_state::InstallTarget;
use ctx_provider_runtime::provider_usability::{
    provider_status_is_usable, provider_status_unusable_reason,
};
use ctx_session_tools::model_resolution::ModelCatalog;

use super::super::super::errors::ApiResult;
use super::SubagentSpawnHost;

impl SubagentSpawnHost {
    pub(in crate::daemon) async fn load_requested_model_catalogs(
        &self,
        workspace: &Workspace,
        provider_ids: &HashSet<String>,
        execution_environment: ExecutionEnvironment,
    ) -> ApiResult<HashMap<String, Option<ModelCatalog>>> {
        super::super::super::providers::load_requested_model_catalogs(
            self,
            workspace,
            provider_ids,
            execution_environment,
        )
        .await
    }

    pub(in crate::daemon) async fn effective_install_target_for_environment(
        &self,
        workspace_id: WorkspaceId,
        execution_environment: ExecutionEnvironment,
    ) -> anyhow::Result<InstallTarget> {
        let store = self
            .provider_launch
            .store_for_workspace(workspace_id)
            .await?;
        ctx_settings_service::effective_install_target_for_environment(
            self.provider_launch.global_store(),
            &store,
            execution_environment,
        )
        .await
    }

    pub(in crate::daemon) async fn load_provider_matrix(
        &self,
    ) -> ctx_provider_matrix::ProviderMatrix {
        self.provider_launch
            .providers()
            .load_provider_matrix(&self.data_root)
            .await
    }

    pub(in crate::daemon) async fn known_harness_provider_ids(
        &self,
        matrix: &ctx_provider_matrix::ProviderMatrix,
    ) -> HashSet<String> {
        self.provider_launch
            .providers()
            .known_harness_provider_ids(matrix)
            .await
    }

    pub(in crate::daemon) async fn provider_unusable_reason_for_target(
        &self,
        managed: &ctx_managed_installs::AgentServerConfigFile,
        matrix: &ctx_provider_matrix::ProviderMatrix,
        provider_id: &str,
        install_target: InstallTarget,
    ) -> Option<String> {
        let status = ctx_provider_runtime::provider_launch::status::provider_status_for_target(
            self.provider_launch.as_ref(),
            managed,
            matrix,
            provider_id,
            install_target,
        )
        .await;
        (!provider_status_is_usable(&status)).then(|| {
            provider_status_unusable_reason(&status)
                .unwrap_or_else(|| "provider not ready for use".to_string())
        })
    }

    pub(in crate::daemon) async fn load_provider_model_catalog_for_execution_environment(
        &self,
        workspace: &Workspace,
        provider_id: &str,
        execution_environment: ExecutionEnvironment,
    ) -> Result<Option<ModelCatalog>, String> {
        crate::daemon::sessions::model_catalog::load_provider_model_catalog_for_execution_environment(
            self.provider_launch.as_ref(),
            workspace,
            provider_id,
            execution_environment,
        )
        .await
    }
}
