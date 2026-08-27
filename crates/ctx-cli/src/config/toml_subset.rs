use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};

use super::{AutoUpgradeMode, DaemonMode, IndexingMode, SemanticIndexingIntensity};

#[derive(Debug, Clone)]
pub(super) struct ConfigValue {
    raw: String,
    pub(super) line: usize,
}

pub(super) fn parse_toml_subset(text: &str) -> Result<BTreeMap<String, ConfigValue>> {
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

pub(super) fn strip_comment(line: &str) -> &str {
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

pub(super) fn parse_non_empty_string(key: &str, value: &ConfigValue) -> Result<String> {
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

pub(super) fn parse_config_bool(key: &str, value: &ConfigValue) -> Result<bool> {
    match value.raw.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => bail!("{key} at line {} must be a boolean", value.line),
    }
}

pub(super) fn parse_config_u64(key: &str, value: &ConfigValue) -> Result<u64> {
    value
        .raw
        .trim()
        .parse::<u64>()
        .with_context(|| format!("{key} at line {} must be an unsigned integer", value.line))
}

pub(super) fn parse_upgrade_auto(value: &ConfigValue) -> Result<String> {
    let auto = parse_non_empty_string("upgrade.auto", value)?;
    parse_upgrade_auto_text("upgrade.auto", &auto)
        .map(|mode| mode.as_str().to_owned())
        .with_context(|| format!("upgrade.auto at line {}", value.line))
}

pub(super) fn parse_upgrade_auto_text(key: &str, value: &str) -> Result<AutoUpgradeMode> {
    match value.to_ascii_lowercase().as_str() {
        "apply" => Ok(AutoUpgradeMode::Apply),
        "off" => Ok(AutoUpgradeMode::Off),
        _ => bail!("{key} must be either \"apply\" or \"off\""),
    }
}

pub(super) fn parse_indexing_mode(value: &ConfigValue) -> Result<IndexingMode> {
    let mode = parse_non_empty_string("indexing.mode", value)?;
    match mode.to_ascii_lowercase().as_str() {
        "auto" | "automatic" => Ok(IndexingMode::Automatic),
        "manual" => Ok(IndexingMode::Manual),
        _ => bail!(
            "indexing.mode at line {} must be either \"auto\" or \"manual\"",
            value.line
        ),
    }
}

pub(super) fn parse_semantic_indexing_intensity(
    value: &ConfigValue,
) -> Result<SemanticIndexingIntensity> {
    let intensity = parse_non_empty_string("semantic.indexing_intensity", value)?;
    match intensity.as_str() {
        "quiet" => Ok(SemanticIndexingIntensity::Quiet),
        "full" => Ok(SemanticIndexingIntensity::Full),
        _ => bail!(
            "semantic.indexing_intensity at line {} must be either \"quiet\" or \"full\"",
            value.line
        ),
    }
}

pub(super) fn parse_daemon_mode(value: &ConfigValue) -> Result<DaemonMode> {
    let mode = parse_non_empty_string("daemon.mode", value)?;
    parse_daemon_mode_text("daemon.mode", &mode)
        .with_context(|| format!("daemon.mode at line {}", value.line))
}

pub(super) fn parse_daemon_mode_text(key: &str, value: &str) -> Result<DaemonMode> {
    DaemonMode::parse(value)
        .ok_or_else(|| anyhow::anyhow!("{key} must be either \"full\" or \"source-refresh-only\""))
}

pub(super) fn parse_bool_value(value: &str) -> Option<bool> {
    match value.trim().trim_matches('"').to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}
