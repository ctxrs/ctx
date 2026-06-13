use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use ctx_provider_matrix::{MatrixRefreshOutcome, ProviderMatrix, ProviderMatrixEntryKind};
use ctx_providers::adapters::{ProviderAdapter, ProviderStatus};

use crate::ProviderRuntime;

impl ProviderRuntime {
    pub async fn inspect_provider_adapters(&self) -> Vec<(String, Result<ProviderStatus, String>)> {
        let adapters = self.adapters.lock().await;
        let mut statuses = Vec::with_capacity(adapters.len());
        for (id, adapter) in adapters.iter() {
            statuses.push((
                id.clone(),
                adapter.inspect().await.map_err(|err| err.to_string()),
            ));
        }
        statuses
    }

    pub async fn upsert_provider_adapter(
        &self,
        provider_id: String,
        adapter: Arc<dyn ProviderAdapter>,
    ) {
        self.adapters.lock().await.insert(provider_id, adapter);
    }

    pub async fn provider_adapter(&self, provider_id: &str) -> Option<Arc<dyn ProviderAdapter>> {
        self.adapters.lock().await.get(provider_id).cloned()
    }

    pub async fn has_provider_adapter(&self, provider_id: &str) -> bool {
        self.adapters.lock().await.contains_key(provider_id)
    }

    pub async fn can_create_loaded_session_for_provider(&self, provider_id: &str) -> bool {
        self.has_provider_adapter(provider_id).await
    }

    pub async fn provider_adapter_count(&self) -> usize {
        self.adapters.lock().await.len()
    }

    pub async fn provider_adapter_entries(&self) -> Vec<(String, Arc<dyn ProviderAdapter>)> {
        self.adapters
            .lock()
            .await
            .iter()
            .map(|(id, adapter)| (id.clone(), Arc::clone(adapter)))
            .collect()
    }

    pub async fn upsert_target_provider_adapter(
        &self,
        cache_key: String,
        adapter: Arc<dyn ProviderAdapter>,
    ) {
        self.target_adapters.lock().await.insert(cache_key, adapter);
    }

    pub async fn target_provider_adapter(
        &self,
        cache_key: &str,
    ) -> Option<Arc<dyn ProviderAdapter>> {
        self.target_adapters.lock().await.get(cache_key).cloned()
    }

    pub async fn has_target_provider_adapter(&self, cache_key: &str) -> bool {
        self.target_adapters.lock().await.contains_key(cache_key)
    }

    pub async fn target_provider_adapter_count(&self) -> usize {
        self.target_adapters.lock().await.len()
    }

    pub async fn target_provider_adapter_entries(&self) -> Vec<(String, Arc<dyn ProviderAdapter>)> {
        self.target_adapters
            .lock()
            .await
            .iter()
            .map(|(id, adapter)| (id.clone(), Arc::clone(adapter)))
            .collect()
    }

    pub async fn all_provider_adapter_entries(&self) -> Vec<(String, Arc<dyn ProviderAdapter>)> {
        let mut adapters = self.provider_adapter_entries().await;
        adapters.extend(self.target_provider_adapter_entries().await);
        adapters
    }

    pub async fn provider_adapter_entries_for_provider(
        &self,
        provider_id: &str,
    ) -> Vec<(String, Arc<dyn ProviderAdapter>)> {
        let mut adapters = Vec::new();
        if let Some(adapter) = self.provider_adapter(provider_id).await {
            adapters.push((provider_id.to_string(), adapter));
        }
        let target_prefix = format!("{provider_id}@");
        adapters.extend(
            self.target_provider_adapter_entries()
                .await
                .into_iter()
                .filter(|(id, _)| id.starts_with(&target_prefix)),
        );
        adapters
    }

    pub async fn replace_provider_statuses(&self, statuses: HashMap<String, ProviderStatus>) {
        *self.statuses.lock().await = statuses;
    }

    pub async fn upsert_provider_status(&self, provider_id: String, status: ProviderStatus) {
        self.statuses.lock().await.insert(provider_id, status);
    }

