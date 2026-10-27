//! Only the newest committed native generation belongs to a logical session.
use crate::Result;
use std::{fs, path::Path};
fn version(name: &str) -> Option<u64> {
    let name = name.strip_suffix(".zstd").unwrap_or(name);
    if name == "session.jsonl" {
        return Some(0);
    }
    let number = name.strip_prefix("session.v")?.strip_suffix(".jsonl")?;
    let value: u64 = number.parse().ok()?;
    (value > 0 && value.to_string() == number).then_some(value)
}
pub(crate) fn selected(path: &Path) -> Result<bool> {
    let Some(current) = path.file_name().and_then(|n| n.to_str()).and_then(version) else {
        return Ok(true);
    };
    let Some(parent) = path.parent() else {
        return Ok(true);
    };
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry
                .file_name()
                .to_str()
                .and_then(version)
                .is_some_and(|v| v > current)
        {
            return Ok(false);
        }
    }
    Ok(true)
}
