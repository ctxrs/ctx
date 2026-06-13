use super::*;

pub async fn set_provider_endpoint_manual_models(
    data_root: &Path,
    provider_id: &str,
    endpoint_id: &str,
    manual_model_ids: Vec<String>,
) -> Result<HarnessEndpointRecord> {
    let canonical = validation::normalize_provider_id(provider_id).ok_or_else(|| {
        anyhow::anyhow!("provider does not support harness endpoints: {provider_id}")
    })?;
    let normalized_manual = validation::normalize_manual_model_ids(&manual_model_ids);
    let _registry_write_guard = REGISTRY_WRITE_LOCK.lock().await;
    let mut registry = registry::load_registry(data_root).await?;
    let provider = registry
        .providers
        .get_mut(canonical)
        .ok_or_else(|| anyhow::anyhow!("unknown provider endpoint config for {canonical}"))?;
    let endpoint = provider
        .endpoints
        .iter_mut()
        .find(|ep| ep.id == endpoint_id)
        .ok_or_else(|| anyhow::anyhow!("unknown endpoint_id: {endpoint_id}"))?;

    endpoint.manual_model_ids = normalized_manual.clone();
    endpoint.model_catalog_source = if endpoint.manual_model_ids.is_empty() {
        if endpoint.model_catalog_models.is_empty() {
            None
        } else {
            Some("discovered".to_string())
        }
    } else if endpoint.model_catalog_models.is_empty() {
        Some("manual".to_string())
    } else {
        Some("mixed".to_string())
    };
    endpoint.model_catalog_status = if endpoint.manual_model_ids.is_empty() {
        if endpoint.model_catalog_models.is_empty() {
            EndpointModelCatalogStatus::Unknown
        } else {
            EndpointModelCatalogStatus::Ready
        }
    } else if endpoint.model_catalog_models.is_empty() {
        EndpointModelCatalogStatus::ManualOnly
    } else {
        EndpointModelCatalogStatus::Ready
    };
    endpoint.updated_at = Utc::now();

    let public = public_endpoint_from_internal(data_root, endpoint).await;
    registry::save_registry(data_root, &registry).await?;
    Ok(public)
}

pub async fn refresh_provider_endpoint_model_catalog(
    data_root: &Path,
    provider_id: &str,
    endpoint_id: &str,
) -> Result<HarnessEndpointRecord> {
    let canonical = validation::normalize_provider_id(provider_id).ok_or_else(|| {
        anyhow::anyhow!("provider does not support harness endpoints: {provider_id}")
    })?;

    let endpoint_snapshot = {
        let _registry_write_guard = REGISTRY_WRITE_LOCK.lock().await;
        let registry = registry::load_registry(data_root).await?;
        let provider = registry
            .providers
            .get(canonical)
            .ok_or_else(|| anyhow::anyhow!("unknown provider endpoint config for {canonical}"))?;
        provider
            .endpoints
            .iter()
            .find(|ep| ep.id == endpoint_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown endpoint_id: {endpoint_id}"))?
    };
    let secret = secrets::read_endpoint_secret(data_root, &endpoint_snapshot.secret_ref).await?;
    let discovery_result = match (
        model_catalog::supports_model_discovery(&endpoint_snapshot),
        endpoint_snapshot.provider_id.as_str(),
        endpoint_snapshot.auth_type.as_str(),
    ) {
        (true, PROVIDER_GEMINI, GEMINI_AUTH_TYPE_VERTEX_AI) => Err(anyhow::anyhow!(
            "model discovery is unsupported for Gemini Vertex AI service-account auth"
        )),
        (true, _, _) => {
            let api_key = secrets::endpoint_secret_api_key(&secret)?;
            model_catalog::discover_openai_models(
                &endpoint_snapshot.base_url,
                &endpoint_snapshot.auth_type,
                &api_key,
            )
            .await
        }
        (false, _, _) => Err(anyhow::anyhow!(
            "model discovery is unsupported for provider '{}' with api_shape '{}' and base_url '{}'",
            canonical,
            endpoint_snapshot.api_shape.as_str(),
            endpoint_snapshot.base_url
        )),
    };

    let _registry_write_guard = REGISTRY_WRITE_LOCK.lock().await;
    let mut registry = registry::load_registry(data_root).await?;
    let provider = registry
        .providers
        .get_mut(canonical)
        .ok_or_else(|| anyhow::anyhow!("unknown provider endpoint config for {canonical}"))?;
    let endpoint = provider
        .endpoints
        .iter_mut()
        .find(|ep| ep.id == endpoint_id)
        .ok_or_else(|| anyhow::anyhow!("unknown endpoint_id: {endpoint_id}"))?;

    endpoint.updated_at = Utc::now();
    match discovery_result {
        Ok(discovered_models) => {
            endpoint.model_catalog_models = discovered_models;
            endpoint.model_catalog_fetched_at = Some(Utc::now());
            endpoint.model_catalog_error = None;
            endpoint.model_catalog_source = if endpoint.manual_model_ids.is_empty() {
                Some("discovered".to_string())
            } else {
                Some("mixed".to_string())
            };
            endpoint.model_catalog_status = if endpoint.model_catalog_models.is_empty() {
                if endpoint.manual_model_ids.is_empty() {
                    EndpointModelCatalogStatus::Error
                } else {
                    EndpointModelCatalogStatus::ManualOnly
                }
            } else {
                EndpointModelCatalogStatus::Ready
            };
        }
        Err(err) => {
            endpoint.model_catalog_error =
                Some(model_catalog::truncate_discovery_error(&err.to_string()));
            endpoint.model_catalog_source = if endpoint.manual_model_ids.is_empty() {
                if endpoint.model_catalog_models.is_empty() {
                    None
                } else {
                    Some("discovered".to_string())
                }
            } else if endpoint.model_catalog_models.is_empty() {
                Some("manual".to_string())
            } else {
                Some("mixed".to_string())
            };
            endpoint.model_catalog_status = if endpoint.manual_model_ids.is_empty() {
                if endpoint.model_catalog_models.is_empty() {
                    EndpointModelCatalogStatus::Error
                } else {
                    EndpointModelCatalogStatus::Ready
                }
            } else if endpoint.model_catalog_models.is_empty() {
                EndpointModelCatalogStatus::ManualOnly
            } else {
                EndpointModelCatalogStatus::Ready
            };
        }
    }

    let public = public_endpoint_from_internal(data_root, endpoint).await;
    registry::save_registry(data_root, &registry).await?;
    Ok(public)
}
