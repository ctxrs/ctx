//! Final-host configuration adapter for neutral history command bodies.

use std::path::Path;

use anyhow::Result;
use ctx_history_cli::{HistoryCliConfig, HistoryConfigPort, HistoryConfigSnapshotPort};

use ctx_app_config::{self as config, AppConfig};

/// Static bridge to the final binary's persisted configuration authority.
/// It intentionally performs no loading, environment lookup, or fallback.
pub(crate) struct CliHistoryConfigAdapter<'a> {
    data_root: &'a Path,
    config: &'a mut AppConfig,
}

/// Read-only import projection. It deliberately cannot persist or reload
/// configuration while command application is running.
pub(crate) struct CliHistoryConfigSnapshot<'a> {
    config: &'a AppConfig,
}

impl<'a> CliHistoryConfigSnapshot<'a> {
    pub(crate) const fn new(config: &'a AppConfig) -> Self {
        Self { config }
    }
}

impl HistoryConfigSnapshotPort for CliHistoryConfigSnapshot<'_> {
    fn snapshot(&self) -> HistoryCliConfig {
        HistoryCliConfig {
            daemon_enabled: self.config.automatic_indexing_enabled(),
            semantic_search_enabled: self.config.semantic_search_enabled(),
            semantic_executor: self.config.semantic_embedding_executor().clone(),
            local_usage_enabled: self.config.local_usage.enabled,
            automatic_provider_discovery: self.config.automatic_source_discovery_enabled(),
            provider_roots: self.config.provider_root_definitions(),
        }
    }
}

impl<'a> CliHistoryConfigAdapter<'a> {
    pub(crate) const fn new(data_root: &'a Path, config: &'a mut AppConfig) -> Self {
        Self { data_root, config }
    }
}

impl HistoryConfigPort for CliHistoryConfigAdapter<'_> {
    type Error = anyhow::Error;

    fn snapshot(&self) -> HistoryCliConfig {
        CliHistoryConfigSnapshot::new(self.config).snapshot()
    }

    fn write_default_config(&mut self) -> Result<(), Self::Error> {
        config::write_default_config(self.data_root)
    }

    fn set_semantic_search_enabled(&mut self, enabled: bool) -> Result<(), Self::Error> {
        config::set_semantic_search_enabled(self.data_root, enabled)?;
        self.config.apply_persisted_semantic_search_enabled(enabled);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn adapter_delegates_mutation_without_reloading_config() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default();
        config.semantic.executor = ctx_daemon_cli::SemanticEmbeddingExecutorConfig::http(
            "http://127.0.0.1:9",
            ctx_daemon_cli::ExternalSemanticSpace::new("test-space", 384).unwrap(),
        )
        .unwrap();
        let (snapshot, load_count) = config::count_app_config_loads(|| {
            let mut adapter = CliHistoryConfigAdapter::new(temp.path(), &mut config);

            adapter.write_default_config().unwrap();
            adapter.set_semantic_search_enabled(true).unwrap();
            HistoryConfigPort::snapshot(&adapter)
        });

        assert_eq!(
            fs::read_to_string(temp.path().join(config::CONFIG_FILE)).unwrap(),
            "[search]\nsemantic = true\n"
        );
        assert_eq!(load_count, 0);
        assert!(snapshot.semantic_search_enabled);
        assert_eq!(
            snapshot.semantic_executor.http_endpoint(),
            Some("http://127.0.0.1:9/")
        );
    }
}
