use std::{fs, path::Path};

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::process_executable_sha256;

pub(super) fn verify_lock_paths(
    value: &Value,
    data_root: &Path,
    expected_executable: &Path,
) -> Result<()> {
    let recorded_root = value
        .get("data_root")
        .and_then(Value::as_str)
        .map(Path::new)
        .ok_or_else(|| anyhow!("ctx daemon lock has no data-root identity"))?;
    if !same_windows_path(recorded_root, data_root) {
        return Err(anyhow!(
            "ctx daemon lock data-root identity does not match uninstall target"
        ));
    }
    let recorded_binary = value
        .get("binary")
        .and_then(Value::as_str)
        .map(Path::new)
        .ok_or_else(|| anyhow!("ctx daemon lock has no executable identity"))?;
    if !same_windows_path(recorded_binary, expected_executable) {
        return Err(anyhow!(
            "ctx daemon lock executable is not the installed ctx executable"
        ));
    }
    Ok(())
}

pub(super) fn verify_recorded_digest_identity(pid: u32, value: &Value) -> Result<()> {
    let recorded_sha256 = value
        .get("binary_sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("ctx daemon lock has no executable digest identity"))?;
    let process_sha256 = process_executable_sha256(pid).ok_or_else(|| {
        anyhow!(
            "cannot verify executable image for residual ctx process {pid}; refusing to terminate"
        )
    })?;
    if process_sha256 != recorded_sha256 {
        return Err(anyhow!(
            "residual lock owner image does not match its held ctx daemon lock; refusing to terminate"
        ));
    }
    Ok(())
}

pub(super) fn same_windows_path(left: &Path, right: &Path) -> bool {
    let normalize = |path: &Path| {
        fs::canonicalize(path)
            .ok()
            .map(|path| path.to_string_lossy().to_lowercase())
    };
    matches!((normalize(left), normalize(right)), (Some(left), Some(right)) if left == right)
}
