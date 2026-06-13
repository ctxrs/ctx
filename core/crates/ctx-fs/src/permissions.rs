use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

mod chain;
mod file_ops;
mod platform;
#[cfg(all(test, unix))]
mod tests;

pub const PRIVATE_DIR_MODE: u32 = 0o700;
pub const PRIVATE_FILE_MODE: u32 = 0o600;

pub fn ensure_private_dir_sync(path: &Path) -> Result<()> {
    chain::ensure_private_dir_chain_sync(path, true)?;
    platform::apply_private_dir_permissions_sync(path)
}

pub async fn ensure_private_dir(path: &Path) -> Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || ensure_private_dir_sync(&path))
        .await
        .context("joining private directory creation task")?
}

pub fn harden_private_dir_sync(path: &Path) -> Result<()> {
    if !chain::ensure_private_dir_chain_sync(path, false)? {
        anyhow::bail!("private directory path not found: {}", path.display());
    }
    platform::apply_private_dir_permissions_sync(path)
}

pub fn harden_private_file_sync(path: &Path) -> Result<()> {
    if let Some(parent) = private_parent(path) {
        chain::ensure_private_dir_chain_sync(parent, false)?;
    }
    if !chain::reject_symlink_sync(path)? {
        anyhow::bail!("private file path not found: {}", path.display());
    }
    platform::apply_private_file_permissions_sync(path)
}

pub fn reject_symlink_sync(path: &Path) -> Result<bool> {
    chain::reject_symlink_sync(path)
}

pub fn read_private_file_to_string_sync(path: &Path) -> Result<Option<String>> {
    file_ops::read_private_file_to_string_sync(path)
}

pub async fn harden_private_file_if_exists(path: &Path) -> Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        if let Some(parent) = private_parent(&path) {
            if !chain::ensure_private_dir_chain_sync(parent, false)? {
                return Ok(());
            }
        }
        if chain::reject_symlink_sync(&path)? {
            platform::apply_private_file_permissions_sync(&path)?;
        }
        Ok(())
    })
    .await
    .context("joining private file chmod task")?
}

pub fn write_private_file_atomic_sync(path: &Path, bytes: &[u8]) -> Result<()> {
    file_ops::write_private_file_atomic_sync(path, bytes)
}

pub async fn write_private_file_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let path = path.to_path_buf();
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || write_private_file_atomic_sync(&path, &bytes))
        .await
        .context("joining private file write task")?
}

pub fn open_private_append_sync(path: &Path) -> Result<File> {
    file_ops::open_private_append_sync(path)
}

pub async fn open_private_append(path: &Path) -> Result<tokio::fs::File> {
    let path = path.to_path_buf();
    let file = tokio::task::spawn_blocking(move || open_private_append_sync(&path))
        .await
        .context("joining private append open task")??;
    Ok(tokio::fs::File::from_std(file))
}

pub async fn harden_sqlite_file_family(path: &Path) -> Result<()> {
    for file in sqlite_file_family(path) {
        harden_private_file_if_exists(&file).await?;
    }
    Ok(())
}

pub fn harden_private_directory_files_sync(
    dir: &Path,
    should_harden: impl Fn(&str) -> bool,
) -> Result<()> {
    if !chain::ensure_private_dir_chain_sync(dir, false)? {
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("reading {}", dir.display())),
    };

    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry under {}", dir.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("reading file type for {}", entry.path().display()))?;
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if should_harden(&name) {
            harden_private_file_sync(&entry.path())?;
        }
    }
    Ok(())
}

pub(super) fn private_parent(path: &Path) -> Option<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

fn sqlite_file_family(path: &Path) -> [PathBuf; 3] {
    [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.to_string_lossy())),
        PathBuf::from(format!("{}-shm", path.to_string_lossy())),
    ]
}
