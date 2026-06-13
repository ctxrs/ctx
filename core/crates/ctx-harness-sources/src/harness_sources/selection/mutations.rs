use super::*;

pub async fn upsert_provider_endpoint(
    data_root: &Path,
    provider_id: &str,
    input: HarnessEndpointUpsert,
) -> Result<HarnessEndpointRecord> {
    let canonical = validation::normalize_provider_id(provider_id).ok_or_else(|| {
        anyhow::anyhow!("provider does not support harness endpoints: {provider_id}")
    })?;
    if !validation::provider_supports_harness_endpoint(canonical) {
        anyhow::bail!("provider does not support harness endpoints: {provider_id}");
    }
    let api_shape = input
        .api_shape
        .or_else(|| validation::default_shape_for_provider(canonical))
        .ok_or_else(|| anyhow::anyhow!("api_shape is required"))?;
    validation::ensure_shape_compatible(canonical, api_shape)?;

    let name = validation::normalize_name(&input.name)?;
    let base_url =
        validation::normalize_base_url_for_provider(canonical, input.base_url.as_deref())?;
    let model_override = input
        .model_override
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let _registry_write_guard = REGISTRY_WRITE_LOCK.lock().await;
    let mut registry = registry::load_registry(data_root).await?;
    let provider = registry.providers.entry(canonical.to_string()).or_default();

    let now = Utc::now();
    let endpoint_id = match input
        .endpoint_id
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(value) => validation::normalize_endpoint_id(&value)?,
        None => uuid::Uuid::new_v4().to_string(),
    };
    validation::ensure_safe_endpoint_id(&endpoint_id)?;

    let auth_type =
        validation::normalize_auth_type_for_provider(canonical, input.auth_type.as_deref())?;

    let existing_index = provider
        .endpoints
        .iter()
        .position(|ep| ep.id == endpoint_id);
    let secret_ref = existing_index
        .and_then(|idx| provider.endpoints.get(idx).map(|ep| ep.secret_ref.clone()))
        .unwrap_or_else(|| format!("{canonical}-{endpoint_id}.json"));
    let existing_secret = match existing_index {
        Some(index) => {
            secrets::read_endpoint_secret(data_root, &provider.endpoints[index].secret_ref)
                .await
                .ok()
        }
        None => None,
    };
    let next_secret = secrets::resolve_endpoint_secret_material(
        canonical,
        &auth_type,
        existing_secret.as_ref(),
        &input,
    )?;
    secrets::write_endpoint_secret(data_root, &secret_ref, &next_secret).await?;

    let mut next = HarnessEndpointRecordInternal {
        id: endpoint_id.clone(),
        provider_id: canonical.to_string(),
        name,
        base_url,
        api_shape,
        auth_type,
        model_override,
        created_at: now,
        updated_at: now,
        last_verification_status: HarnessEndpointVerificationStatus::Unknown,
        last_verification_at: None,
        last_error: None,
        model_catalog_status: EndpointModelCatalogStatus::Unknown,
        model_catalog_fetched_at: None,
        model_catalog_error: None,
        model_catalog_models: Vec::new(),
        manual_model_ids: Vec::new(),
        model_catalog_source: None,
        secret_ref,
    };

    if let Some(idx) = existing_index {
        if let Some(previous) = provider.endpoints.get(idx) {
            next.created_at = previous.created_at;
            next.model_catalog_status = previous.model_catalog_status;
            next.model_catalog_fetched_at = previous.model_catalog_fetched_at;
            next.model_catalog_error = previous.model_catalog_error.clone();
            next.model_catalog_models = previous.model_catalog_models.clone();
            next.manual_model_ids = previous.manual_model_ids.clone();
            next.model_catalog_source = previous.model_catalog_source.clone();
        }
        provider.endpoints[idx] = next.clone();
    } else {
        provider.endpoints.push(next.clone());
    }

    registry::save_registry(data_root, &registry).await?;
    Ok(super::public_endpoint_from_internal(data_root, &next).await)
}

