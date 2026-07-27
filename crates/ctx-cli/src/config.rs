use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context, Result};

use crate::deprecated_controls::DeprecatedControls;

pub const CONFIG_FILE: &str = "config.toml";
pub const AUTO_UPGRADE_DEFAULT_MODE: &str = "apply";
pub const DAEMON_DEFAULT_ENABLED: bool = true;
pub const SEMANTIC_SEARCH_DEFAULT_ENABLED: bool = false;

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

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub analytics: AnalyticsConfig,
    pub upgrade: UpgradeConfig,
    pub daemon: DaemonConfig,
    pub search: SearchConfig,
}

#[derive(Debug, Clone)]
pub struct AnalyticsConfig {
    pub enabled: bool,
    pub endpoint: String,
}

#[derive(Debug, Clone)]
pub struct UpgradeConfig {
    pub auto: String,
    pub channel: String,
    pub interval: Duration,
    pub functions_base: String,
}

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct SearchConfig {
    pub semantic: Option<bool>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            analytics: AnalyticsConfig {
                // Product analytics are default-on and can be disabled by config or env.
                enabled: true,
                endpoint: "https://cli.ctx.rs/functions/v1/analytics".to_owned(),
            },
            upgrade: UpgradeConfig {
                // Managed installs check and apply in the background unless explicitly disabled.
                auto: AUTO_UPGRADE_DEFAULT_MODE.to_owned(),
                channel: "stable".to_owned(),
                interval: Duration::from_secs(24 * 60 * 60),
                functions_base: "https://cli.ctx.rs/functions/v1".to_owned(),
            },
            daemon: DaemonConfig {
                enabled: DAEMON_DEFAULT_ENABLED,
            },
            search: SearchConfig { semantic: None },
        }
    }
}

impl AppConfig {
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

    pub fn load(data_root: &Path) -> Result<Self> {
        let deprecated_controls = DeprecatedControls::detect();
        Self::load_with_deprecated_controls(data_root, &deprecated_controls)
    }

