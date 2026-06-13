use ctx_harness_sources::HarnessEndpointRecord;
use ctx_provider_install::install_state::InstallTarget;
use ctx_providers::adapters::ProviderStatus;

use crate::provider_auth::{
    selected_endpoint_from_harness_config, selected_endpoint_record_from_harness_config,
};
use crate::provider_launch::status::provider_status_for_target;
use crate::ProviderRuntimeHost;

pub struct ProviderLaunchConfigSnapshot {
    managed: ctx_managed_installs::AgentServerConfigFile,
    matrix: ctx_provider_matrix::ProviderMatrix,
    pub managed_config_error: Option<String>,
    pub source_config: Option<ctx_harness_sources::HarnessProviderSourceConfig>,
    pub source_config_error: Option<String>,
}

#[derive(Debug)]
pub enum ProviderLaunchConfigError {
    UnsupportedProvider { provider_id: String },
}

pub async fn load_provider_launch_config_snapshot(
    state: &impl ProviderRuntimeHost,
    provider_id: &str,
) -> ProviderLaunchConfigSnapshot {
    let (managed, managed_config_error) =
        crate::provider_launch::config::load_managed_agent_server_config_with_error(
            state.data_root(),
        )
        .await;
    let matrix = state
        .provider_runtime()
        .load_provider_matrix(state.data_root())
        .await;
    let (source_config, source_config_error) =
        crate::provider_launch::config::load_provider_source_config_with_error(
            state.data_root(),
            provider_id,
        )
        .await;

    ProviderLaunchConfigSnapshot {
        managed,
        matrix,
        managed_config_error,
        source_config,
        source_config_error,
    }
}

impl ProviderLaunchConfigSnapshot {
    pub async fn ensure_known_provider(
        &self,
        state: &impl ProviderRuntimeHost,
        provider_id: &str,
    ) -> Result<(), ProviderLaunchConfigError> {
        if state
            .provider_runtime()
            .is_known_provider_id(&self.matrix, provider_id)
            .await
        {
            return Ok(());
        }

        Err(ProviderLaunchConfigError::UnsupportedProvider {
            provider_id: provider_id.to_string(),
        })
    }

    pub async fn provider_status(
        &self,
        state: &impl ProviderRuntimeHost,
        provider_id: &str,
        target: InstallTarget,
    ) -> ProviderStatus {
        if self.managed_config_error.is_some() {
            return state
                .provider_runtime()
                .provider_status_without_target_bootstrap(provider_id, target)
                .await;
        }

        provider_status_for_target(state, &self.managed, &self.matrix, provider_id, target).await
    }

    pub fn selected_endpoint_record(&self) -> Option<HarnessEndpointRecord> {
        selected_endpoint_record_from_harness_config(self.source_config.as_ref())
    }

    pub fn selected_endpoint_id(&self) -> Option<String> {
        selected_endpoint_from_harness_config(self.source_config.clone())
    }

    pub fn source_config(&self) -> Option<&ctx_harness_sources::HarnessProviderSourceConfig> {
        self.source_config.as_ref()
    }
}
