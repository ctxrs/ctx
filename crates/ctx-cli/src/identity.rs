use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use ctx_history_core::utc_now;
use serde_json::json;
use uuid::Uuid;

const INSTALL_FILE: &str = "install.json";
const DEVICE_FILE: &str = "device.json";

pub fn install_id(data_root: &Path) -> Result<String> {
    fs::create_dir_all(data_root)?;
    let path = install_path(data_root);
    if path.exists() {
        let value: serde_json::Value = serde_json::from_slice(
            &fs::read(&path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("parse {}", path.display()))?;
        if let Some(id) = value.get("install_id").and_then(|value| value.as_str()) {
            if !id.trim().is_empty() {
                return Ok(id.to_owned());
            }
        }
    }

    let id = Uuid::new_v4().to_string();
    let body = serde_json::to_vec_pretty(&json!({
        "schema_version": 1,
        "install_id": id,
        "created_at": utc_now(),
    }))?;
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(id)
}

pub fn install_path(data_root: &Path) -> PathBuf {
    data_root.join(INSTALL_FILE)
}

pub fn device_id(data_root: &Path) -> Result<String> {
    let path = device_path(data_root)?;
    if path.exists() {
        let value: serde_json::Value = serde_json::from_slice(
            &fs::read(&path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("parse {}", path.display()))?;
        if let Some(id) = value.get("device_id").and_then(|value| value.as_str()) {
            if Uuid::parse_str(id.trim()).is_ok() {
                return Ok(id.trim().to_owned());
            }
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let id = Uuid::new_v4().to_string();
    let body = serde_json::to_vec_pretty(&json!({
        "schema_version": 1,
        "device_id": id,
        "created_at": utc_now(),
    }))?;
    write_private_file(&path, &body).with_context(|| format!("write {}", path.display()))?;
    Ok(id)
}

pub fn device_path(data_root: &Path) -> Result<PathBuf> {
    device_state_path(DEVICE_FILE, data_root)
}

pub(crate) fn device_state_path(file_name: &str, data_root: &Path) -> Result<PathBuf> {
    let path = device_state_dir()?.join(file_name);
    ensure_device_path_outside_data_root(&path, data_root)?;
    Ok(path)
}

pub(crate) fn ensure_device_path_outside_data_root(path: &Path, data_root: &Path) -> Result<()> {
    let normalized_path = normalize_for_prefix_check(path);
    let normalized_data_root = normalize_for_prefix_check(data_root);
    let resolved_path = resolve_for_prefix_check(&normalized_path)?;
    let resolved_data_root = resolve_for_prefix_check(&normalized_data_root)?;
    if normalized_path.starts_with(&normalized_data_root)
        || resolved_path.starts_with(&resolved_data_root)
    {
        bail!(
            "refusing to store telemetry state under ctx data root: {}",
            path.display()
        );
    }
    Ok(())
}

fn resolve_for_prefix_check(path: &Path) -> Result<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(&existing) {
            Ok(_) => break,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let name = existing
                    .file_name()
                    .context("resolve telemetry state path")?
                    .to_os_string();
                missing.push(name);
                existing = existing
                    .parent()
                    .context("resolve telemetry state parent")?
                    .to_path_buf();
            }
            Err(err) => return Err(err).with_context(|| format!("inspect {}", existing.display())),
        }
    }
    let mut resolved =
        fs::canonicalize(&existing).with_context(|| format!("resolve {}", existing.display()))?;
    for name in missing.into_iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

pub(crate) fn normalize_for_prefix_check(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn device_state_dir() -> Result<PathBuf> {
    if let Some(local_app_data) = non_empty_env_path("LOCALAPPDATA") {
        return Ok(local_app_data.join("ctx"));
    }
    Ok(home_dir()
        .context("resolve home directory")?
        .join("AppData")
        .join("Local")
        .join("ctx"))
}

#[cfg(target_os = "macos")]
pub(crate) fn device_state_dir() -> Result<PathBuf> {
    Ok(home_dir()
        .context("resolve home directory")?
        .join("Library")
        .join("Application Support")
        .join("ctx"))
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
pub(crate) fn device_state_dir() -> Result<PathBuf> {
    if let Some(xdg_state_home) = non_empty_env_path("XDG_STATE_HOME") {
        return Ok(xdg_state_home.join("ctx"));
    }
    Ok(home_dir()
        .context("resolve home directory")?
        .join(".local")
        .join("state")
        .join("ctx"))
}

pub(crate) fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Resolve the user home directory from `HOME`, falling back to the
/// Windows `USERPROFILE` and `HOMEDRIVE`+`HOMEPATH` conventions.
pub(crate) fn home_dir() -> Option<PathBuf> {
    non_empty_env_path("HOME")
        .or_else(|| non_empty_env_path("USERPROFILE"))
        .or_else(|| {
            let drive = env::var_os("HOMEDRIVE")?;
            let path = env::var_os("HOMEPATH")?;
            Some(PathBuf::from(format!(
                "{}{}",
                drive.to_string_lossy(),
                path.to_string_lossy()
            )))
        })
}

#[cfg(unix)]
pub(crate) fn write_private_file(path: &Path, body: &[u8]) -> Result<()> {
    use std::{
        fs::OpenOptions,
        io::Write,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };

    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(body)?;
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn write_private_file(path: &Path, body: &[u8]) -> Result<()> {
    use std::{fs::OpenOptions, io::Write, os::windows::fs::OpenOptionsExt};

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if file.metadata()?.file_type().is_symlink() {
        bail!("refusing to write telemetry state through a symlink");
    }
    file.set_len(0)?;
    file.write_all(body)?;
    Ok(())
}

#[cfg(not(any(unix, target_os = "windows")))]
pub(crate) fn write_private_file(path: &Path, body: &[u8]) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        bail!("refusing to write telemetry state through a symlink");
    }
    fs::write(path, body)?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn create_private_file(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};

    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(body)
}

#[cfg(not(unix))]
pub(crate) fn create_private_file(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::{fs::OpenOptions, io::Write};

    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(body)
}
