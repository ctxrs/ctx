use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context, Result};

pub const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub analytics: AnalyticsConfig,
    pub upgrade: UpgradeConfig,
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

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            analytics: AnalyticsConfig {
                enabled: true,
                endpoint: "https://cli.ctx.rs/functions/v1/analytics".to_owned(),
            },
            upgrade: UpgradeConfig {
                auto: "apply".to_owned(),
                channel: "stable".to_owned(),
                interval: Duration::from_secs(24 * 60 * 60),
                functions_base: "https://cli.ctx.rs/functions/v1".to_owned(),
            },
        }
    }
}

impl AppConfig {
    pub fn load(data_root: &Path) -> Result<Self> {
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
        config.apply_env();
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
                "upgrade.interval_seconds" => {
                    self.upgrade.interval = Duration::from_secs(parse_config_u64(key, value)?);
                }
                "upgrade.functions_base" => {
                    self.upgrade.functions_base = parse_non_empty_string(key, value)?;
                }
                _ => bail!("unknown config key `{key}` at line {}", value.line),
            }
        }
        Ok(())
    }

    fn apply_env(&mut self) {
        if let Ok(value) = env::var("CTX_ANALYTICS_ENABLED") {
            if let Some(enabled) = parse_bool_value(&value) {
                self.analytics.enabled = enabled;
            }
        }
        if env_flag("CTX_ANALYTICS_OFF") || env_flag("CTX_DISABLE_ANALYTICS") {
            self.analytics.enabled = false;
        }
        if let Ok(endpoint) = env::var("CTX_ANALYTICS_ENDPOINT") {
            if !endpoint.trim().is_empty() {
                self.analytics.endpoint = endpoint;
            }
        }
        if env_flag("CTX_UPGRADE_OFF") || env_flag("CTX_DISABLE_AUTO_UPGRADE") {
            self.upgrade.auto = "off".to_owned();
        }
        if let Ok(auto) = env::var("CTX_UPGRADE_AUTO") {
            if !auto.trim().is_empty() {
                self.upgrade.auto = auto;
            }
        }
        if let Ok(channel) = env::var("CTX_CHANNEL").or_else(|_| env::var("CTX_UPGRADE_CHANNEL")) {
            if !channel.trim().is_empty() {
                self.upgrade.channel = channel;
            }
        }
        if let Ok(functions_base) = env::var("CTX_FUNCTIONS_BASE") {
            if !functions_base.trim().is_empty() {
                self.upgrade.functions_base = functions_base;
            }
        }
        if let Ok(seconds) = env::var("CTX_UPGRADE_INTERVAL_SECONDS") {
            if let Ok(seconds) = seconds.parse::<u64>() {
                self.upgrade.interval = Duration::from_secs(seconds);
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
        b"[upgrade]\n\
auto = \"apply\"\n\
channel = \"stable\"\n\
interval_hours = 24\n",
    )?;
    Ok(())
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
    match auto.to_ascii_lowercase().as_str() {
        "apply" | "off" => Ok(auto.to_ascii_lowercase()),
        _ => bail!(
            "upgrade.auto at line {} must be either \"apply\" or \"off\"",
            value.line
        ),
    }
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

[upgrade]
auto = "off"
channel = "beta"
interval_seconds = 60
"#,
        )
        .unwrap();
        let mut config = AppConfig::default();
        assert_eq!(
            config.analytics.endpoint,
            "https://cli.ctx.rs/functions/v1/analytics"
        );
        assert!(config.analytics.enabled);
        assert_eq!(config.upgrade.auto, "apply");
        config.apply_values(&values).unwrap();
        assert!(!config.analytics.enabled);
        assert_eq!(config.upgrade.auto, "off");
        assert_eq!(config.upgrade.channel, "beta");
        assert_eq!(config.upgrade.interval, Duration::from_secs(60));
    }

    #[test]
    fn load_without_config_file_uses_defaults() {
        let temp = tempfile::tempdir().unwrap();

        let config = AppConfig::load(temp.path()).unwrap();

        assert!(config.analytics.enabled);
        assert_eq!(config.upgrade.auto, "apply");
        assert_eq!(config.upgrade.channel, "stable");
        assert_eq!(config.upgrade.interval, Duration::from_secs(24 * 60 * 60));
    }

    #[test]
    fn load_valid_config_file_applies_values() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join(CONFIG_FILE),
            r#"
[analytics]
enabled = false
endpoint = "file:///tmp/ctx-analytics.jsonl"

[upgrade]
auto = "off"
channel = "beta"
interval_hours = 2
functions_base = "https://example.test/functions/v1"
"#,
        )
        .unwrap();

        let config = AppConfig::load(temp.path()).unwrap();

        assert!(!config.analytics.enabled);
        assert_eq!(config.analytics.endpoint, "file:///tmp/ctx-analytics.jsonl");
        assert_eq!(config.upgrade.auto, "off");
        assert_eq!(config.upgrade.channel, "beta");
        assert_eq!(config.upgrade.interval, Duration::from_secs(2 * 60 * 60));
        assert_eq!(
            config.upgrade.functions_base,
            "https://example.test/functions/v1"
        );
    }

    #[test]
    fn rejects_invalid_config_booleans() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join(CONFIG_FILE),
            "[analytics]\nenabled = flase\n",
        )
        .unwrap();

        let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());

        assert!(error.contains("analytics.enabled"), "{error}");
        assert!(error.contains("boolean"), "{error}");
    }

    #[test]
    fn rejects_invalid_upgrade_auto_values() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join(CONFIG_FILE),
            "[upgrade]\nauto = \"offf\"\n",
        )
        .unwrap();

        let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());

        assert!(error.contains("upgrade.auto"), "{error}");
        assert!(error.contains("\"apply\" or \"off\""), "{error}");
    }

    #[test]
    fn rejects_unquoted_upgrade_auto_values() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(CONFIG_FILE), "[upgrade]\nauto = offf\n").unwrap();

        let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());

        assert!(error.contains("upgrade.auto"), "{error}");
        assert!(error.contains("quoted string"), "{error}");
    }

    #[test]
    fn rejects_invalid_config_numbers() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join(CONFIG_FILE),
            "[upgrade]\ninterval_seconds = nope\n",
        )
        .unwrap();

        let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());

        assert!(error.contains("upgrade.interval_seconds"), "{error}");
        assert!(error.contains("unsigned integer"), "{error}");
    }

    #[test]
    fn rejects_malformed_config_lines() {
        let error = parse_toml_subset("[upgrade]\nthis is not valid\n").unwrap_err();
        let error = error.to_string();

        assert!(error.contains("invalid config line 2"), "{error}");
    }

    #[test]
    fn rejects_unknown_config_keys() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join(CONFIG_FILE),
            "[analytics]\nenabld = false\n",
        )
        .unwrap();

        let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());

        assert!(error.contains("unknown config key"), "{error}");
        assert!(error.contains("analytics.enabld"), "{error}");
    }
}
