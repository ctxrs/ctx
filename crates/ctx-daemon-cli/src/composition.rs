use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

use anyhow::{anyhow, Result};
#[cfg(test)]
use ctx_app_config::CONFIG_FILE;
use ctx_client_observability::analytics::PublicEventV1;
use ctx_daemon_service::CoreGenerationPublished;
use ctx_semantic_model::{SemanticEmbeddingExecutorConfig, SemanticModelContract};

pub const DAEMON_DEFAULT_ENABLED: bool = true;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DaemonMode {
    #[default]
    Full,
    SourceRefreshOnly,
}

impl DaemonMode {
    pub const fn runs_only_source_refresh(self) -> bool {
        matches!(self, Self::SourceRefreshOnly)
    }
}

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub enabled: bool,
    pub mode: DaemonMode,
}

#[derive(Debug, Clone)]
pub struct AnalyticsConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct UpgradeConfig {
    automatic_upgrade_enabled: bool,
    pub channel: String,
    pub interval: Duration,
}

#[derive(Debug, Clone)]
pub struct DaemonRuntimeConfig {
    pub analytics: AnalyticsConfig,
    pub upgrade: UpgradeConfig,
    pub daemon: DaemonConfig,
    semantic_enabled: bool,
    semantic_source: &'static str,
    semantic_executor: SemanticEmbeddingExecutorConfig,
    automatic_provider_discovery: bool,
    provider_roots: Vec<ctx_history_capture::ProviderRootDefinition>,
}

impl DaemonRuntimeConfig {
    pub fn new(
        analytics_enabled: bool,
        automatic_upgrade_enabled: bool,
        upgrade_channel: String,
        upgrade_interval: Duration,
        daemon: DaemonConfig,
        semantic_enabled: bool,
        semantic_source: &'static str,
    ) -> Self {
        Self {
            analytics: AnalyticsConfig {
                enabled: analytics_enabled,
            },
            upgrade: UpgradeConfig {
                automatic_upgrade_enabled,
                channel: upgrade_channel,
                interval: upgrade_interval,
            },
            daemon,
            semantic_enabled,
            semantic_source,
            semantic_executor: SemanticEmbeddingExecutorConfig::builtin(),
            automatic_provider_discovery: true,
            provider_roots: Vec::new(),
        }
    }

    pub fn with_provider_roots(
        mut self,
        roots: Vec<ctx_history_capture::ProviderRootDefinition>,
    ) -> Self {
        self.provider_roots = roots;
        self
    }

    pub fn with_automatic_provider_discovery(mut self, enabled: bool) -> Self {
        self.automatic_provider_discovery = enabled;
        self
    }

    pub fn with_semantic_embedding_executor(
        mut self,
        executor: SemanticEmbeddingExecutorConfig,
    ) -> Self {
        self.semantic_executor = executor;
        self
    }

    pub fn provider_roots(&self) -> &[ctx_history_capture::ProviderRootDefinition] {
        &self.provider_roots
    }

    pub const fn automatic_provider_discovery_enabled(&self) -> bool {
        self.automatic_provider_discovery
    }

    pub const fn auto_upgrade_enabled(&self) -> bool {
        self.upgrade.automatic_upgrade_enabled
    }

    pub fn upgrade_channel(&self) -> &str {
        self.upgrade.channel.as_ref()
    }

    pub const fn semantic_search_enabled(&self) -> bool {
        self.semantic_enabled
    }

    pub const fn semantic_search_source(&self) -> &'static str {
        self.semantic_source
    }

    pub fn semantic_embedding_executor(&self) -> &SemanticEmbeddingExecutorConfig {
        &self.semantic_executor
    }

    pub fn semantic_model_contract(&self) -> &SemanticModelContract {
        self.semantic_executor.contract()
    }
}

impl Default for DaemonRuntimeConfig {
    fn default() -> Self {
        Self::new(
            true,
            true,
            "stable".to_owned(),
            Duration::from_secs(24 * 60 * 60),
            DaemonConfig {
                enabled: true,
                mode: DaemonMode::Full,
            },
            false,
            "default",
        )
    }
}

pub trait DaemonCliHost: Send + Sync {
    fn load_config(&self, data_root: &Path) -> Result<DaemonRuntimeConfig>;
    fn persisted_daemon_enabled(&self, data_root: &Path) -> Result<bool>;
    fn set_daemon_enabled(&self, data_root: &Path, enabled: bool) -> Result<()>;
    fn home_dir(&self) -> Option<PathBuf>;
    fn run_daemon_service(
        &self,
        data_root: &Path,
        request: crate::DaemonHostRunRequest,
        config: &DaemonRuntimeConfig,
    ) -> Result<()>;
    fn deliver_daemon_events(&self, data_root: &Path, events: &[PublicEventV1]);
    fn fetch_to_writer(
        &self,
        endpoint: &str,
        max_bytes: u64,
        timeout: Duration,
        writer: &mut dyn Write,
    ) -> Result<u64>;
    /// Neutral post-publication seam for companion composition. The default is
    /// intentionally a no-op until a companion bridge is installed.
    fn core_generation_published(
        &self,
        _data_root: &Path,
        _publication: &CoreGenerationPublished,
    ) -> Result<()> {
        Ok(())
    }
}

pub(crate) fn load_runtime_config(data_root: &Path) -> Result<DaemonRuntimeConfig> {
    host().load_config(data_root)
}

static HOST: OnceLock<&'static dyn DaemonCliHost> = OnceLock::new();