    pub(crate) fn load_with_deprecated_controls(
        data_root: &Path,
        deprecated_controls: &DeprecatedControls,
    ) -> Result<Self> {
        let mut config = Self::default();
        let path = data_root.join(CONFIG_FILE);
        match fs::read_to_string(&path) {
            Ok(text) => {
                let parsed = parse_toml_subset(&text)
                    .with_context(|| format!("parse {}", path.display()))?;
                config
                    .apply_values(&parsed)
                    .with_context(|| format!("load {}", path.display()))?;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
        }
        config.apply_env(deprecated_controls)?;
        Ok(config)
    }

    fn apply_values(&mut self, values: &BTreeMap<String, ConfigValue>) -> Result<()> {
        for (key, value) in values {
            match key.as_str() {
                "analytics.enabled" => {
                    self.analytics.enabled = parse_config_bool(key, value)?;
                }
                "analytics.endpoint" => {
                    self.analytics.endpoint = parse_non_empty_string(key, value)?;
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
                "upgrade.functions_base" => {
                    self.upgrade.functions_base = parse_non_empty_string(key, value)?;
                }
                "daemon.enabled" => {
                    self.daemon.enabled = parse_config_bool(key, value)?;
                }
                "search.semantic" => {
                    self.search.semantic = Some(parse_config_bool(key, value)?);
                }
                _ => bail!("unknown config key `{key}` at line {}", value.line),
            }
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
        if let Ok(functions_base) = env::var("CTX_UPGRADE_FUNCTIONS_BASE") {
            if !functions_base.trim().is_empty() {
                self.upgrade.functions_base = functions_base;
            }
        }
        if let Ok(seconds) = env::var("CTX_UPGRADE_INTERVAL_SECONDS") {
            if let Ok(seconds) = seconds.parse::<u64>() {
                self.upgrade.interval = Duration::from_secs(seconds);
            }
        }
        let daemon_config_disabled = !self.daemon.enabled;
        let daemon_enabled_override = env::var("CTX_DAEMON_ENABLED")
            .ok()
            .and_then(|value| parse_bool_value(&value));
        let daemon_disabled = daemon_config_disabled
            || daemon_enabled_override == Some(false)
            || deprecated_controls.disables_daemon();
        if daemon_disabled {
            self.daemon.enabled = false;
        } else if daemon_enabled_override == Some(true) {
            self.daemon.enabled = true;
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
}

pub fn write_default_config(data_root: &Path) -> Result<()> {
    fs::create_dir_all(data_root)?;
    Ok(())
}

pub fn set_daemon_enabled(data_root: &Path, enabled: bool) -> Result<()> {
    set_config_bool(data_root, "daemon", "enabled", enabled)
}

pub fn set_semantic_search_enabled(data_root: &Path, enabled: bool) -> Result<()> {
    set_config_bool(data_root, "search", "semantic", enabled)
}

fn set_config_bool(data_root: &Path, section: &str, key: &str, enabled: bool) -> Result<()> {
    fs::create_dir_all(data_root)?;
    let path = AppConfig::config_path(data_root);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let parsed = parse_toml_subset(&text).with_context(|| format!("parse {}", path.display()))?;
    let mut config = AppConfig::default();
    config
        .apply_values(&parsed)
        .with_context(|| format!("load {}", path.display()))?;
    let updated = set_toml_bool(&text, section, key, enabled);
    let parsed =
        parse_toml_subset(&updated).with_context(|| format!("parse updated {}", path.display()))?;
    let mut config = AppConfig::default();
    config
        .apply_values(&parsed)
        .with_context(|| format!("load updated {}", path.display()))?;
    if updated == text {
        return Ok(());
    }
    fs::write(&path, updated).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn set_toml_bool(text: &str, section: &str, key: &str, enabled: bool) -> String {
    let rendered = format!("{key} = {enabled}");
    let mut lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
    let mut current_section = String::new();
    let mut section_start = None;
    let mut insert_before = lines.len();
    for (index, raw_line) in lines.iter().enumerate() {
        let line = strip_comment(raw_line).trim();
        if line.starts_with('[') && line.ends_with(']') {
            if section_start.is_some() && current_section == section {
                insert_before = index;
                break;
            }
            current_section = line[1..line.len() - 1].trim().to_owned();
            if current_section == section {
                section_start = Some(index);
                insert_before = lines.len();
            }
            continue;
        }
        if current_section == section {
            if let Some((candidate, _)) = line.split_once('=') {
                if candidate.trim() == key {
                    lines[index] = rendered;
                    return ensure_trailing_newline(lines.join("\n"));
                }
            }
        }
    }
    match section_start {
        Some(start) => {
            let insert_at = insert_before.max(start + 1);
            lines.insert(insert_at, rendered);
        }
        None => {
            if !lines.last().is_none_or(|line| line.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(format!("[{section}]"));
            lines.push(rendered);
        }
    }
    ensure_trailing_newline(lines.join("\n"))
}

fn ensure_trailing_newline(mut text: String) -> String {
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

#[derive(Debug, Clone)]
struct ConfigValue {
    raw: String,
    line: usize,
}

fn parse_toml_subset(text: &str) -> Result<BTreeMap<String, ConfigValue>> {
    let mut section = String::new();
    let mut values = BTreeMap::new();
    for (index, raw_line) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            if !line.ends_with(']') {
                bail!("invalid config section header at line {line_number}: {line}");
            }
            section = line[1..line.len() - 1].trim().to_owned();
            if section.is_empty() {
                bail!("empty config section header at line {line_number}");
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!("invalid config line {line_number}: expected `[section]` or `key = value`");
        };
        let key = key.trim();
        if key.is_empty() {
            bail!("empty config key at line {line_number}");
        }
        let full_key = if section.is_empty() {
            key.to_owned()
        } else {
            format!("{section}.{key}")
        };
        let value = ConfigValue {
            raw: value.trim().to_owned(),
            line: line_number,
        };
        if let Some(previous) = values.insert(full_key.clone(), value) {
            bail!(
                "duplicate config key `{full_key}` at line {line_number}; first set at line {}",
                previous.line
            );
        }
    }
    Ok(values)
}

fn strip_comment(line: &str) -> &str {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if in_double_quote {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => in_double_quote = false,
                _ => {}
            }
            continue;
        }
        if in_single_quote {
            if ch == '\'' {
                in_single_quote = false;
            }
            continue;
        }
        match ch {
            '#' => return &line[..index],
            '"' => in_double_quote = true,
            '\'' => in_single_quote = true,
            _ => {}
        }
    }
    line
}

fn parse_non_empty_string(key: &str, value: &ConfigValue) -> Result<String> {
    let parsed = parse_config_string(key, value)?;
    if parsed.trim().is_empty() {
        bail!("{key} at line {} must not be empty", value.line);
    }
    Ok(parsed)
}

fn parse_config_string(key: &str, value: &ConfigValue) -> Result<String> {
    let raw = value.raw.trim();
    if raw.len() >= 2
        && ((raw.starts_with('"') && raw.ends_with('"'))
            || (raw.starts_with('\'') && raw.ends_with('\'')))
    {
        return Ok(raw[1..raw.len() - 1].to_owned());
    }
    bail!("{key} at line {} must be a quoted string", value.line);
}

fn parse_config_bool(key: &str, value: &ConfigValue) -> Result<bool> {
    match value.raw.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => bail!("{key} at line {} must be a boolean", value.line),
    }
}

fn parse_config_u64(key: &str, value: &ConfigValue) -> Result<u64> {
    value
        .raw
        .trim()
        .parse::<u64>()
        .with_context(|| format!("{key} at line {} must be an unsigned integer", value.line))
}

fn parse_upgrade_auto(value: &ConfigValue) -> Result<String> {
    let auto = parse_non_empty_string("upgrade.auto", value)?;
    parse_upgrade_auto_text("upgrade.auto", &auto)
        .map(|mode| mode.as_str().to_owned())
        .with_context(|| format!("upgrade.auto at line {}", value.line))
}

fn parse_upgrade_auto_text(key: &str, value: &str) -> Result<AutoUpgradeMode> {
    match value.to_ascii_lowercase().as_str() {
        "apply" => Ok(AutoUpgradeMode::Apply),
        "off" => Ok(AutoUpgradeMode::Off),
        _ => bail!("{key} must be either \"apply\" or \"off\""),
    }
}

fn parse_bool_value(value: &str) -> Option<bool> {
    match value.trim().trim_matches('"').to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
