use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use ctx_history_capture::{
    provider_paths_equivalent, ProviderRootDefinition, ProviderRootKind,
    MAX_CONFIGURED_PROVIDER_ROOTS,
};
use ctx_history_cli::parse_capture_provider_name;
use ctx_history_core::CaptureProvider;
use ctx_history_platform::platform_security::{
    establish_private_data_root, validate_provider_source_outside_data_root,
};

mod durable_write;
mod mutation;
mod provider_roots;
mod toml_subset;

#[cfg(test)]
pub(crate) use mutation::add_claude_root;
pub(crate) use mutation::{
    add_provider_root_with_kind, configure_manual_semantic_search, persisted_daemon_enabled,
    remove_provider_root, set_daemon_enabled, set_semantic_search_enabled, write_default_config,
    ProviderRootMutation,
};

use crate::deprecated_controls::DeprecatedControls;
use durable_write::{write_config_durably, ConfigMutationLock};
use provider_roots::{
    validate_provider_root_existing_kind, validate_provider_root_kind, validate_provider_root_path,
    validate_provider_root_support, validate_root_selector,
};
use toml_subset::*;

pub const CONFIG_FILE: &str = "config.toml";
pub const AUTO_UPGRADE_DEFAULT_MODE: &str = "apply";
pub const DAEMON_MODE_ENV: &str = "CTX_DAEMON_MODE";
pub const LOCAL_USAGE_DEFAULT_ENABLED: bool = true;
pub const SEMANTIC_SEARCH_DEFAULT_ENABLED: bool = false;

pub(crate) fn normalized_analytics_environment_override() -> Option<bool> {
    let deprecated_controls = DeprecatedControls::detect();
    if deprecated_controls.disables_analytics() {
        return Some(false);
    }
    env::var_os("CTX_ANALYTICS_ENABLED")
        .map(|value| value.to_str().and_then(parse_bool_value).unwrap_or(false))
}

pub(crate) fn resolved_analytics_consent(config: &AppConfig) -> bool {
    config.analytics.enabled && normalized_analytics_environment_override() != Some(false)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IndexingMode {
    #[default]
    Automatic,
    Manual,
}

impl IndexingMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "auto",
            Self::Manual => "manual",
        }
    }

    pub const fn is_automatic(self) -> bool {
        matches!(self, Self::Automatic)
    }

    const fn from_legacy_daemon_enabled(enabled: bool) -> Self {
        if enabled {
            Self::Automatic
        } else {
            Self::Manual
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown config key `cloud.mode`: cloud history configuration is no longer supported")]
struct RemovedCloudModeConfigError;

pub(crate) fn is_removed_cloud_mode_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<RemovedCloudModeConfigError>()
        .is_some()
}

#[cfg(test)]
pub(crate) static TEST_LOCAL_USAGE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoUpgradeMode {
    Apply,
    Off,
}

impl AutoUpgradeMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Off => "off",
        }
    }

    pub const fn enabled(self) -> bool {
        matches!(self, Self::Apply)
    }
}

/// Selects which production daemon scheduler surface is active.
///
/// Configure this with `daemon.mode = "source-refresh-only"` or the
/// `CTX_DAEMON_MODE=source-refresh-only` environment override. Autostart
/// explicitly propagates the effective environment value to its child.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DaemonMode {
    #[default]
    Full,
    SourceRefreshOnly,
}

impl DaemonMode {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "full" => Some(Self::Full),
            "source-refresh-only" => Some(Self::SourceRefreshOnly),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub analytics: AnalyticsConfig,
    pub local_usage: LocalUsageConfig,
    pub upgrade: UpgradeConfig,
    pub indexing: IndexingConfig,
    pub daemon: DaemonConfig,
    pub search: SearchConfig,
    pub sources: SourcesConfig,
    pub provider_roots: BTreeMap<String, ProviderRootDefinition>,
}

#[derive(Debug, Clone)]
pub struct AnalyticsConfig {
    pub enabled: bool,
    pub endpoint: String,
}

#[derive(Debug, Clone)]
pub struct LocalUsageConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalUsageEnvOverride {
    Unset,
    Enabled,
    Disabled,
    Invalid,
}

