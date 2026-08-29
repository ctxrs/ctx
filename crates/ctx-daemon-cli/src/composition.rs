use std::{
    borrow::Cow,
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

use anyhow::{anyhow, Result};
use ctx_client_observability::analytics::PublicEventV1;
use ctx_daemon_service::CoreGenerationPublished;
use ctx_semantic_model::SemanticEmbeddingExecutorConfig;

pub const CONFIG_FILE: &str = "config.toml";
pub const DAEMON_DEFAULT_ENABLED: bool = true;
#[cfg(test)]
pub const DAEMON_MODE_ENV: &str = "CTX_DAEMON_MODE";

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
pub struct UpgradeConfig<'a> {
    automatic_upgrade_enabled: bool,
    pub channel: Cow<'a, str>,
    pub interval: Duration,
}

#[derive(Debug, Clone)]
pub struct AppConfig<'a> {
    pub analytics: AnalyticsConfig,
    pub upgrade: UpgradeConfig<'a>,
    pub daemon: DaemonConfig,
    semantic_enabled: bool,
    semantic_source: &'static str,
    semantic_executor: SemanticEmbeddingExecutorConfig,
    automatic_provider_discovery: bool,
    provider_roots: Vec<ctx_history_capture::ProviderRootDefinition>,
}

impl<'a> AppConfig<'a> {
    pub fn new(
        analytics_enabled: bool,
        automatic_upgrade_enabled: bool,
        upgrade_channel: Cow<'a, str>,
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

    pub fn load(data_root: &Path) -> Result<AppConfig<'static>> {
        host().load_config(data_root)
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
}

impl Default for AppConfig<'static> {
    fn default() -> Self {
        Self::new(
            true,
            true,
            Cow::Borrowed("stable"),
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
    fn load_config(&self, data_root: &Path) -> Result<AppConfig<'static>>;
    fn persisted_daemon_enabled(&self, data_root: &Path) -> Result<bool>;
    fn set_daemon_enabled(&self, data_root: &Path, enabled: bool) -> Result<()>;
    fn home_dir(&self) -> Option<PathBuf>;
    fn run_daemon_service(
        &self,
        data_root: &Path,
        request: crate::DaemonHostRunRequest,
        config: &AppConfig<'_>,
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

pub fn set_daemon_enabled(data_root: &Path, enabled: bool) -> Result<()> {
    host().set_daemon_enabled(data_root, enabled)
}

pub fn persisted_daemon_enabled(data_root: &Path) -> Result<bool> {
    host().persisted_daemon_enabled(data_root)
}

#[cfg(test)]
struct TestHost;

#[cfg(test)]
static TEST_HOST: TestHost = TestHost;

#[cfg(test)]
impl TestHost {
    fn parsed_config(&self, data_root: &Path) -> Result<toml_edit::DocumentMut> {
        let path = data_root.join(CONFIG_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| anyhow!("parse {}: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(toml_edit::DocumentMut::new())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn config_item<'a>(
        document: &'a toml_edit::DocumentMut,
        table: &str,
        key: &str,
    ) -> Option<&'a toml_edit::Item> {
        document
            .as_table()
            .get(table)
            .and_then(toml_edit::Item::as_table)
            .and_then(|table| table.get(key))
    }
}

#[cfg(test)]
impl DaemonCliHost for TestHost {
    fn load_config(&self, data_root: &Path) -> Result<AppConfig<'static>> {
        let document = self.parsed_config(data_root)?;
        let mut config = AppConfig::default();
        if let Some(enabled) =
            Self::config_item(&document, "analytics", "enabled").and_then(toml_edit::Item::as_bool)
        {
            config.analytics.enabled = enabled;
        }
        if let Some(enabled) =
            Self::config_item(&document, "daemon", "enabled").and_then(toml_edit::Item::as_bool)
        {
            config.daemon.enabled = enabled;
        }
        if let Some(mode) =
            Self::config_item(&document, "indexing", "mode").and_then(toml_edit::Item::as_str)
        {
            config.daemon.enabled = match mode {
                "auto" | "automatic" => true,
                "manual" => false,
                _ => return Err(anyhow!("unknown indexing mode `{mode}`")),
            };
        }
        if let Some(mode) =
            Self::config_item(&document, "daemon", "mode").and_then(toml_edit::Item::as_str)
        {
            config.daemon.mode = match mode {
                "full" => DaemonMode::Full,
                "source-refresh-only" => DaemonMode::SourceRefreshOnly,
                _ => return Err(anyhow!("unknown daemon mode `{mode}`")),
            };
        }
        if let Some(enabled) =
            Self::config_item(&document, "search", "semantic").and_then(toml_edit::Item::as_bool)
        {
            config.semantic_enabled = enabled;
            config.semantic_source = "config";
        }
        let executor =
            Self::config_item(&document, "semantic", "executor").and_then(toml_edit::Item::as_str);
        if Self::config_item(&document, "semantic", "endpoint").is_some() {
            return Err(anyhow!("unknown config key `semantic.endpoint`"));
        }
        config.semantic_executor = match executor {
            None | Some("builtin") => SemanticEmbeddingExecutorConfig::builtin(),
            Some(endpoint) => SemanticEmbeddingExecutorConfig::http(endpoint)?,
        };
        Ok(config)
    }

    fn persisted_daemon_enabled(&self, data_root: &Path) -> Result<bool> {
        Ok(self.load_config(data_root)?.daemon.enabled)
    }

    fn set_daemon_enabled(&self, data_root: &Path, enabled: bool) -> Result<()> {
        std::fs::create_dir_all(data_root)?;
        let mut document = self.parsed_config(data_root)?;
        if document.as_table().get("indexing").is_none() {
            document
                .as_table_mut()
                .insert("indexing", toml_edit::table());
        }
        let indexing = document
            .as_table_mut()
            .get_mut("indexing")
            .and_then(toml_edit::Item::as_table_mut)
            .ok_or_else(|| anyhow!("indexing configuration must be a table"))?;
        indexing.insert(
            "mode",
            toml_edit::value(if enabled { "auto" } else { "manual" }),
        );
        if let Some(daemon) = document
            .as_table_mut()
            .get_mut("daemon")
            .and_then(toml_edit::Item::as_table_mut)
        {
            daemon.remove("enabled");
        }
        std::fs::write(data_root.join(CONFIG_FILE), document.to_string())?;
        Ok(())
    }

    fn home_dir(&self) -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }

    fn run_daemon_service(
        &self,
        _data_root: &Path,
        _request: crate::DaemonHostRunRequest,
        _config: &AppConfig<'_>,
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
            "[semantic]\nexecutor = \"https://embed.example.test/base\"\n",
        )
        .unwrap();
        let external = AppConfig::load(temp.path()).unwrap();
        assert_eq!(
            external.semantic_embedding_executor().http_endpoint(),
            Some("https://embed.example.test/base/")
        );

        std::fs::write(&path, "[semantic]\nexecutor = \"builtin\"\n").unwrap();
        assert!(AppConfig::load(temp.path())
            .unwrap()
            .semantic_embedding_executor()
            .is_builtin());

        std::fs::write(&path, "[search]\nsemantic = true\n").unwrap();
        assert!(AppConfig::load(temp.path())
            .unwrap()
            .semantic_embedding_executor()
            .is_builtin());

        for retired in [
            "[semantic]\nexecutor = \"http\"\nendpoint = \"https://embed.example.test\"\n",
            "[semantic]\nendpoint = \"https://embed.example.test\"\n",
        ] {
            std::fs::write(&path, retired).unwrap();
            let error = AppConfig::load(temp.path()).unwrap_err();
            assert!(
                format!("{error:#}").contains("unknown config key `semantic.endpoint`"),
                "{error:#}"
            );
        }

        std::fs::write(&path, "[semantic]\nexecutor = \"http\"\n").unwrap();
        let error = AppConfig::load(temp.path()).unwrap_err();
        assert!(
            format!("{error:#}").contains("endpoint is invalid"),
            "{error:#}"
        );
    }
}
