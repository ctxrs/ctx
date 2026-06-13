use ctx_core::ids::WorkspaceId;
use ctx_core::models::Workspace;
use ctx_harness_sources::{HarnessEndpointRecord, HarnessProviderSourceConfig};
use ctx_provider_install::install_state::InstallTarget;
use serde_json::Value;

use crate::model_preferences::{
    inject_preferred_model_id, preferred_model_id_from_available_models,
};
use crate::provider_auth::provider_auth_mode;
use crate::provider_launch::config_snapshot::load_provider_launch_config_snapshot;
use crate::provider_launch::models::{
    endpoint_models_payload, subscription_models_payload_from_status,
};
use crate::provider_launch::options::{
    provider_options_probe_plan, provider_supports_runtime_model_catalog,
    runtime_probe_models_payload, ProviderOptionsProbePlan,
};
use crate::provider_launch::probe::ProviderProbeHost;
use crate::provider_options::cache::ProviderOptionsCacheSnapshot;
use crate::provider_runtime_probe_service;
use crate::provider_usability::provider_status_is_usable;
use crate::ProviderRuntimeHost;

use super::{PROVIDER_OPTIONS_CACHE_TTL, PROVIDER_OPTIONS_VERIFY_TTL};

pub async fn effective_preferred_model_id_for_workspace_runtime<H>(
    host: &H,
    workspace: &Workspace,
    provider_id: &str,
    install_target: InstallTarget,
    preferred_model_id: Option<String>,
) -> Option<String>
where
    H: ProviderRuntimeHost + ProviderProbeHost,
{
    let preferred_model_id = preferred_model_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)?;

    let models = effective_model_payload_for_workspace(
        host,
        workspace,
        provider_id,
        install_target,
        &preferred_model_id,
    )
    .await;
    preferred_model_id_from_available_models(Some(preferred_model_id), models.as_ref())
}

async fn effective_model_payload_for_workspace<H>(
    host: &H,
    workspace: &Workspace,
    provider_id: &str,
    install_target: InstallTarget,
    preferred_model_id: &str,
) -> Option<Value>
where
    H: ProviderRuntimeHost + ProviderProbeHost,
{
    let launch_config = load_provider_launch_config_snapshot(host, provider_id).await;
    if launch_config.managed_config_error.is_some() {
        return None;
    }

    let provider_status = launch_config
        .provider_status(host, provider_id, install_target)
        .await;
    let selected_endpoint = launch_config.selected_endpoint_record();
    let source_config = launch_config.source_config();
    let skip_cached_config_surfaces =
        launch_config.managed_config_error.is_some() || launch_config.source_config_error.is_some();
    let cache = ProviderOptionsCacheSnapshot::load(
        host.provider_runtime(),
        workspace.id,
        install_target,
        provider_id,
        skip_cached_config_surfaces,
    )
    .await;

    if let Some(cached) =
        cache.fresh_authoritative_response(PROVIDER_OPTIONS_CACHE_TTL, PROVIDER_OPTIONS_VERIFY_TTL)
    {
        if preferred_model_id_from_available_models(
            Some(preferred_model_id.to_string()),
            cached.get("models"),
        )
        .is_some()
        {
            return cached.get("models").cloned();
        }
    }

    let active_auth = if launch_config.source_config_error.is_none() {
        provider_runtime_probe_service::provider_has_active_auth_for_workspace_runtime(
            host,
            workspace,
            provider_id,
            source_config,
        )
        .await
        .ok()
    } else {
        None
    };

    if let Some(has_active_auth) = active_auth {
        if let Some(endpoint) = selected_endpoint.as_ref() {
            return Some(endpoint_models_payload_for_provider(provider_id, endpoint));
        }

        if provider_status_is_usable(&provider_status)
            && matches!(
                provider_options_probe_plan(
                    provider_supports_runtime_model_catalog(provider_id),
                    None,
                ),
                ProviderOptionsProbePlan::RuntimeModels
            )
        {
            let probe = provider_runtime_probe_service::probe_runtime_models_for_provider_options(
                host,
                workspace,
                provider_id,
                install_target,
            )
            .await;
            if let Ok(probe) = probe {
                let fallback_current_model_id =
                    subscription_models_payload_from_status(&provider_status).and_then(|models| {
                        models
                            .get("current_model_id")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string)
                    });
                if let Some(models) = runtime_probe_models_payload(
                    provider_id,
                    &probe,
                    fallback_current_model_id.as_deref(),
                ) {
                    let response =
                        cached_runtime_models_response(CachedRuntimeModelsResponseArgs {
                            provider_id,
                            workspace_id: workspace.id,
                            installed: provider_status.installed,
                            models: &models,
                            has_active_auth,
                            auth_mode: provider_auth_mode(has_active_auth, source_config),
                            source_config,
                            preferred_model_id,
                        });
                    cache
                        .store_response(host.provider_runtime(), response)
                        .await;
                    return Some(models);
                }
            }
        }
    }

    subscription_models_payload_from_status(&provider_status).or_else(|| cache.cached_models())
}

fn endpoint_models_payload_for_provider(
    provider_id: &str,
    endpoint: &HarnessEndpointRecord,
) -> Value {
    endpoint_models_payload(provider_id, endpoint, chrono::Utc::now())
}

struct CachedRuntimeModelsResponseArgs<'a> {
    provider_id: &'a str,
    workspace_id: WorkspaceId,
    installed: bool,
    models: &'a Value,
    has_active_auth: bool,
    auth_mode: &'a str,
    source_config: Option<&'a HarnessProviderSourceConfig>,
    preferred_model_id: &'a str,
}

fn cached_runtime_models_response(args: CachedRuntimeModelsResponseArgs<'_>) -> Value {
    let CachedRuntimeModelsResponseArgs {
        provider_id,
        workspace_id,
        installed,
        models,
        has_active_auth,
        auth_mode,
        source_config,
        preferred_model_id,
    } = args;
    let mut response = serde_json::json!({
        "provider_id": provider_id,
        "workspace_id": workspace_id.0,
        "installed": installed,
        "probe_ok": true,
        "supports_load": false,
        "auth_required": false,
        "has_active_auth": has_active_auth,
        "auth_mode": auth_mode,
        "probed_at": chrono::Utc::now().to_rfc3339(),
        "models": models,
    });
    if let Some(source_config) = source_config {
        response["source"] = serde_json::to_value(source_config).unwrap_or(Value::Null);
    }
    inject_preferred_model_id(&mut response, Some(preferred_model_id.to_string()));
    response
}
