use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::platform::{
    apply_private_dir_permissions_sync, is_private_dir_boundary, metadata_is_link_or_reparse_point,
};

struct PrivateDirChainEntry {
    path: PathBuf,
    exists: bool,
    is_dir: bool,
    is_link_or_reparse_point: bool,
    is_private_boundary: bool,
}

pub(super) fn ensure_private_dir_chain_sync(path: &Path, create_missing: bool) -> Result<bool> {
    let entries = collect_private_dir_chain(path)?;
    if entries.is_empty() {
        return Ok(true);
    }

    let validate_through = entries
        .iter()
        .rposition(|entry| entry.is_private_boundary)
        .unwrap_or(entries.len() - 1);
    for entry in entries.iter().take(validate_through + 1) {
        validate_private_dir_chain_entry(entry)?;
    }

    let mut all_exist = true;
    for entry in entries.iter().take(validate_through + 1).rev() {
        match fs::symlink_metadata(&entry.path) {
            Ok(metadata) => validate_private_dir_metadata(&entry.path, &metadata)?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                all_exist = false;
                if !create_missing {
                    continue;
                }
                fs::create_dir(&entry.path).with_context(|| {
                    format!("creating private directory {}", entry.path.display())
                })?;
                let metadata = fs::symlink_metadata(&entry.path).with_context(|| {
                    format!("reading private directory {}", entry.path.display())
                })?;
                validate_private_dir_metadata(&entry.path, &metadata)?;
                apply_private_dir_permissions_sync(&entry.path)?;
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("reading private directory {}", entry.path.display())
                });
            }
        }
    }
    Ok(all_exist)
}

pub(super) fn reject_symlink_sync(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata_is_link_or_reparse_point(&metadata) {
                anyhow::bail!(
                    "private path must not be a symlink or reparse point: {}",
                    path.display()
                );
            }
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| format!("reading private path {}", path.display())),
    }
}

fn collect_private_dir_chain(path: &Path) -> Result<Vec<PrivateDirChainEntry>> {
    let mut entries = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        if current.as_os_str().is_empty() {
            break;
        }
        let entry = match fs::symlink_metadata(&current) {
            Ok(metadata) => PrivateDirChainEntry {
                path: current.clone(),
                exists: true,
                is_dir: metadata.is_dir(),
                is_link_or_reparse_point: metadata_is_link_or_reparse_point(&metadata),
                is_private_boundary: is_private_dir_boundary(&metadata),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => PrivateDirChainEntry {
                path: current.clone(),
                exists: false,
                is_dir: false,
                is_link_or_reparse_point: false,
                is_private_boundary: false,
            },
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("reading private directory {}", current.display()));
            }
        };
        entries.push(entry);
        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current || parent.as_os_str().is_empty() {
            break;
        }
        current = parent.to_path_buf();
    }
    Ok(entries)
}

fn validate_private_dir_chain_entry(entry: &PrivateDirChainEntry) -> Result<()> {
    if !entry.exists {
        return Ok(());
    }
    if entry.is_link_or_reparse_point {
        anyhow::bail!(
            "private directory path must not contain a symlink or reparse point: {}",
            entry.path.display()
        );
    }
    if !entry.is_dir {
        anyhow::bail!(
            "private directory path must be a directory: {}",
            entry.path.display()
        );
    }
    Ok(())
}

fn validate_private_dir_metadata(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata_is_link_or_reparse_point(metadata) {
        anyhow::bail!(
            "private directory path must not contain a symlink or reparse point: {}",
            path.display()
        );
    }
    if !metadata.is_dir() {
        anyhow::bail!(
            "private directory path must be a directory: {}",
            path.display()
        );
    }
    Ok(())
}
