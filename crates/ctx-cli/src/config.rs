use std::{
    collections::BTreeMap,
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

pub const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub analytics: AnalyticsConfig,
}

#[derive(Debug, Clone)]
pub struct AnalyticsConfig {
    pub enabled: bool,
    pub endpoint: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            analytics: AnalyticsConfig {
                enabled: false,
                endpoint: "https://api.ctx.rs/functions/v1/analytics".to_owned(),
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
        b"[analytics]\n\
enabled = false\n",
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
"#,
        );
        let mut config = AppConfig::default();
        assert_eq!(
            config.analytics.endpoint,
            "https://api.ctx.rs/functions/v1/analytics"
        );
        config.apply_values(&values);
        assert!(!config.analytics.enabled);
    }
}
