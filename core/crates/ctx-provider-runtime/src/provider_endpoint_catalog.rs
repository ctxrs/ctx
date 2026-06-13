use std::collections::HashSet;
use std::path::Path;

use chrono::Utc;

use crate::provider_cache::cache_key_matches_provider;
use crate::ProviderRuntime;

#[derive(Debug, Default)]
pub struct EndpointModelCatalogSweepResult {
    pub refreshed: usize,
    pub failed: usize,
    pub refreshed_provider_ids: HashSet<String>,
}

impl EndpointModelCatalogSweepResult {
    pub fn has_activity(&self) -> bool {
        self.refreshed > 0 || self.failed > 0
    }
}

impl ProviderRuntime {
    pub async fn refresh_stale_selected_endpoint_model_catalogs(
        &self,
        data_root: &Path,
    ) -> EndpointModelCatalogSweepResult {
        let provider_ids = self.provider_status_ids().await;
        let now = Utc::now();
        let mut result = EndpointModelCatalogSweepResult::default();

        for provider_id in provider_ids {
            let config = match ctx_harness_sources::get_provider_source_config(
                data_root,
                &provider_id,
            )
            .await
            {
                Ok(config) => config,
                Err(err) => {
                    result.failed += 1;
                    tracing::warn!(
                        provider_id = provider_id,
                        err = %err,
                        "endpoint model catalog sweep failed to load provider harness config"
                    );
                    continue;
                }
            };
            if config.selected_source_kind != ctx_harness_sources::HarnessSourceKind::Endpoint {
                continue;
            }
            let Some(selected_endpoint_id) = config.selected_endpoint_id.as_deref() else {
                continue;
            };
            let Some(endpoint) = config
                .endpoints
                .iter()
                .find(|candidate| candidate.id == selected_endpoint_id)
            else {
                continue;
            };
            if !ctx_harness_sources::endpoint_model_catalog_is_stale(endpoint, now) {
                continue;
            }

            match ctx_harness_sources::refresh_provider_endpoint_model_catalog(
                data_root,
                &provider_id,
                selected_endpoint_id,
            )
            .await
            {
                Ok(_) => {
                    result.refreshed += 1;
                    result.refreshed_provider_ids.insert(provider_id);
                }
                Err(err) => {
                    result.failed += 1;
                    tracing::warn!(
                        provider_id = provider_id,
                        endpoint_id = selected_endpoint_id,
                        err = %err,
                        "endpoint model catalog refresh failed"
                    );
                }
            }
        }

        if !result.refreshed_provider_ids.is_empty() {
            self.options_cache.lock().await.retain(|cache_key, _| {
                !result
                    .refreshed_provider_ids
                    .iter()
                    .any(|provider_id| cache_key_matches_provider(cache_key, provider_id))
            });
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ctx_providers::adapters::{ProviderHealth, ProviderStatus, ProviderUsability};

    use super::*;

    fn write_invalid_harness_registry(data_root: &std::path::Path) {
        let path = data_root
            .join("providers")
            .join("harness_sources")
            .join("registry.json");
        std::fs::create_dir_all(path.parent().expect("registry parent")).unwrap();
        std::fs::write(path, "{ not valid json").unwrap();
    }

    #[tokio::test]
    async fn endpoint_model_sweeper_counts_harness_config_load_failures() {
        let data_dir = tempfile::tempdir().unwrap();
        write_invalid_harness_registry(data_dir.path());
        let runtime = ProviderRuntime::new(HashMap::new());
        runtime
            .upsert_provider_status(
                "qwen".to_string(),
                ProviderStatus {
                    provider_id: "qwen".to_string(),
                    installed: true,
                    detected_path: None,
                    version: None,
                    capabilities: None,
                    health: ProviderHealth::Ok,
                    diagnostics: Vec::new(),
                    details: HashMap::new(),
                    usability: ProviderUsability::default(),
                },
            )
            .await;

        let result = runtime
            .refresh_stale_selected_endpoint_model_catalogs(data_dir.path())
            .await;

        assert_eq!(result.refreshed, 0);
        assert_eq!(result.failed, 1);
        assert!(result.refreshed_provider_ids.is_empty());
    }
}
