use super::*;

pub(super) async fn provider_source_config_from_internal(
    data_root: &Path,
    canonical: &str,
    endpoint_supported: bool,
    provider: &HarnessProviderConfigInternal,
) -> HarnessProviderSourceConfig {
    let mut endpoints = Vec::new();
    if endpoint_supported {
        endpoints.reserve(provider.endpoints.len());
        for endpoint in &provider.endpoints {
            endpoints.push(public_endpoint_from_internal(data_root, endpoint).await);
        }
    }
    HarnessProviderSourceConfig {
        provider_id: canonical.to_string(),
        selected_source_kind: provider.selected_source_kind,
        selected_endpoint_id: if endpoint_supported {
            provider.selected_endpoint_id.clone()
        } else {
            None
        },
        endpoints,
    }
}

pub(super) async fn get_provider_source_config_locked(
    data_root: &Path,
    registry: &mut HarnessSourceRegistryInternal,
    canonical: &str,
    endpoint_supported: bool,
) -> Result<HarnessProviderSourceConfig> {
    let config = registry
        .providers
        .entry(canonical.to_string())
        .or_default()
        .clone();
    super::validate_provider_selection(&config, canonical, endpoint_supported)?;
    Ok(
        provider_source_config_from_internal(data_root, canonical, endpoint_supported, &config)
            .await,
    )
}