    pub async fn provider_status(&self, provider_id: &str) -> Option<ProviderStatus> {
        self.statuses.lock().await.get(provider_id).cloned()
    }

    pub async fn has_provider_status(&self, provider_id: &str) -> bool {
        self.statuses.lock().await.contains_key(provider_id)
    }

    pub async fn is_known_provider_id(&self, matrix: &ProviderMatrix, provider_id: &str) -> bool {
        self.has_provider_status(provider_id).await
            || ctx_provider_matrix::get_entry(matrix, provider_id).is_some()
    }

    pub async fn is_configurable_provider_id(
        &self,
        matrix: &ProviderMatrix,
        provider_id: &str,
    ) -> bool {
        self.is_known_provider_id(matrix, provider_id).await
            || self.has_provider_adapter(provider_id).await
    }

    pub async fn visible_provider_status_ids(
        &self,
        matrix: &ProviderMatrix,
        include_matrix_providers: bool,
    ) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut provider_ids = Vec::new();
        for provider_id in self.provider_status_ids().await {
            if !ctx_provider_matrix::is_user_facing_harness_id(matrix, &provider_id) {
                continue;
            }
            if seen.insert(provider_id.clone()) {
                provider_ids.push(provider_id);
            }
        }
        if include_matrix_providers {
            for entry in &matrix.providers {
                if entry.kind != ProviderMatrixEntryKind::Harness {
                    continue;
                }
                if seen.insert(entry.id.clone()) {
                    provider_ids.push(entry.id.clone());
                }
            }
        }
        provider_ids
    }

    pub async fn known_harness_provider_ids(&self, matrix: &ProviderMatrix) -> HashSet<String> {
        let mut provider_ids = self
            .provider_status_ids()
            .await
            .into_iter()
            .collect::<HashSet<_>>();
        for entry in &matrix.providers {
            if entry.kind == ProviderMatrixEntryKind::Harness {
                provider_ids.insert(entry.id.clone());
            }
        }
        provider_ids
    }

    pub async fn provider_status_count(&self) -> usize {
        self.statuses.lock().await.len()
    }

    pub async fn provider_status_ids(&self) -> Vec<String> {
        self.statuses.lock().await.keys().cloned().collect()
    }

    pub async fn provider_statuses(&self) -> Vec<ProviderStatus> {
        self.statuses.lock().await.values().cloned().collect()
    }

    pub async fn load_provider_matrix(&self, data_root: &Path) -> ProviderMatrix {
        ctx_provider_matrix::load_matrix_cached(data_root, &self.matrix_cache).await
    }

    pub async fn invalidate_provider_matrix_cache(&self) {
        ctx_provider_matrix::invalidate_matrix_cache(&self.matrix_cache).await;
    }

    pub async fn refresh_provider_matrix_from_local_sources(
        &self,
        data_root: &Path,
    ) -> MatrixRefreshOutcome {
        self.invalidate_provider_matrix_cache().await;
        ctx_provider_matrix::refresh_matrix_from_local_sources(data_root, &self.matrix_cache).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ctx_provider_matrix::{ProviderMatrix, ProviderMatrixEntry, ProviderMatrixEntryKind};
    use ctx_providers::adapters::{ProviderHealth, ProviderStatus, ProviderUsability};

    use super::*;

    fn test_matrix(entries: &[(&str, ProviderMatrixEntryKind)]) -> ProviderMatrix {
        ProviderMatrix {
            version: 3,
            generated_at: None,
            providers: entries
                .iter()
                .map(|(provider_id, kind)| ProviderMatrixEntry {
                    id: (*provider_id).to_string(),
                    kind: *kind,
                    display_name: None,
                    tier: None,
                    command: None,
                    managed_install: None,
                    provider_dependencies: Vec::new(),
                    dependencies: Vec::new(),
                    version_probe: None,
                    releases: Vec::new(),
                })
                .collect(),
        }
    }

    fn provider_status(provider_id: &str) -> ProviderStatus {
        ProviderStatus {
            provider_id: provider_id.to_string(),
            installed: true,
            detected_path: None,
            version: None,
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ProviderUsability::default(),
        }
    }

    #[tokio::test]
    async fn known_provider_id_accepts_status_or_matrix_entry() {
        let runtime = ProviderRuntime::new(HashMap::new());
        let matrix = test_matrix(&[("matrix-provider", ProviderMatrixEntryKind::Harness)]);
        runtime
            .upsert_provider_status(
                "status-provider".to_string(),
                provider_status("status-provider"),
            )
            .await;

        assert!(
            runtime
                .is_known_provider_id(&matrix, "status-provider")
                .await
        );
        assert!(
            runtime
                .is_known_provider_id(&matrix, "matrix-provider")
                .await
        );
        assert!(!runtime.is_known_provider_id(&matrix, "missing").await);
    }

    #[tokio::test]
    async fn configurable_provider_id_accepts_adapter_presence() {
        let adapter = Arc::new(ctx_providers::fake::FakeProviderAdapter::new());
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("adapter-provider".to_string(), adapter);
        let runtime = ProviderRuntime::new(providers);
        let matrix = test_matrix(&[]);

        assert!(
            runtime
                .is_configurable_provider_id(&matrix, "adapter-provider")
                .await
        );
        assert!(
            !runtime
                .is_configurable_provider_id(&matrix, "missing")
                .await
        );
    }

    #[tokio::test]
    async fn loaded_session_provider_requires_root_adapter() {
        let adapter = Arc::new(ctx_providers::fake::FakeProviderAdapter::new());
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("adapter-provider".to_string(), adapter);
        let runtime = ProviderRuntime::new(providers);

        assert!(
            runtime
                .can_create_loaded_session_for_provider("adapter-provider")
                .await
        );
        assert!(
            !runtime
                .can_create_loaded_session_for_provider("missing-provider")
                .await
        );
    }

    #[tokio::test]
    async fn visible_provider_status_ids_filter_statuses_and_optionally_include_matrix() {
        let runtime = ProviderRuntime::new(HashMap::new());
        let matrix = test_matrix(&[
            ("codex", ProviderMatrixEntryKind::Harness),
            ("codex-crp", ProviderMatrixEntryKind::Dependency),
            ("gemini", ProviderMatrixEntryKind::Harness),
        ]);
        runtime
            .upsert_provider_status("codex".to_string(), provider_status("codex"))
            .await;
        runtime
            .upsert_provider_status("codex-crp".to_string(), provider_status("codex-crp"))
            .await;
        runtime
            .upsert_provider_status("local-only".to_string(), provider_status("local-only"))
            .await;

        let status_only = runtime.visible_provider_status_ids(&matrix, false).await;
        assert_eq!(
            status_only.into_iter().collect::<HashSet<_>>(),
            HashSet::from(["codex".to_string(), "local-only".to_string()])
        );
        let with_matrix = runtime.visible_provider_status_ids(&matrix, true).await;
        assert_eq!(
            with_matrix.into_iter().collect::<HashSet<_>>(),
            HashSet::from([
                "codex".to_string(),
                "local-only".to_string(),
                "gemini".to_string()
            ])
        );
    }

    #[tokio::test]
    async fn known_harness_provider_ids_include_statuses_and_matrix_harness_entries() {
        let runtime = ProviderRuntime::new(HashMap::new());
        let matrix = test_matrix(&[
            ("gemini", ProviderMatrixEntryKind::Harness),
            ("codex-crp", ProviderMatrixEntryKind::Dependency),
        ]);
        runtime
            .upsert_provider_status(
                "status-provider".to_string(),
                provider_status("status-provider"),
            )
            .await;

        let provider_ids = runtime.known_harness_provider_ids(&matrix).await;

        assert!(provider_ids.contains("status-provider"));
        assert!(provider_ids.contains("gemini"));
        assert!(!provider_ids.contains("codex-crp"));
    }
}
