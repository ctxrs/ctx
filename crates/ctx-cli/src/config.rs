use std::{
    collections::BTreeMap,
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};

pub const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub analytics: AnalyticsConfig,
    pub updates: UpdatesConfig,
}

#[derive(Debug, Clone)]
pub struct AnalyticsConfig {
    pub enabled: bool,
    pub endpoint: String,
}

#[derive(Debug, Clone)]
pub struct UpdatesConfig {
    pub auto_update: bool,
    pub channel: String,
    pub endpoint_base: String,
    pub check_interval: Duration,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            analytics: AnalyticsConfig {
                enabled: true,
                endpoint: "https://api.ctx.rs/functions/v1/telemetry".to_owned(),
            },
            updates: UpdatesConfig {
                auto_update: true,
                channel: "stable".to_owned(),
                endpoint_base: "https://api.ctx.rs/functions/v1".to_owned(),
                check_interval: Duration::from_secs(24 * 60 * 60),
            },
        }
    }
}

impl AppConfig {
    pub fn load(data_root: &Path) -> Result<Self> {
        let mut config = Self::default();
        let path = data_root.join(CONFIG_FILE);
        if path.exists() {
            let text =
                fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            let parsed = parse_toml_subset(&text);
            config.apply_values(&parsed);
        }
        config.apply_env();
        Ok(config)
    }

    fn apply_values(&mut self, values: &BTreeMap<String, String>) {
        if let Some(enabled) = parse_bool(values.get("analytics.enabled")) {
            self.analytics.enabled = enabled;
        }
        if let Some(endpoint) = parse_string(values.get("analytics.endpoint")) {
            self.analytics.endpoint = endpoint;
        }
        if let Some(auto_update) = parse_bool(values.get("updates.auto_update")) {
            self.updates.auto_update = auto_update;
        }
        if let Some(channel) = parse_string(values.get("updates.channel")) {
            self.updates.channel = channel;
        }
        if let Some(endpoint_base) = parse_string(values.get("updates.endpoint_base")) {
            self.updates.endpoint_base = endpoint_base;
        }
        if let Some(seconds) = parse_u64(values.get("updates.check_interval_seconds")) {
            self.updates.check_interval = Duration::from_secs(seconds);
        }
    }

    fn apply_env(&mut self) {
        if env_flag("CTX_ANALYTICS_OFF") || env_flag("CTX_DISABLE_ANALYTICS") {
            self.analytics.enabled = false;
        }
        if let Ok(value) = env::var("CTX_ANALYTICS_ENABLED") {
            if let Some(enabled) = parse_bool_value(&value) {
                self.analytics.enabled = enabled;
            }
        }
        if let Ok(endpoint) = env::var("CTX_ANALYTICS_ENDPOINT") {
            if !endpoint.trim().is_empty() {
                self.analytics.endpoint = endpoint;
            }
        }
        if env_flag("CTX_DISABLE_AUTO_UPDATE") || env_flag("CTX_NO_AUTO_UPDATE") {
            self.updates.auto_update = false;
        }
        if let Ok(value) = env::var("CTX_AUTO_UPDATE") {
            if let Some(enabled) = parse_bool_value(&value) {
                self.updates.auto_update = enabled;
            }
        }
        if let Ok(channel) = env::var("CTX_UPDATE_CHANNEL") {
            if !channel.trim().is_empty() {
                self.updates.channel = channel;
            }
        }
        if let Ok(endpoint_base) = env::var("CTX_UPDATE_ENDPOINT_BASE") {
            if !endpoint_base.trim().is_empty() {
                self.updates.endpoint_base = endpoint_base;
            }
        }
        if let Ok(seconds) = env::var("CTX_UPDATE_CHECK_INTERVAL_SECONDS") {
            if let Ok(seconds) = seconds.parse::<u64>() {
                self.updates.check_interval = Duration::from_secs(seconds);
            }
        }
    }

    pub fn config_path(data_root: &Path) -> PathBuf {
        data_root.join(CONFIG_FILE)
    }
}

pub fn write_default_config(data_root: &Path) -> Result<()> {
    let path = AppConfig::config_path(data_root);
    if path.exists() {
        return Ok(());
    }
    let mut file = fs::File::create(&path)?;
    file.write_all(
        b"[updates]\n\
channel = \"stable\"\n\
auto_update = true\n",
    )?;
    Ok(())
}

fn parse_toml_subset(text: &str) -> BTreeMap<String, String> {
    let mut section = String::new();
    let mut values = BTreeMap::new();
    for raw_line in text.lines() {
        let line = raw_line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .to_owned();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let full_key = if section.is_empty() {
            key.to_owned()
        } else {
            format!("{section}.{key}")
        };
        values.insert(
            full_key,
            value.trim().trim_end_matches(',').trim().to_owned(),
        );
    }
    values
}

fn parse_string(value: Option<&String>) -> Option<String> {
    value
        .map(|value| value.trim().trim_matches('"').trim_matches('\'').to_owned())
        .filter(|value| !value.is_empty())
}

fn parse_bool(value: Option<&String>) -> Option<bool> {
    value.and_then(|value| parse_bool_value(value))
}

fn parse_bool_value(value: &str) -> Option<bool> {
    match value.trim().trim_matches('"').to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse_u64(value: Option<&String>) -> Option<u64> {
    value.and_then(|value| value.trim().trim_matches('"').parse::<u64>().ok())
}

fn env_flag(key: &str) -> bool {
    env::var_os(key).is_some_and(|value| {
        let value = value.to_string_lossy();
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_day_one_config_values() {
        let values = parse_toml_subset(
            r#"
[analytics]
enabled = false

[updates]
channel = "beta"
auto_update = false
check_interval_seconds = 60
"#,
        );
        let mut config = AppConfig::default();
        config.apply_values(&values);
        assert!(!config.analytics.enabled);
        assert!(!config.updates.auto_update);
        assert_eq!(config.updates.channel, "beta");
        assert_eq!(config.updates.check_interval, Duration::from_secs(60));
    }
}