impl LocalUsageEnvOverride {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Unset => "none",
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::Invalid => "invalid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalUsageControl {
    pub(crate) persisted_enabled: bool,
    pub(crate) effective_enabled: bool,
    pub(crate) environment_override: LocalUsageEnvOverride,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalUsageConfigState {
    Resolved(bool),
    Malformed,
    Unresolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalUsageResolution {
    pub(crate) config_state: LocalUsageConfigState,
    pub(crate) environment_override: LocalUsageEnvOverride,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LocalUsageConfigSource {
    Missing,
    Text(String),
}

#[derive(Debug, Default)]
pub(crate) struct LocalUsageConfigResolver {
    cached: Option<(LocalUsageConfigSource, LocalUsageConfigState)>,
}

impl LocalUsageConfigResolver {
    pub(crate) fn resolve(&mut self, data_root: &Path) -> LocalUsageResolution {
        let path = AppConfig::config_path(data_root);
        let source = match fs::read_to_string(&path) {
            Ok(text) => Some(LocalUsageConfigSource::Text(text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Some(LocalUsageConfigSource::Missing)
            }
            Err(_) => None,
        };
        let config_state = match source {
            Some(source) => {
                if let Some((cached_source, cached_state)) = &self.cached {
                    if *cached_source == source {
                        *cached_state
                    } else {
                        self.cache_source(source)
                    }
                } else {
                    self.cache_source(source)
                }
            }
            None => LocalUsageConfigState::Unresolved,
        };
        LocalUsageResolution {
            config_state,
            environment_override: local_usage_env_override(),
        }
    }

    fn cache_source(&mut self, source: LocalUsageConfigSource) -> LocalUsageConfigState {
        let state = match &source {
            LocalUsageConfigSource::Missing => {
                LocalUsageConfigState::Resolved(LOCAL_USAGE_DEFAULT_ENABLED)
            }
            LocalUsageConfigSource::Text(text) => resolve_local_usage_config_text(text),
        };
        self.cached = Some((source, state));
        state
    }
}

impl LocalUsageResolution {
    pub(crate) fn effective_on_startup(self) -> bool {
        self.effective_after(None)
    }

    pub(crate) fn effective_after(self, previous: Option<bool>) -> bool {
        if matches!(
            self.environment_override,
            LocalUsageEnvOverride::Disabled | LocalUsageEnvOverride::Invalid
        ) {
            return false;
        }
        match self.config_state {
            LocalUsageConfigState::Resolved(persisted_enabled) => {
                effective_local_usage_enabled(persisted_enabled, self.environment_override)
            }
            LocalUsageConfigState::Malformed => false,
            LocalUsageConfigState::Unresolved => previous.unwrap_or(false),
        }
    }

    fn control(self) -> Option<LocalUsageControl> {
        let LocalUsageConfigState::Resolved(persisted_enabled) = self.config_state else {
            return None;
        };
        Some(LocalUsageControl {
            persisted_enabled,
            effective_enabled: effective_local_usage_enabled(
                persisted_enabled,
                self.environment_override,
            ),
            environment_override: self.environment_override,
        })
    }
}

#[derive(Debug, Clone)]
pub struct UpgradeConfig {
    pub auto: String,
    pub channel: String,
    pub interval: Duration,
}

#[derive(Debug, Clone)]
pub struct IndexingConfig {
    pub mode: IndexingMode,
}

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub mode: DaemonMode,
}

#[derive(Debug, Clone)]
pub struct SearchConfig {
    pub semantic: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct SourcesConfig {
    pub automatic: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            analytics: AnalyticsConfig {
                // Product analytics are default-on and can be disabled by config or env.
                enabled: true,
                endpoint: "https://cli.ctx.rs/functions/v1/analytics".to_owned(),
            },
            local_usage: LocalUsageConfig {
                // Content-free local product-state aggregates are independent of analytics.
                enabled: LOCAL_USAGE_DEFAULT_ENABLED,
            },
            upgrade: UpgradeConfig {
                // Managed installs check and apply in the background unless explicitly disabled.
                auto: AUTO_UPGRADE_DEFAULT_MODE.to_owned(),
                channel: "stable".to_owned(),
                interval: Duration::from_secs(24 * 60 * 60),
            },
            indexing: IndexingConfig {
                mode: IndexingMode::Automatic,
            },
            daemon: DaemonConfig {
                mode: DaemonMode::Full,
            },
            search: SearchConfig { semantic: None },
            sources: SourcesConfig { automatic: true },
            provider_roots: BTreeMap::new(),
        }
    }
}

impl AppConfig {
    pub const fn automatic_indexing_enabled(&self) -> bool {
        self.indexing.mode.is_automatic()
    }

    pub const fn persistent_automatic_upgrade_driver_enabled(&self) -> bool {
        self.indexing.mode.is_automatic() && matches!(self.daemon.mode, DaemonMode::Full)
    }

    pub fn auto_upgrade_mode(&self) -> AutoUpgradeMode {
        if self.upgrade.auto.eq_ignore_ascii_case("apply") {
            AutoUpgradeMode::Apply
        } else {
            AutoUpgradeMode::Off
        }
    }

    pub fn auto_upgrade_enabled(&self) -> bool {
        self.auto_upgrade_mode().enabled()
    }

    pub fn semantic_search_enabled(&self) -> bool {
        self.search
            .semantic
            .unwrap_or(SEMANTIC_SEARCH_DEFAULT_ENABLED)
    }

    pub fn semantic_search_source(&self) -> &'static str {
        if self.search.semantic.is_some() {
            "config"
        } else {
            "default"
        }
    }

    pub const fn automatic_source_discovery_enabled(&self) -> bool {
        self.sources.automatic
    }

    pub fn load(data_root: &Path) -> Result<Self> {
        let deprecated_controls = DeprecatedControls::detect();
        Self::load_with_deprecated_controls(data_root, &deprecated_controls)
    }

    pub(crate) fn load_with_deprecated_controls(
        data_root: &Path,
        deprecated_controls: &DeprecatedControls,
    ) -> Result<Self> {
        let mut config = Self::load_persisted(data_root)?;
        config.apply_env(deprecated_controls)?;
        if ctx_upgrade_engine::current_exe_is_staging_dogfood() {
            config.upgrade.auto = AutoUpgradeMode::Off.as_str().to_owned();
        }
        Ok(config)
    }

    fn load_persisted(data_root: &Path) -> Result<Self> {
        observe_app_config_load();
        let mut config = Self::default();
        let path = data_root.join(CONFIG_FILE);
        match fs::read_to_string(&path) {
            Ok(text) => {
                let parsed = parse_toml_subset(&text)
                    .with_context(|| format!("parse {}", path.display()))?;
                config
                    .apply_values(&parsed)
                    .with_context(|| format!("load {}", path.display()))?;
                config
                    .validate_provider_root_data_root(data_root)
                    .with_context(|| format!("load {}", path.display()))?;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
        }
        Ok(config)
    }

    fn apply_values(&mut self, values: &BTreeMap<String, ConfigValue>) -> Result<()> {
        let mut legacy_daemon_enabled = None;
        let mut indexing_mode = None;
        let mut provider_roots = BTreeMap::<
            String,
            (
                Option<CaptureProvider>,
                Option<PathBuf>,
                Option<String>,
                Option<ProviderRootKind>,
            ),
        >::new();
        for (key, value) in values {
            if let Some(dynamic) = key.strip_prefix("sources.roots.") {
                let Some((id, field)) = dynamic.rsplit_once('.') else {
                    bail!(
                        "provider root config key `{key}` at line {} must name a field",
                        value.line
                    );
                };
                validate_root_selector("provider root name", id)?;
                let draft = provider_roots.entry(id.to_owned()).or_default();
                match field {
                    "provider" => {
                        let provider_name = parse_non_empty_string(key, value)?;
                        let provider =
                            parse_capture_provider_name(&provider_name).with_context(|| {
                                format!(
                                    "sources.roots.{id}.provider at line {} is unknown",
                                    value.line
                                )
                            })?;
                        validate_provider_root_support(provider)?;
                        draft.0 = Some(provider);
                    }
                    "path" => {
                        let path = PathBuf::from(parse_non_empty_string(key, value)?);
                        validate_provider_root_path(&path)?;
                        draft.1 = Some(path);
                    }
                    "group" => {
                        let group = parse_non_empty_string(key, value)?;
                        validate_root_selector("source group", &group)?;
                        draft.2 = Some(group);
                    }
                    "kind" => {
                        let kind = parse_non_empty_string(key, value)?.parse().map_err(|_| {
                            anyhow::anyhow!(
                                "sources.roots.{id}.kind at line {} must be current-conversations or legacy-persistence",
                                value.line
                            )
                        })?;
                        draft.3 = Some(kind);
                    }
                    _ => bail!("unknown config key `{key}` at line {}", value.line),
                }
                continue;
            }
            match key.as_str() {
                "analytics.enabled" => {
                    self.analytics.enabled = parse_config_bool(key, value)?;
                }
                "analytics.endpoint" => {
                    self.analytics.endpoint = parse_non_empty_string(key, value)?;
                }
                "local_usage.enabled" => {
                    self.local_usage.enabled = parse_config_bool(key, value)?;
                }
                "upgrade.auto" => {
                    self.upgrade.auto = parse_upgrade_auto(value)?;
                }
                "upgrade.channel" => {
                    self.upgrade.channel = parse_non_empty_string(key, value)?;
                }
                "upgrade.interval_hours" => {
                    let hours = parse_config_u64(key, value)?;
                    self.upgrade.interval = Duration::from_secs(hours.saturating_mul(60 * 60));
                }
                "daemon.enabled" => {
                    legacy_daemon_enabled = Some(parse_config_bool(key, value)?);
                }
                "daemon.mode" => {
                    self.daemon.mode = parse_daemon_mode(value)?;
                }
                "indexing.mode" => {
                    indexing_mode = Some(parse_indexing_mode(value)?);
                }
                "search.semantic" => {
                    self.search.semantic = Some(parse_config_bool(key, value)?);
                }
                "sources.automatic" => {
                    self.sources.automatic = parse_config_bool(key, value)?;
                }
                "cloud.mode" => return Err(RemovedCloudModeConfigError.into()),
                _ => bail!("unknown config key `{key}` at line {}", value.line),
            }
        }
        self.indexing.mode = indexing_mode.unwrap_or_else(|| {
            legacy_daemon_enabled
                .map(IndexingMode::from_legacy_daemon_enabled)
                .unwrap_or(self.indexing.mode)
        });
        if provider_roots.len() > MAX_CONFIGURED_PROVIDER_ROOTS {
            bail!(
                "configured provider roots exceed the maximum of {MAX_CONFIGURED_PROVIDER_ROOTS}"
            );
        }
        self.provider_roots = provider_roots
            .into_iter()
            .map(|(id, (provider, path, group, kind))| {
                let provider = provider
                    .ok_or_else(|| anyhow::anyhow!("sources.roots.{id}.provider is required"))?;
                let path =
                    path.ok_or_else(|| anyhow::anyhow!("sources.roots.{id}.path is required"))?;
                validate_provider_root_kind(provider, kind)?;
                Ok((
                    id.clone(),
                    ProviderRootDefinition {
                        id,
                        provider,
                        path,
                        group,
                        kind,
                    },
                ))
            })
            .collect::<Result<_>>()?;
        for root in self.provider_roots.values_mut() {
            if let Ok(physical_path) = fs::canonicalize(&root.path) {
                validate_provider_root_path(&physical_path)?;
                root.path = physical_path;
            }
        }
        let roots = self.provider_roots.values().collect::<Vec<_>>();
        if let Some((left, right)) = roots.iter().enumerate().find_map(|(index, left)| {
            roots[index + 1..]
                .iter()
                .find(|right| {
                    left.provider == right.provider
                        && provider_paths_equivalent(&left.path, &right.path)
                })
                .map(|right| (*left, *right))
        }) {
            // Use the same physical-file identity authority as the safe editor
            // and discovery. Canonical spellings alone do not collapse hard
            // links to one provider database.
            bail!(
                "provider roots `{}` and `{}` select the same {} history root {}",
                left.id,
                right.id,
                right.provider.as_str(),
                right.path.display()
            );
        }
        if let Some((left, right)) = roots.iter().enumerate().find_map(|(index, left)| {
            roots[index + 1..]
                .iter()
                .find(|right| left.openhands_selected_histories_overlap(right))
                .map(|right| (*left, *right))
        }) {
            bail!(
                "openhands provider roots `{}` and `{}` select overlapping legacy and current history",
                left.id,
                right.id
            );
        }
        Ok(())
    }

    fn apply_env(&mut self, deprecated_controls: &DeprecatedControls) -> Result<()> {
        let analytics_config_disabled = !self.analytics.enabled;
        let analytics_enabled_override = env::var("CTX_ANALYTICS_ENABLED")
            .ok()
            .and_then(|value| parse_bool_value(&value));
        let analytics_disabled = analytics_config_disabled
            || analytics_enabled_override == Some(false)
            || deprecated_controls.disables_analytics();
        if analytics_disabled {
            self.analytics.enabled = false;
        } else if analytics_enabled_override == Some(true) {
            self.analytics.enabled = true;
        }
        if let Ok(endpoint) = env::var("CTX_ANALYTICS_ENDPOINT") {
            if !endpoint.trim().is_empty() {
                self.analytics.endpoint = endpoint;
            }
        }
        self.local_usage.enabled =
            effective_local_usage_enabled(self.local_usage.enabled, local_usage_env_override());
        let upgrade_config_disabled = self.auto_upgrade_mode() == AutoUpgradeMode::Off;
        let upgrade_env_mode = match env::var("CTX_UPGRADE_AUTO") {
            Ok(auto) if !auto.trim().is_empty() => {
                Some(parse_upgrade_auto_text("CTX_UPGRADE_AUTO", auto.trim())?)
            }
            _ => None,
        };
        if upgrade_config_disabled
            || upgrade_env_mode == Some(AutoUpgradeMode::Off)
            || deprecated_controls.disables_auto_upgrade()
        {
            self.upgrade.auto = AutoUpgradeMode::Off.as_str().to_owned();
        } else if upgrade_env_mode == Some(AutoUpgradeMode::Apply) {
            self.upgrade.auto = AutoUpgradeMode::Apply.as_str().to_owned();
        }
        if let Ok(channel) = env::var("CTX_UPGRADE_CHANNEL") {
            if !channel.trim().is_empty() {
                self.upgrade.channel = channel;
            }
        }
        if let Ok(seconds) = env::var("CTX_UPGRADE_INTERVAL_SECONDS") {
            if let Ok(seconds) = seconds.parse::<u64>() {
                self.upgrade.interval = Duration::from_secs(seconds);
            }
        }
        let indexing_config_manual = !self.indexing.mode.is_automatic();
        let daemon_enabled_override = env::var("CTX_DAEMON_ENABLED")
            .ok()
            .and_then(|value| parse_bool_value(&value));
        let daemon_disabled = indexing_config_manual
            || daemon_enabled_override == Some(false)
            || deprecated_controls.disables_daemon();
        if daemon_disabled {
            self.indexing.mode = IndexingMode::Manual;
        } else if daemon_enabled_override == Some(true) {
            self.indexing.mode = IndexingMode::Automatic;
        }
        if let Ok(mode) = env::var(DAEMON_MODE_ENV) {
            if !mode.trim().is_empty() {
                self.daemon.mode = parse_daemon_mode_text(DAEMON_MODE_ENV, mode.trim())?;
            }
        }
        if let Ok(value) = env::var("CTX_SEARCH_SEMANTIC") {
            if let Some(enabled) = parse_bool_value(&value) {
                self.search.semantic = Some(enabled);
            }
        }
        Ok(())
    }

    pub fn config_path(data_root: &Path) -> PathBuf {
        data_root.join(CONFIG_FILE)
    }

    pub fn provider_root_definitions(&self) -> Vec<ProviderRootDefinition> {
        self.provider_roots.values().cloned().collect()
    }

    fn validate_provider_root_data_root(&self, data_root: &Path) -> Result<()> {
        for root in self.provider_roots.values() {
            validate_provider_source_outside_data_root(data_root, &root.path).with_context(
                || {
                    format!(
                        "configured provider root `{}` must not overlap the ctx data root",
                        root.id
                    )
                },
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    static APP_CONFIG_LOAD_COUNT: std::cell::Cell<Option<usize>> = const {
        std::cell::Cell::new(None)
    };
}

fn observe_app_config_load() {
    #[cfg(test)]
    APP_CONFIG_LOAD_COUNT.with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current.saturating_add(1)));
        }
    });
}

