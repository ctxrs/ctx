use super::*;
mod helpers;
mod model_catalog_selection;
mod mutations;

use helpers::get_provider_source_config_locked;
pub use model_catalog_selection::{
    refresh_provider_endpoint_model_catalog, set_provider_endpoint_manual_models,
};
pub use mutations::{
    delete_provider_endpoint, mark_endpoint_verification, set_provider_source_selection,
    upsert_provider_endpoint,
};

pub(super) fn validate_provider_selection(
    provider: &HarnessProviderConfigInternal,
    canonical: &str,
    endpoint_supported: bool,
) -> Result<()> {
    if !endpoint_supported {
        if provider.selected_source_kind != HarnessSourceKind::Subscription
            || provider.selected_endpoint_id.is_some()
        {
            anyhow::bail!(
                "provider {canonical} does not support harness endpoints but endpoint selection is configured"
            );
        }
        return Ok(());
    }

    match provider.selected_source_kind {
        HarnessSourceKind::Subscription => {
            if let Some(endpoint_id) = provider.selected_endpoint_id.as_deref() {
                anyhow::bail!(
                    "provider {canonical} is configured for subscription but still has selected endpoint '{endpoint_id}'"
                );
            }
        }
        HarnessSourceKind::Endpoint => {
            let endpoint_id =
                provider
                    .selected_endpoint_id
                    .as_deref()
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "provider {canonical} is configured for endpoint mode but has no selected endpoint_id"
                        )
                    })?;
            if !provider
                .endpoints
                .iter()
                .any(|endpoint| endpoint.id == endpoint_id)
            {
                anyhow::bail!(
                    "selected endpoint '{endpoint_id}' not found for provider {canonical}"
                );
            }
        }
    }

    Ok(())
}

pub(crate) async fn load_provider_internal(
    data_root: &Path,
    canonical: &str,
    endpoint_supported: bool,
) -> Result<HarnessProviderConfigInternal> {
    let _registry_write_guard = REGISTRY_WRITE_LOCK.lock().await;
    let mut registry = registry::load_registry(data_root).await?;
    let provider = registry
        .providers
        .entry(canonical.to_string())
        .or_default()
        .clone();
    validate_provider_selection(&provider, canonical, endpoint_supported)?;
    Ok(provider)
}

pub async fn get_provider_source_config(
    data_root: &Path,
    provider_id: &str,
) -> Result<HarnessProviderSourceConfig> {
    let canonical = validation::normalize_provider_id(provider_id).ok_or_else(|| {
        anyhow::anyhow!("provider does not support harness endpoints: {provider_id}")
    })?;
    let endpoint_supported = validation::provider_supports_harness_endpoint(canonical);
    let _registry_write_guard = REGISTRY_WRITE_LOCK.lock().await;
    let mut registry = registry::load_registry(data_root).await?;
    get_provider_source_config_locked(data_root, &mut registry, canonical, endpoint_supported).await
}

pub async fn find_provider_endpoint_import_match(
    data_root: &Path,
    provider_id: &str,
    base_url: Option<String>,
    api_shape: HarnessApiShape,
    auth_type: Option<String>,
    model_override: Option<String>,
    api_key: &str,
) -> Result<Option<HarnessEndpointImportMatch>> {
    let canonical = validation::normalize_provider_id(provider_id).ok_or_else(|| {
        anyhow::anyhow!("provider does not support harness endpoints: {provider_id}")
    })?;
    if !validation::provider_supports_harness_endpoint(canonical) {
        anyhow::bail!("provider does not support harness endpoints: {provider_id}");
    }
    validation::ensure_shape_compatible(canonical, api_shape)?;
    let normalized_base_url =
        validation::normalize_base_url_for_provider(canonical, base_url.as_deref())?;
    let normalized_auth_type =
        validation::normalize_auth_type_for_provider(canonical, auth_type.as_deref())?;
    let normalized_model_override = model_override
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let registry = registry::load_registry(data_root).await?;
    let Some(provider) = registry.providers.get(canonical) else {
        return Ok(None);
    };

    let mut config_match_endpoint_id: Option<String> = None;
    for endpoint in &provider.endpoints {
        if endpoint.base_url != normalized_base_url
            || endpoint.api_shape != api_shape
            || endpoint.auth_type != normalized_auth_type
        {
            continue;
        }
        let endpoint_model_override = endpoint
            .model_override
            .as_ref()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if endpoint_model_override != normalized_model_override {
            continue;
        }
        if config_match_endpoint_id.is_none() {
            config_match_endpoint_id = Some(endpoint.id.clone());
        }
        if let Ok(existing_secret) =
            secrets::read_endpoint_secret(data_root, &endpoint.secret_ref).await
        {
            if existing_secret.api_key.as_deref() == Some(api_key) {
                return Ok(Some(HarnessEndpointImportMatch {
                    endpoint_id: endpoint.id.clone(),
                    kind: HarnessEndpointImportMatchKind::ExactCredentials,
                }));
            }
        }
    }

    Ok(
        config_match_endpoint_id.map(|endpoint_id| HarnessEndpointImportMatch {
            endpoint_id,
            kind: HarnessEndpointImportMatchKind::SameConfig,
        }),
    )
}

pub(super) async fn public_endpoint_from_internal(
    data_root: &Path,
    endpoint: &HarnessEndpointRecordInternal,
) -> HarnessEndpointRecord {
    let manual_model_ids = validation::normalize_manual_model_ids(&endpoint.manual_model_ids);
    let model_catalog_models = model_catalog::merge_endpoint_model_records(
        &endpoint.model_catalog_models,
        &manual_model_ids,
    );
    HarnessEndpointRecord {
        id: endpoint.id.clone(),
        provider_id: endpoint.provider_id.clone(),
        name: endpoint.name.clone(),
        base_url: if endpoint.base_url.trim().is_empty() {
            None
        } else {
            Some(endpoint.base_url.clone())
        },
        api_shape: endpoint.api_shape,
        auth_type: endpoint.auth_type.clone(),
        model_override: endpoint.model_override.clone(),
        created_at: endpoint.created_at,
        updated_at: endpoint.updated_at,
        last_verification_status: endpoint.last_verification_status,
        last_verification_at: endpoint.last_verification_at,
        last_error: endpoint.last_error.clone(),
        has_api_key: secrets::read_endpoint_secret(data_root, &endpoint.secret_ref)
            .await
            .is_ok(),
        model_catalog_status: endpoint.model_catalog_status,
        model_catalog_fetched_at: endpoint.model_catalog_fetched_at,
        model_catalog_error: endpoint.model_catalog_error.clone(),
        model_catalog_models,
        manual_model_ids,
        model_catalog_source: endpoint.model_catalog_source.clone(),
    }
}
