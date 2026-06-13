use std::sync::Arc;

use ctx_managed_installs as installer;
use ctx_managed_installs::provider_install_contract;
use ctx_provider_install::install_state::{InstallId, InstallTarget};
use ctx_provider_matrix as provider_matrix;

use super::{validate_install_target_allowed, ProviderInstallHost};

pub(super) async fn start_contract_readiness_dependencies<H>(
    state: &Arc<H>,
    managed: &installer::AgentServerConfigFile,
    matrix: &provider_matrix::ProviderMatrix,
    provider_id: &str,
    target: InstallTarget,
    install_id: InstallId,
) where
    H: ProviderInstallHost,
{
    let current_ctx_version = installer::ManagedInstallHost::current_ctx_version(state.as_ref());
    let Ok(contract) = provider_install_contract::resolve_provider_install_contract(
        installer::ManagedInstallHost::data_root(state.as_ref()),
        managed,
        matrix,
        provider_id,
        target,
        current_ctx_version.as_deref(),
    ) else {
        return;
    };
    for dependency in contract.dependencies_for_role(
        provider_install_contract::ProviderInstallDependencyRoleKind::Readiness,
    ) {
        if let Err(error) = validate_install_target_allowed(state, dependency.target) {
            tracing::error!(
                provider_id,
                dependency_provider_id = dependency.provider_id,
                dependency_target = dependency.target.as_str(),
                "provider readiness dependency target is disabled: {}",
                error.message
            );
            continue;
        }
        if dependency.satisfied {
            continue;
        }
        let (dependency_install_id, started_new) = state
            .start_install(dependency.provider_id.clone(), Some(dependency.target))
            .await;
        let _ = state
            .register_install_progress_mirror(dependency_install_id, install_id)
            .await;
        if !started_new {
            continue;
        }
        let state2 = state.clone();
        let dependency_provider_id = dependency.provider_id.clone();
        tokio::spawn(async move {
            if let Err(error) = installer::install_provider_with_progress(
                state2.clone(),
                dependency_install_id,
                dependency_provider_id.clone(),
                dependency.target,
            )
            .await
            {
                tracing::error!(
                    "provider dependency install failed ({dependency_provider_id}): {error:#}"
                );
            }
        });
    }
}

pub(super) async fn seed_running_prerequisite_progress<H>(
    state: &Arc<H>,
    managed: &installer::AgentServerConfigFile,
    matrix: &provider_matrix::ProviderMatrix,
    provider_id: &str,
    target: InstallTarget,
    install_id: InstallId,
) where
    H: ProviderInstallHost,
{
    let current_ctx_version = installer::ManagedInstallHost::current_ctx_version(state.as_ref());
    let Ok(contract) = provider_install_contract::resolve_provider_install_contract(
        installer::ManagedInstallHost::data_root(state.as_ref()),
        managed,
        matrix,
        provider_id,
        target,
        current_ctx_version.as_deref(),
    ) else {
        return;
    };
    for dependency in &contract.dependencies {
        if dependency.satisfied {
            continue;
        }
        let Some(prerequisite_install_id) = state
            .find_running_install(&dependency.provider_id, Some(dependency.target))
            .await
        else {
            continue;
        };
        let _ = state
            .register_install_progress_mirror(prerequisite_install_id, install_id)
            .await;
    }
}
