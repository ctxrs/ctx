use std::{env, fmt::Write as _, fs, io::Read, path::Path};

use anyhow::{Context, Result};
use ring::digest::{Context as DigestContext, SHA256};
use serde_json::{json, Value};

use super::{daemon_lock_path, pid_lock_payload, read_pid_lock_json};

pub(in crate::semantic) fn current_daemon_lock_identity(data_root: &Path) -> Result<Value> {
    let binary = env::current_exe().context("resolve ctx daemon executable identity")?;
    Ok(pid_lock_payload(json!({
        "binary": binary,
        "binary_sha256": executable_sha256(&binary)?,
        "data_root": data_root,
    })))
}

pub(in crate::semantic) fn daemon_lock_matches_executable(
    data_root: &Path,
    executable: &Path,
) -> Result<bool> {
    let Some(value) = read_pid_lock_json(&daemon_lock_path(data_root)) else {
        return Ok(false);
    };
    daemon_lock_binary_identity_matches(&value, executable)
}

pub(in crate::semantic) fn daemon_lock_binary_identity_matches(
    value: &Value,
    executable: &Path,
) -> Result<bool> {
    let Some(recorded_binary) = value.get("binary").and_then(Value::as_str).map(Path::new) else {
        return Ok(false);
    };
    if fs::canonicalize(recorded_binary).ok() != fs::canonicalize(executable).ok() {
        return Ok(false);
    }
    let Some(recorded_sha256) = value.get("binary_sha256").and_then(Value::as_str) else {
        return Ok(false);
    };
    Ok(recorded_sha256 == executable_sha256(executable)?)
}

pub(in crate::semantic) fn executable_sha256(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("open executable identity {}", path.display()))?;
    let mut hasher = DigestContext::new(&SHA256);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .with_context(|| format!("read executable identity {}", path.display()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let mut encoded = String::with_capacity(64);
    for byte in hasher.finish().as_ref() {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn binary_identity_detects_same_path_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("ctx");
        fs::write(&executable, b"old executable image").unwrap();
        let lock = json!({
            "binary": executable,
            "binary_sha256": executable_sha256(&executable).unwrap(),
        });

        assert!(daemon_lock_binary_identity_matches(&lock, &executable).unwrap());
        fs::write(&executable, b"new executable image").unwrap();
        assert!(!daemon_lock_binary_identity_matches(&lock, &executable).unwrap());
    }
}
