use anyhow::{anyhow, Result};

use super::super::validate_sha256;

pub(super) fn validate_asset_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(anyhow!("invalid Semantic asset ID {id:?}"));
    }
    Ok(())
}

pub(super) fn validate_archive_prefix(prefix: &str) -> Result<()> {
    if prefix.is_empty() {
        return Ok(());
    }
    validate_relative_path(prefix)
}

pub(super) fn validate_relative_path(path: &str) -> Result<()> {
    if path.is_empty()
        || !path.is_ascii()
        || path
            .bytes()
            .any(|byte| !(0x20..=0x7e).contains(&byte) || byte == b':')
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains('\\')
        || path.contains("//")
        || path.split('/').any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || component.ends_with('.')
                || component.ends_with(' ')
                || windows_reserved_component(component)
        })
    {
        return Err(anyhow!(
            "unsafe or non-canonical Semantic file path {path:?}"
        ));
    }
    Ok(())
}

fn windows_reserved_component(component: &str) -> bool {
    let stem = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem)
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
}

pub(super) fn validate_lowercase_sha256(value: &str) -> Result<()> {
    validate_sha256(value)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(anyhow!("Semantic checksum must use lowercase SHA-256 hex"));
    }
    Ok(())
}