pub fn install_host(host: &'static dyn DaemonCliHost) -> Result<()> {
    if let Some(installed) = HOST.get() {
        if std::ptr::eq(*installed, host) {
            return Ok(());
        }
        return Err(anyhow!("ctx daemon CLI host is already installed"));
    }
    HOST.set(host)
        .map_err(|_| anyhow!("ctx daemon CLI host is already installed"))
}

pub(crate) fn host() -> &'static dyn DaemonCliHost {
    if let Some(host) = HOST.get().copied() {
        return host;
    }
    #[cfg(test)]
    {
        &TEST_HOST
    }
    #[cfg(not(test))]
    panic!("ctx daemon CLI host must be installed before adapter use")
}

#[cfg(test)]
struct TestHost;

#[cfg(test)]
static TEST_HOST: TestHost = TestHost;

#[cfg(test)]
impl DaemonCliHost for TestHost {
    fn load_config(&self, data_root: &Path) -> Result<DaemonRuntimeConfig> {
        let config = ctx_app_config::AppConfig::load(data_root)?;
        let daemon_enabled = config.automatic_indexing_enabled();
        let daemon_mode = match config.daemon.mode {
            ctx_app_config::DaemonMode::Full => DaemonMode::Full,
            ctx_app_config::DaemonMode::SourceRefreshOnly => DaemonMode::SourceRefreshOnly,
        };
        let semantic_enabled = config.semantic_search_enabled();
        let semantic_source = config.semantic_search_source();
        let semantic_executor = config.semantic_embedding_executor().clone();
        let automatic_provider_discovery = config.automatic_source_discovery_enabled();
        let provider_roots = config.provider_root_definitions();
        Ok(DaemonRuntimeConfig::new(
            config.analytics.enabled,
            config.auto_upgrade_enabled(),
            config.upgrade.channel,
            config.upgrade.interval,
            DaemonConfig {
                enabled: daemon_enabled,
                mode: daemon_mode,
            },
            semantic_enabled,
            semantic_source,
        )
        .with_semantic_embedding_executor(semantic_executor)
        .with_automatic_provider_discovery(automatic_provider_discovery)
        .with_provider_roots(provider_roots))
    }

    fn persisted_daemon_enabled(&self, data_root: &Path) -> Result<bool> {
        Ok(self.load_config(data_root)?.daemon.enabled)
    }

    fn set_daemon_enabled(&self, data_root: &Path, enabled: bool) -> Result<()> {
        ctx_app_config::set_daemon_enabled(data_root, enabled)
    }

    fn home_dir(&self) -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }

    fn run_daemon_service(
        &self,
        _data_root: &Path,
        _request: crate::DaemonHostRunRequest,
        _config: &DaemonRuntimeConfig,
    ) -> Result<()> {
        Err(anyhow!("test daemon service host is unavailable"))
    }

    fn deliver_daemon_events(&self, _data_root: &Path, _events: &[PublicEventV1]) {}

    fn fetch_to_writer(
        &self,
        _endpoint: &str,
        _max_bytes: u64,
        _timeout: Duration,
        _writer: &mut dyn Write,
    ) -> Result<u64> {
        Err(anyhow!("test artifact fetcher is unavailable"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_tracks_the_shipped_one_key_semantic_executor_contract() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(CONFIG_FILE);

        std::fs::write(
            &path,
            "[semantic]\nexecutor = \"https://embed.example.test/base\"\nspace_id = \"acme/multilingual-v2\"\ndimensions = 768\n",
        )
        .unwrap();
        let external = load_runtime_config(temp.path()).unwrap();
        assert_eq!(
            external.semantic_embedding_executor().http_endpoint(),
            Some("https://embed.example.test/base/")
        );
        let space = external
            .semantic_embedding_executor()
            .external_space()
            .unwrap();
        assert_eq!(space.space_id(), "acme/multilingual-v2");
        assert_eq!(space.dimensions(), 768);

        std::fs::write(&path, "[semantic]\nexecutor = \"builtin\"\n").unwrap();
        assert!(load_runtime_config(temp.path())
            .unwrap()
            .semantic_embedding_executor()
            .is_builtin());

        std::fs::write(&path, "[search]\nsemantic = true\n").unwrap();
        assert!(load_runtime_config(temp.path())
            .unwrap()
            .semantic_embedding_executor()
            .is_builtin());

        for retired in [
            "[semantic]\nexecutor = \"http\"\nendpoint = \"https://embed.example.test\"\n",
            "[semantic]\nendpoint = \"https://embed.example.test\"\n",
        ] {
            std::fs::write(&path, retired).unwrap();
            let error = load_runtime_config(temp.path()).unwrap_err();
            assert!(
                format!("{error:#}").contains("unknown config key `semantic.endpoint`"),
                "{error:#}"
            );
        }

        std::fs::write(
            &path,
            "[semantic]\nexecutor = \"http\"\nspace_id = \"space-v1\"\ndimensions = 384\n",
        )
        .unwrap();
        let error = load_runtime_config(temp.path()).unwrap_err();
        assert!(
            format!("{error:#}").contains("endpoint is invalid"),
            "{error:#}"
        );

        std::fs::write(
            &path,
            "[semantic]\nexecutor = \"https://embed.example.test\"\n",
        )
        .unwrap();
        let legacy = load_runtime_config(temp.path()).unwrap();
        assert!(legacy.semantic_embedding_executor().is_legacy_fixed_http());

        for incomplete in [
            "[semantic]\nspace_id = \"space-v1\"\ndimensions = 384\n",
            "[semantic]\nexecutor = \"builtin\"\nspace_id = \"space-v1\"\ndimensions = 384\n",
        ] {
            std::fs::write(&path, incomplete).unwrap();
            assert!(
                load_runtime_config(temp.path()).is_err(),
                "accepted {incomplete}"
            );
        }
    }
}
