use super::*;

use async_trait::async_trait;
use ctx_core::ids::WorkspaceId;
use ctx_provider_runtime::provider_launch::options::{
    provider_options_cache_entry_is_authoritative, provider_supports_runtime_model_catalog,
};
use ctx_provider_runtime::provider_launch::probe::ProviderProbeHost;
use ctx_provider_runtime::provider_launch::status::provider_status_for_target;
use ctx_provider_runtime::ProviderRuntimeHost;
use ctx_session_tools::model_resolution::{build_model_catalog, ModelCatalog};
use ctx_store::Store;

mod endpoint;
mod runtime;

use endpoint::{load_endpoint_model_catalog, EndpointModelCatalog};
use runtime::load_runtime_model_catalog;

#[async_trait]
pub(in crate::daemon) trait ModelCatalogHost:
    ProviderRuntimeHost + ProviderProbeHost + Send + Sync
{
    fn global_store(&self) -> &Store;

    async fn store_for_workspace(&self, workspace_id: WorkspaceId) -> anyhow::Result<Store>;
}

#[async_trait]
impl ModelCatalogHost for crate::daemon::SessionTitleModelModeHandle {
    fn global_store(&self) -> &Store {
        self.global_store()
    }

    async fn store_for_workspace(&self, workspace_id: WorkspaceId) -> anyhow::Result<Store> {
        self.store_for_workspace(workspace_id).await
    }
}

#[async_trait]
impl ModelCatalogHost for crate::daemon::ProviderWorkspaceLaunchRuntime {
    fn global_store(&self) -> &Store {
        self.global_store()
    }

    async fn store_for_workspace(&self, workspace_id: WorkspaceId) -> anyhow::Result<Store> {
        self.store_for_workspace(workspace_id).await
    }
}

async fn load_pinned_subscription_model_catalog(
    host: &impl ModelCatalogHost,
    provider_id: &str,
    install_target: ctx_provider_install::install_state::InstallTarget,
) -> Result<Option<ModelCatalog>, String> {
    let (managed, config_error) =
        ctx_provider_runtime::provider_launch::config::load_managed_agent_server_config_with_error(
            ProviderRuntimeHost::data_root(host),
        )
        .await;
    if let Some(config_error) = config_error {
        return Err(config_error);
    }
    let matrix = host
        .provider_runtime()
        .load_provider_matrix(ProviderRuntimeHost::data_root(host))
        .await;
    let provider_status =
        provider_status_for_target(host, &managed, &matrix, provider_id, install_target).await;
    let models_value = provider_accounts::pinned_subscription_models_value(
        provider_id,
        provider_status.version.as_deref(),
    );
    Ok(models_value.and_then(|value| build_model_catalog(&value)))
}

fn provider_model_cache_key(
    workspace: &Workspace,
    provider_id: &str,
    install_target: ctx_provider_install::install_state::InstallTarget,
) -> String {
    format!(
        "{}/{}/{}",
        workspace.id.0,
        install_target.as_str(),
        provider_id
    )
}

async fn load_provider_model_catalog_for_install_target(
    host: &impl ModelCatalogHost,
    workspace: &Workspace,
    provider_id: &str,
    install_target: ctx_provider_install::install_state::InstallTarget,
) -> Result<Option<ModelCatalog>, String> {
    let cache_key = provider_model_cache_key(workspace, provider_id, install_target);
    let cached_models = host
        .provider_runtime()
        .provider_options_cache_entry(&cache_key)
        .await;
    let cached_models = cached_models
        .filter(|entry| provider_options_cache_entry_is_authoritative(provider_id, &entry.value))
        .and_then(|entry| entry.value.get("models").cloned());
    if let Some(models) = cached_models {
        if let Some(catalog) = build_model_catalog(&models) {
            return Ok(Some(catalog));
        }
    }

    match load_endpoint_model_catalog(host, workspace, provider_id, cache_key.clone()).await? {
        EndpointModelCatalog::Loaded(catalog) => return Ok(catalog.map(|catalog| *catalog)),
        EndpointModelCatalog::NotEndpointSource => {}
    }

    if !provider_supports_runtime_model_catalog(provider_id) {
        return load_pinned_subscription_model_catalog(host, provider_id, install_target).await;
    }

    let pinned_catalog =
        load_pinned_subscription_model_catalog(host, provider_id, install_target).await?;
    load_runtime_model_catalog(
        host,
        workspace,
        provider_id,
        install_target,
        cache_key,
        pinned_catalog,
    )
    .await
}

#[cfg(test)]
pub(in crate::daemon) async fn load_provider_model_catalog(
    host: &impl ModelCatalogHost,
    workspace: &Workspace,
    provider_id: &str,
) -> Result<Option<ModelCatalog>, String> {
    let store = ModelCatalogHost::store_for_workspace(host, workspace.id)
        .await
        .map_err(|err| {
            format!("workspace execution settings unavailable for provider options: {err:#}")
        })?;
    let effective = ctx_settings_service::effective_install_target(
        ModelCatalogHost::global_store(host),
        &store,
    )
    .await
    .map_err(|err| {
        format!("workspace execution settings unavailable for provider options: {err:#}")
    })?;
    load_provider_model_catalog_for_install_target(host, workspace, provider_id, effective).await
}

pub(in crate::daemon) async fn load_provider_model_catalog_for_execution_environment(
    host: &impl ModelCatalogHost,
    workspace: &Workspace,
    provider_id: &str,
    execution_environment: ctx_core::models::ExecutionEnvironment,
) -> Result<Option<ModelCatalog>, String> {
    let store = ModelCatalogHost::store_for_workspace(host, workspace.id)
        .await
        .map_err(|err| {
            format!("workspace execution settings unavailable for provider options: {err:#}")
        })?;
    let install_target = ctx_settings_service::effective_install_target_for_environment(
        ModelCatalogHost::global_store(host),
        &store,
        execution_environment,
    )
    .await
    .map_err(|err| {
        format!("workspace execution settings unavailable for provider options: {err:#}")
    })?;
    load_provider_model_catalog_for_install_target(host, workspace, provider_id, install_target)
        .await
}
