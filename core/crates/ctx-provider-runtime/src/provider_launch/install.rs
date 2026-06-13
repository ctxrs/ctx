use anyhow::Result as AnyhowResult;
use async_trait::async_trait;
use ctx_managed_installs as installer;
use ctx_managed_installs::provider_install_contract;
use ctx_provider_install::install_state::{InstallId, InstallTarget};
use std::sync::Arc;

use crate::ProviderRuntimeHost;

mod bulk;
mod dependencies;
#[cfg(test)]
mod tests;

pub use bulk::{should_skip_install_for_healthy_provider, start_all_provider_installs};

use dependencies::{seed_running_prerequisite_progress, start_contract_readiness_dependencies};

#[derive(Debug, Clone)]
pub struct StartProviderInstallError {
    pub message: String,
    pub code: Option<String>,
}

#[async_trait]
pub trait ProviderInstallHost:
    installer::ManagedInstallHost + ProviderRuntimeHost + Send + Sync + 'static
{
    async fn find_running_install(
        &self,
        provider_id: &str,
        target: Option<InstallTarget>,
    ) -> Option<InstallId>;
}

fn install_target_error(error: anyhow::Error) -> StartProviderInstallError {
    StartProviderInstallError {
        message: error.to_string(),
        code: Some("install_target_disabled".to_string()),
    }
}

pub(super) fn validate_install_target_allowed<H>(
    state: &Arc<H>,
    target: InstallTarget,
) -> std::result::Result<(), StartProviderInstallError>
where
    H: ProviderInstallHost,
{
    installer::ManagedInstallHost::validate_install_target_allowed(state.as_ref(), target)
        .map_err(install_target_error)
}

fn validate_contract_dependency_targets<F>(
    contract: &provider_install_contract::ProviderInstallContract,
    mut validate: F,
) -> std::result::Result<(), StartProviderInstallError>
where
    F: FnMut(InstallTarget) -> AnyhowResult<()>,
{
    for dependency in &contract.dependencies {
        validate(dependency.target).map_err(|error| StartProviderInstallError {
            message: format!(
                "provider install dependency '{}' target '{}' is not allowed: {error}",
                dependency.provider_id,
                dependency.target.as_str()
            ),
            code: Some("install_target_disabled".to_string()),
        })?;
    }
    Ok(())
}

pub(super) fn validate_provider_contract_targets<H>(
    state: &Arc<H>,
    contract: &provider_install_contract::ProviderInstallContract,
) -> std::result::Result<(), StartProviderInstallError>
where
    H: ProviderInstallHost,
{
    validate_contract_dependency_targets(contract, |target| {
        installer::ManagedInstallHost::validate_install_target_allowed(state.as_ref(), target)
    })
}

pub async fn start_provider_install<H>(
    state: &Arc<H>,
    provider_id: &str,
    target: InstallTarget,
) -> Result<(InstallId, bool), StartProviderInstallError>
where
    H: ProviderInstallHost,
{
    validate_install_target_allowed(state, target)?;
    let matrix = installer::ManagedInstallHost::load_provider_matrix(state.as_ref()).await;
    let current_ctx_version = installer::ManagedInstallHost::current_ctx_version(state.as_ref());
    let managed = installer::load_agent_server_config(installer::ManagedInstallHost::data_root(
        state.as_ref(),
    ))
    .await
    .map_err(|err| StartProviderInstallError {
        message: err.to_string(),
        code: Some("agent_server_config_invalid".to_string()),
    })?;
    if !installer::is_supported_managed_provider_for_target(&matrix, provider_id, target) {
        return Err(StartProviderInstallError {
            message: format!(
                "unsupported provider for managed install target '{}': {provider_id}",
                target.as_str()
            ),
            code: None,
        });
    }
    if let Some(issue) = provider_install_contract::provider_install_viability_issue(
        installer::ManagedInstallHost::data_root(state.as_ref()),
        &managed,
        &matrix,
        provider_id,
        target,
        current_ctx_version.as_deref(),
    ) {
        return Err(StartProviderInstallError {
            message: issue.message,
            code: Some(issue.code.to_string()),
        });
    }
    let install_contract = provider_install_contract::resolve_provider_install_contract(
        installer::ManagedInstallHost::data_root(state.as_ref()),
        &managed,
        &matrix,
        provider_id,
        target,
        current_ctx_version.as_deref(),
    )
    .map_err(|err| StartProviderInstallError {
        message: err.to_string(),
        code: Some("install_contract_invalid".to_string()),
    })?;
    validate_provider_contract_targets(state, &install_contract)?;

    let (install_id, started_new) = state
        .start_install(provider_id.to_string(), Some(target))
        .await;
    if started_new {
        seed_running_prerequisite_progress(
            state,
            &managed,
            &matrix,
            provider_id,
            target,
            install_id,
        )
        .await;
        start_contract_readiness_dependencies(
            state,
            &managed,
            &matrix,
            provider_id,
            target,
            install_id,
        )
        .await;
        let state2 = state.clone();
        let provider_id = provider_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = installer::install_provider_with_progress(
                state2.clone(),
                install_id,
                provider_id.clone(),
                target,
            )
            .await
            {
                tracing::error!("provider install failed ({provider_id}): {e:#}");
            }
        });
    }

    Ok((install_id, started_new))
}
