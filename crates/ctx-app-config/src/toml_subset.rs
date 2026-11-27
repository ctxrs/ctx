use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};

use super::{
    AutoUpgradeMode, DaemonMode, IndexingMode, INDEXING_INTERVAL_MINUTES_MAX,
    INDEXING_INTERVAL_MINUTES_MIN, INDEXING_MEMORY_MB_MAX, INDEXING_MEMORY_MB_MIN,
    INDEXING_WORKERS_MAX, INDEXING_WORKERS_MIN,
};

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
        "scheduled" => Ok(IndexingMode::Scheduled),
        _ => bail!(
            "indexing.mode at line {} must be \"auto\", \"manual\", or \"scheduled\"",
            value.line
        ),
    }
}

pub(super) fn parse_indexing_interval_minutes(value: &ConfigValue) -> Result<u32> {
    let parsed = parse_config_u64("indexing.interval_minutes", value)?;
    validate_indexing_interval_minutes("indexing.interval_minutes", parsed)
        .with_context(|| format!("indexing.interval_minutes at line {}", value.line))
}

pub(super) fn validate_indexing_interval_minutes(key: &str, minutes: u64) -> Result<u32> {
    if (INDEXING_INTERVAL_MINUTES_MIN..=INDEXING_INTERVAL_MINUTES_MAX).contains(&minutes) {
        Ok(u32::try_from(minutes).expect("interval bound fits in u32"))
    } else {
        bail!(
            "{key} must be between {INDEXING_INTERVAL_MINUTES_MIN} and {INDEXING_INTERVAL_MINUTES_MAX} minutes"
        )
    }
}

pub(super) fn parse_indexing_memory_mb(value: &ConfigValue) -> Result<usize> {
    let parsed = parse_config_u64("indexing.memory_mb", value)?;
    validate_indexing_memory_mb("indexing.memory_mb", parsed)
        .with_context(|| format!("indexing.memory_mb at line {}", value.line))
}

pub(super) fn parse_indexing_memory_mb_text(key: &str, value: &str) -> Result<usize> {
    let parsed = value
        .parse::<u64>()
        .with_context(|| format!("{key} must be an integer between {INDEXING_MEMORY_MB_MIN} and {INDEXING_MEMORY_MB_MAX} MiB"))?;
    validate_indexing_memory_mb(key, parsed)
}

fn validate_indexing_memory_mb(key: &str, parsed: u64) -> Result<usize> {
    let mb = usize::try_from(parsed).ok().filter(|mb| {
        (INDEXING_MEMORY_MB_MIN..=INDEXING_MEMORY_MB_MAX).contains(mb)
            && mb.checked_mul(1024 * 1024).is_some()
    });
    mb.ok_or_else(|| {
        anyhow::anyhow!(
            "{key} must be between {INDEXING_MEMORY_MB_MIN} and {INDEXING_MEMORY_MB_MAX} MiB"
        )
    })
}

pub(super) fn parse_indexing_workers(value: &ConfigValue) -> Result<usize> {
    let parsed = parse_config_u64("indexing.workers", value)?;
    validate_indexing_workers("indexing.workers", parsed)
        .with_context(|| format!("indexing.workers at line {}", value.line))
}

pub(super) fn parse_indexing_workers_text(key: &str, value: &str) -> Result<usize> {
    let parsed = value.parse::<u64>().with_context(|| {
        format!(
            "{key} must be an integer between {INDEXING_WORKERS_MIN} and {INDEXING_WORKERS_MAX}"
        )
    })?;
    validate_indexing_workers(key, parsed)
}

fn validate_indexing_workers(key: &str, parsed: u64) -> Result<usize> {
    let workers = usize::try_from(parsed)
        .ok()
        .filter(|workers| (INDEXING_WORKERS_MIN..=INDEXING_WORKERS_MAX).contains(workers));
    workers.ok_or_else(|| {
        anyhow::anyhow!("{key} must be between {INDEXING_WORKERS_MIN} and {INDEXING_WORKERS_MAX}")
    })
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
