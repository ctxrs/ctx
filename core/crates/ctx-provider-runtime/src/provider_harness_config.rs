use std::path::Path;

use ctx_harness_sources as harness_sources;

use crate::provider_cache::invalidate_provider_probe_caches;
use crate::ProviderRuntime;

pub async fn get_provider_harness_config(
    data_root: &Path,
    provider_id: &str,
) -> anyhow::Result<harness_sources::HarnessProviderSourceConfig> {
    harness_sources::get_provider_source_config(data_root, provider_id).await
}

pub async fn select_provider_harness_source(
    runtime: &ProviderRuntime,
    data_root: &Path,
    provider_id: &str,
    source_kind: harness_sources::HarnessSourceKind,
    endpoint_id: Option<String>,
) -> anyhow::Result<harness_sources::HarnessProviderSourceConfig> {
    let config = harness_sources::set_provider_source_selection(
        data_root,
        provider_id,
        source_kind,
        endpoint_id,
    )
    .await?;
    invalidate_provider_probe_caches(runtime, provider_id).await;
    Ok(config)
}

pub async fn upsert_provider_harness_endpoint(
    runtime: &ProviderRuntime,
    data_root: &Path,
    provider_id: &str,
    endpoint: harness_sources::HarnessEndpointUpsert,
    manual_model_ids: Option<Vec<String>>,
) -> anyhow::Result<harness_sources::HarnessProviderSourceConfig> {
    let endpoint =
        harness_sources::upsert_provider_endpoint(data_root, provider_id, endpoint).await?;
    if let Some(manual_model_ids) = manual_model_ids {
        harness_sources::set_provider_endpoint_manual_models(
            data_root,
            provider_id,
            &endpoint.id,
            manual_model_ids,
        )
        .await?;
    }
    harness_sources::refresh_provider_endpoint_model_catalog(data_root, provider_id, &endpoint.id)
        .await?;
    invalidate_provider_probe_caches(runtime, provider_id).await;
    get_provider_harness_config(data_root, provider_id).await
}

pub async fn refresh_provider_harness_endpoint_models(
    runtime: &ProviderRuntime,
    data_root: &Path,
    provider_id: &str,
    endpoint_id: &str,
) -> anyhow::Result<harness_sources::HarnessProviderSourceConfig> {
    refresh_provider_endpoint_model_catalog(data_root, provider_id, endpoint_id).await?;
    invalidate_provider_probe_caches(runtime, provider_id).await;
    get_provider_harness_config(data_root, provider_id).await
}

pub async fn set_provider_harness_endpoint_manual_models(
    runtime: &ProviderRuntime,
    data_root: &Path,
    provider_id: &str,
    endpoint_id: &str,
    model_ids: Vec<String>,
) -> anyhow::Result<harness_sources::HarnessProviderSourceConfig> {
    harness_sources::set_provider_endpoint_manual_models(
        data_root,
        provider_id,
        endpoint_id,
        model_ids,
    )
    .await?;
    invalidate_provider_probe_caches(runtime, provider_id).await;
    get_provider_harness_config(data_root, provider_id).await
}

pub async fn delete_provider_harness_endpoint(
    runtime: &ProviderRuntime,
    data_root: &Path,
    provider_id: &str,
    endpoint_id: &str,
) -> anyhow::Result<harness_sources::HarnessProviderSourceConfig> {
    let config =
        harness_sources::delete_provider_endpoint(data_root, provider_id, endpoint_id).await?;
    invalidate_provider_probe_caches(runtime, provider_id).await;
    Ok(config)
}

pub async fn refresh_provider_endpoint_model_catalog(
    data_root: &Path,
    provider_id: &str,
    endpoint_id: &str,
) -> anyhow::Result<harness_sources::HarnessEndpointRecord> {
    harness_sources::refresh_provider_endpoint_model_catalog(data_root, provider_id, endpoint_id)
        .await
}

pub async fn mark_provider_endpoint_verification(
    data_root: &Path,
    provider_id: &str,
    endpoint_id: &str,
    status: harness_sources::HarnessEndpointVerificationStatus,
    error: Option<String>,
) -> anyhow::Result<()> {
    harness_sources::mark_endpoint_verification(data_root, provider_id, endpoint_id, status, error)
        .await
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn source_selection_invalidates_provider_probe_caches() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let runtime = ProviderRuntime::new(HashMap::new());
        let provider_id = "gemini";
        let provider_cache_key = format!("workspace/host/{provider_id}");
        let other_cache_key = "workspace/host/codex".to_string();

        runtime
            .store_provider_options_cache_value(provider_cache_key.clone(), json!({"cached": true}))
            .await;
        runtime
            .store_provider_verify_cache_value(provider_cache_key.clone(), json!({"cached": true}))
            .await;
        runtime
            .store_provider_options_cache_value(other_cache_key.clone(), json!({"cached": true}))
            .await;
        runtime
            .store_provider_verify_cache_value(other_cache_key.clone(), json!({"cached": true}))
            .await;

        select_provider_harness_source(
            &runtime,
            root.path(),
            provider_id,
            harness_sources::HarnessSourceKind::Subscription,
            None,
        )
        .await?;

        assert!(runtime
            .provider_options_cache_entry(&provider_cache_key)
            .await
            .is_none());
        assert!(runtime
            .provider_verify_cache_entry(&provider_cache_key)
            .await
            .is_none());
        assert!(runtime
            .provider_options_cache_entry(&other_cache_key)
            .await
            .is_some());
        assert!(runtime
            .provider_verify_cache_entry(&other_cache_key)
            .await
            .is_some());

        Ok(())
    }
}