#[cfg(test)]
pub(crate) fn count_app_config_loads<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    APP_CONFIG_LOAD_COUNT.with(|count| {
        let previous = count.replace(Some(0));
        assert!(
            previous.is_none(),
            "AppConfig load counters must not be nested"
        );
        let result = operation();
        let observed = count.replace(previous).unwrap_or(0);
        (result, observed)
    })
}

pub fn set_local_usage_enabled(data_root: &Path, enabled: bool) -> Result<()> {
    mutation::set_config_bool(data_root, "local_usage", "enabled", enabled)
}

pub(crate) fn read_local_usage_control(data_root: &Path) -> Result<LocalUsageControl> {
    let resolution = resolve_local_usage_control(data_root);
    let Some(control) = resolution.control() else {
        bail!("local usage configuration could not be resolved");
    };
    Ok(control)
}

pub(crate) fn resolve_local_usage_control(data_root: &Path) -> LocalUsageResolution {
    LocalUsageConfigResolver::default().resolve(data_root)
}

fn effective_local_usage_enabled(
    persisted_enabled: bool,
    environment_override: LocalUsageEnvOverride,
) -> bool {
    persisted_enabled
        && matches!(
            environment_override,
            LocalUsageEnvOverride::Unset | LocalUsageEnvOverride::Enabled
        )
}

