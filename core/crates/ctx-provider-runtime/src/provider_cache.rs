use std::collections::HashMap;
use std::time::Instant;

use ctx_core::ids::WorkspaceId;
use ctx_provider_install::install_state::InstallTarget;
use serde_json::Value;

use crate::{provider_usage, CachedProviderOptions, CachedProviderVerify, ProviderRuntime};

#[derive(Debug, Clone)]
pub struct CachedProviderJsonSnapshot {
    pub cached_at: Instant,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRuntimeCacheStats {
    pub adapters: usize,
    pub statuses: usize,
    pub options_cache: usize,
    pub verify_cache: usize,
    pub usage_cache: usize,
    pub installs: usize,
}

impl ProviderRuntime {
    pub async fn with_provider_options_cache<R>(
        &self,
        f: impl FnOnce(&mut HashMap<String, CachedProviderOptions>) -> R,
    ) -> R {
        let mut cache = self.options_cache.lock().await;
        f(&mut cache)
    }

    pub async fn with_provider_verify_cache<R>(
        &self,
        f: impl FnOnce(&mut HashMap<String, CachedProviderVerify>) -> R,
    ) -> R {
        let mut cache = self.verify_cache.lock().await;
        f(&mut cache)
    }

    pub async fn with_provider_usage_cache<R>(
        &self,
        f: impl FnOnce(&mut HashMap<String, provider_usage::ProviderUsageSnapshot>) -> R,
    ) -> R {
        let mut cache = self.usage_cache.lock().await;
        f(&mut cache)
    }

    pub async fn provider_options_cache_entry(
        &self,
        cache_key: &str,
    ) -> Option<CachedProviderJsonSnapshot> {
        self.options_cache
            .lock()
            .await
            .get(cache_key)
            .map(|entry| CachedProviderJsonSnapshot {
                cached_at: entry.cached_at,
                value: entry.value.clone(),
            })
    }

    pub async fn provider_verify_cache_entry(
        &self,
        cache_key: &str,
    ) -> Option<CachedProviderJsonSnapshot> {
        self.verify_cache
            .lock()
            .await
            .get(cache_key)
            .map(|entry| CachedProviderJsonSnapshot {
                cached_at: entry.cached_at,
                value: entry.value.clone(),
            })
    }

    pub async fn store_provider_options_cache_value(&self, cache_key: String, value: Value) {
        self.options_cache.lock().await.insert(
            cache_key,
            CachedProviderOptions {
                cached_at: Instant::now(),
                value,
            },
        );
    }

    pub async fn store_provider_verify_cache_value(&self, cache_key: String, value: Value) {
        self.verify_cache.lock().await.insert(
            cache_key,
            CachedProviderVerify {
                cached_at: Instant::now(),
                value,
            },
        );
    }

    pub async fn provider_usage_cache_entry(
        &self,
        provider_id: &str,
    ) -> Option<provider_usage::ProviderUsageSnapshot> {
        self.usage_cache.lock().await.get(provider_id).cloned()
    }

    pub async fn cache_stats(&self) -> ProviderRuntimeCacheStats {
        ProviderRuntimeCacheStats {
            adapters: self.adapters.lock().await.len(),
            statuses: self.statuses.lock().await.len(),
            options_cache: self.options_cache.lock().await.len(),
            verify_cache: self.verify_cache.lock().await.len(),
            usage_cache: self.usage_cache.lock().await.len(),
            installs: self.installs.lock().await.len(),
        }
    }
}

pub fn workspace_provider_cache_key(
    workspace_id: WorkspaceId,
    target: InstallTarget,
    provider_id: &str,
) -> String {
    format!("{}/{}/{}", workspace_id.0, target.as_str(), provider_id)
}

pub fn cache_key_matches_provider(cache_key: &str, provider_id: &str) -> bool {
    cache_key
        .rsplit_once('/')
        .is_some_and(|(_, key_provider)| key_provider == provider_id)
}

pub async fn invalidate_provider_probe_caches(runtime: &ProviderRuntime, provider_id: &str) {
    runtime
        .options_cache
        .lock()
        .await
        .retain(|cache_key, _| !cache_key_matches_provider(cache_key, provider_id));
    runtime
        .verify_cache
        .lock()
        .await
        .retain(|cache_key, _| !cache_key_matches_provider(cache_key, provider_id));
}

pub async fn invalidate_workspace_provider_options_cache(
    runtime: &ProviderRuntime,
    workspace_id: WorkspaceId,
    provider_id: &str,
) {
    let key_prefix = format!("{}/", workspace_id.0);
    let key_suffix = format!("/{provider_id}");
    runtime
        .options_cache
        .lock()
        .await
        .retain(|key, _| !(key.starts_with(&key_prefix) && key.ends_with(&key_suffix)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provider_json_cache_entries_round_trip_through_owner_apis() {
        let runtime = ProviderRuntime::new(HashMap::new());

        runtime
            .store_provider_options_cache_value(
                "workspace/host/codex".to_string(),
                serde_json::json!({"models": []}),
            )
            .await;
        runtime
            .store_provider_verify_cache_value(
                "workspace/host/codex".to_string(),
                serde_json::json!({"status": "ok"}),
            )
            .await;

        let options = runtime
            .provider_options_cache_entry("workspace/host/codex")
            .await
            .expect("options cache entry");
        assert_eq!(options.value, serde_json::json!({"models": []}));
        let verify = runtime
            .provider_verify_cache_entry("workspace/host/codex")
            .await
            .expect("verify cache entry");
        assert_eq!(verify.value, serde_json::json!({"status": "ok"}));
        assert!(runtime
            .provider_options_cache_entry("workspace/host/missing")
            .await
            .is_none());

        let stats = runtime.cache_stats().await;
        assert_eq!(stats.options_cache, 1);
        assert_eq!(stats.verify_cache, 1);
    }

    #[test]
    fn cache_key_provider_match_only_checks_provider_segment() {
        assert!(cache_key_matches_provider("workspace/host/codex", "codex"));
        assert!(!cache_key_matches_provider(
            "workspace/host/codex-crp",
            "codex"
        ));
        assert!(!cache_key_matches_provider(
            "workspace/host/codex/extra",
            "codex"
        ));
        assert!(!cache_key_matches_provider("codex", "codex"));
        assert!(!cache_key_matches_provider("not-a-key", "codex"));
    }
}