pub async fn delete_provider_endpoint(
    data_root: &Path,
    provider_id: &str,
    endpoint_id: &str,
) -> Result<HarnessProviderSourceConfig> {
    let canonical = validation::normalize_provider_id(provider_id).ok_or_else(|| {
        anyhow::anyhow!("provider does not support harness endpoints: {provider_id}")
    })?;
    if !validation::provider_supports_harness_endpoint(canonical) {
        anyhow::bail!("provider does not support harness endpoints: {provider_id}");
    }
    let _registry_write_guard = REGISTRY_WRITE_LOCK.lock().await;
    let mut registry = registry::load_registry(data_root).await?;
    let provider = registry.providers.entry(canonical.to_string()).or_default();

    let before = provider.endpoints.len();
    let removed: Vec<(String, String)> = provider
        .endpoints
        .iter()
        .filter(|ep| ep.id == endpoint_id)
        .map(|ep| (ep.id.clone(), ep.secret_ref.clone()))
        .collect();
    provider.endpoints.retain(|ep| ep.id != endpoint_id);
    if removed.is_empty() {
        anyhow::bail!("unknown endpoint");
    }

    if provider.selected_endpoint_id.as_deref() == Some(endpoint_id) {
        provider.selected_source_kind = HarnessSourceKind::Subscription;
        provider.selected_endpoint_id = None;
    }

    if before != provider.endpoints.len() {
        let runtime = runtime_resolution::ProviderRuntimeContext::new(canonical, data_root, None);
        for (removed_endpoint_id, secret_ref) in removed {
            match secrets::endpoint_secret_path(data_root, &secret_ref) {
                Ok(secret_path) => match tokio::fs::remove_file(&secret_path).await {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        return Err(err).with_context(|| {
                            format!("removing endpoint secret {}", secret_path.display())
                        });
                    }
                },
                Err(err) => {
                    tracing::warn!(
                        endpoint_id = %removed_endpoint_id,
                        provider_id = canonical,
                        secret_ref = %secret_ref,
                        error = %err,
                        "skipping unsafe harness endpoint secret ref during delete"
                    );
                }
            }
            runtime
                .cleanup_endpoint_runtime(&removed_endpoint_id)
                .await?;
        }
        registry::save_registry(data_root, &registry).await?;
    }

    let endpoint_supported = validation::provider_supports_harness_endpoint(canonical);
    helpers::get_provider_source_config_locked(
        data_root,
        &mut registry,
        canonical,
        endpoint_supported,
    )
    .await
}

pub async fn set_provider_source_selection(
    data_root: &Path,
    provider_id: &str,
    source_kind: HarnessSourceKind,
    endpoint_id: Option<String>,
) -> Result<HarnessProviderSourceConfig> {
    let canonical = validation::normalize_provider_id(provider_id).ok_or_else(|| {
        anyhow::anyhow!("provider does not support harness endpoints: {provider_id}")
    })?;

    let _registry_write_guard = REGISTRY_WRITE_LOCK.lock().await;
    let mut registry = registry::load_registry(data_root).await?;
    let provider = registry.providers.entry(canonical.to_string()).or_default();

    match source_kind {
        HarnessSourceKind::Subscription => {
            provider.selected_source_kind = HarnessSourceKind::Subscription;
            provider.selected_endpoint_id = None;
        }
        HarnessSourceKind::Endpoint => {
            if !validation::provider_supports_harness_endpoint(canonical) {
                anyhow::bail!("provider does not support harness endpoints: {provider_id}");
            }
            let endpoint_id = endpoint_id
                .as_ref()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow::anyhow!("endpoint_id is required for endpoint source"))?;
            let exists = provider.endpoints.iter().any(|ep| ep.id == endpoint_id);
            if !exists {
                anyhow::bail!("unknown endpoint_id: {endpoint_id}");
            }
            provider.selected_source_kind = HarnessSourceKind::Endpoint;
            provider.selected_endpoint_id = Some(endpoint_id);
        }
    }

    registry::save_registry(data_root, &registry).await?;
    let endpoint_supported = validation::provider_supports_harness_endpoint(canonical);
    helpers::get_provider_source_config_locked(
        data_root,
        &mut registry,
        canonical,
        endpoint_supported,
    )
    .await
}

pub async fn mark_endpoint_verification(
    data_root: &Path,
    provider_id: &str,
    endpoint_id: &str,
    status: HarnessEndpointVerificationStatus,
    error: Option<String>,
) -> Result<()> {
    let canonical = validation::normalize_provider_id(provider_id).ok_or_else(|| {
        anyhow::anyhow!("provider does not support harness endpoints: {provider_id}")
    })?;
    let _registry_write_guard = REGISTRY_WRITE_LOCK.lock().await;
    let mut registry = registry::load_registry(data_root).await?;
    let Some(provider) = registry.providers.get_mut(canonical) else {
        return Ok(());
    };
    let Some(endpoint) = provider
        .endpoints
        .iter_mut()
        .find(|ep| ep.id == endpoint_id)
    else {
        return Ok(());
    };
    endpoint.last_verification_status = status;
    endpoint.last_verification_at = Some(Utc::now());
    endpoint.last_error = error;
    endpoint.updated_at = Utc::now();
    registry::save_registry(data_root, &registry).await
}