fn local_usage_env_override() -> LocalUsageEnvOverride {
    match env::var_os("CTX_LOCAL_USAGE_ENABLED") {
        None => LocalUsageEnvOverride::Unset,
        Some(value) => match value.to_str() {
            Some("true") => LocalUsageEnvOverride::Enabled,
            Some("false") => LocalUsageEnvOverride::Disabled,
            Some(_) | None => LocalUsageEnvOverride::Invalid,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusedLocalUsageValue {
    Absent,
    Explicit(bool),
    Malformed,
}

fn resolve_local_usage_config_text(text: &str) -> LocalUsageConfigState {
    let focused = scan_local_usage_value(text);
    if focused == FocusedLocalUsageValue::Malformed {
        return LocalUsageConfigState::Malformed;
    }
    if focused == FocusedLocalUsageValue::Explicit(false) {
        // An explicit local opt-out remains authoritative even if an unrelated
        // part of the full config is malformed.
        return LocalUsageConfigState::Resolved(false);
    }
    let Ok(values) = parse_toml_subset(text) else {
        return LocalUsageConfigState::Unresolved;
    };
    let mut config = AppConfig::default();
    if config.apply_values(&values).is_err() {
        return LocalUsageConfigState::Unresolved;
    }
    LocalUsageConfigState::Resolved(match focused {
        FocusedLocalUsageValue::Explicit(enabled) => enabled,
        FocusedLocalUsageValue::Absent => LOCAL_USAGE_DEFAULT_ENABLED,
        FocusedLocalUsageValue::Malformed => unreachable!(),
    })
}

fn scan_local_usage_value(text: &str) -> FocusedLocalUsageValue {
    let mut section = "";
    let mut found = None;
    let mut local_usage_table_declared = false;
    for raw_line in text.lines() {
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            if !line.ends_with(']') {
                if starts_with_local_usage_key(line.trim_start_matches('[')) {
                    return FocusedLocalUsageValue::Malformed;
                }
                section = "";
                continue;
            }
            let candidate = line[1..line.len() - 1].trim();
            if candidate == "local_usage" {
                if local_usage_table_declared || found.is_some() {
                    return FocusedLocalUsageValue::Malformed;
                }
                local_usage_table_declared = true;
                section = candidate;
            } else if starts_with_local_usage_key(candidate.trim_start_matches('[')) {
                return FocusedLocalUsageValue::Malformed;
            } else {
                section = candidate;
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            if section == "local_usage" || (section.is_empty() && starts_with_local_usage_key(line))
            {
                return FocusedLocalUsageValue::Malformed;
            }
            continue;
        };
        let key = key.trim();
        let is_local_usage = (section == "local_usage" && key == "enabled")
            || (section.is_empty() && key == "local_usage.enabled");
        if (section == "local_usage" && key != "enabled")
            || (section.is_empty() && starts_with_local_usage_key(key) && !is_local_usage)
        {
            return FocusedLocalUsageValue::Malformed;
        }
        if !is_local_usage {
            continue;
        }
        if found.is_some() || (section.is_empty() && local_usage_table_declared) {
            return FocusedLocalUsageValue::Malformed;
        }
        found = Some(match value.trim() {
            "true" => true,
            "false" => false,
            _ => return FocusedLocalUsageValue::Malformed,
        });
    }
    found.map_or(
        FocusedLocalUsageValue::Absent,
        FocusedLocalUsageValue::Explicit,
    )
}

fn starts_with_local_usage_key(raw: &str) -> bool {
    const LOCAL_USAGE_KEY: &str = "local_usage";

    let candidate = raw.trim_start();
    if let Some(rest) = candidate.strip_prefix(LOCAL_USAGE_KEY) {
        return rest
            .chars()
            .next()
            .is_none_or(|character| character.is_whitespace() || matches!(character, '.' | ']'));
    }
    if candidate.starts_with('"') {
        return basic_quoted_key_starts_local_usage(candidate);
    }
    candidate.strip_prefix('\'').is_some_and(|quoted| {
        let key = quoted.split_once('\'').map_or(quoted, |(key, _)| key);
        key == LOCAL_USAGE_KEY || key.starts_with("local_usage.")
    })
}

fn basic_quoted_key_starts_local_usage(candidate: &str) -> bool {
    const LOCAL_USAGE_KEY: &str = "local_usage";
    const MAX_BASIC_KEY_SCAN_BYTES: usize = 256;

    let mut decoded = String::with_capacity(LOCAL_USAGE_KEY.len() + 1);
    let mut chars = candidate[1..].chars();
    let mut scanned_bytes = 1;
    while let Some(character) = chars.next() {
        scanned_bytes += character.len_utf8();
        if scanned_bytes > MAX_BASIC_KEY_SCAN_BYTES {
            return decoded == LOCAL_USAGE_KEY || decoded.starts_with("local_usage.");
        }
        let decoded_character = match character {
            '"' => return decoded == LOCAL_USAGE_KEY || decoded.starts_with("local_usage."),
            '\\' => {
                let Some(escape) = chars.next() else {
                    return decoded == LOCAL_USAGE_KEY || decoded.starts_with("local_usage.");
                };
                scanned_bytes += escape.len_utf8();
                match escape {
                    'b' => '\u{0008}',
                    't' => '\t',
                    'n' => '\n',
                    'f' => '\u{000c}',
                    'r' => '\r',
                    '"' => '"',
                    '\\' => '\\',
                    'u' => {
                        let Some(character) =
                            decode_basic_key_unicode_escape(&mut chars, 4, &mut scanned_bytes)
                        else {
                            return decoded == LOCAL_USAGE_KEY
                                || decoded.starts_with("local_usage.");
                        };
                        character
                    }
                    'U' => {
                        let Some(character) =
                            decode_basic_key_unicode_escape(&mut chars, 8, &mut scanned_bytes)
                        else {
                            return decoded == LOCAL_USAGE_KEY
                                || decoded.starts_with("local_usage.");
                        };
                        character
                    }
                    _ => {
                        return decoded == LOCAL_USAGE_KEY || decoded.starts_with("local_usage.");
                    }
                }
            }
            character if character.is_control() => {
                return decoded == LOCAL_USAGE_KEY || decoded.starts_with("local_usage.");
            }
            character => character,
        };
        decoded.push(decoded_character);
        if decoded.starts_with("local_usage.") {
            return true;
        }
        if decoded != LOCAL_USAGE_KEY && !LOCAL_USAGE_KEY.starts_with(&decoded) {
            return false;
        }
    }
    decoded == LOCAL_USAGE_KEY || decoded.starts_with("local_usage.")
}

fn decode_basic_key_unicode_escape(
    chars: &mut impl Iterator<Item = char>,
    digits: usize,
    scanned_bytes: &mut usize,
) -> Option<char> {
    let mut value = 0_u32;
    for _ in 0..digits {
        let digit = chars.next()?;
        *scanned_bytes += digit.len_utf8();
        value = value.checked_mul(16)?.checked_add(digit.to_digit(16)?)?;
    }
    char::from_u32(value)
}

#[cfg(test)]
mod provider_root_mutation_tests;

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
