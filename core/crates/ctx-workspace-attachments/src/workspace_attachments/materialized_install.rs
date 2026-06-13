use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use super::materialized_paths::{
    materialized_path_exists_for_replace, remove_materialized_path_if_exists,
};

pub(super) fn unique_materialized_temp_path(dest: &Path) -> Result<PathBuf> {
    unique_materialized_sibling_path(dest, "materialize-tmp")
}

pub(super) async fn install_materialized_temp(
    data_root: &Path,
    temp: &Path,
    dest: &Path,
) -> Result<()> {
    let backup = match materialized_path_exists_for_replace(data_root, dest).await {
        Ok(true) => Some(unique_materialized_backup_path(dest)?),
        Ok(false) => None,
        Err(err) => {
            cleanup_materialized_temp(data_root, temp).await;
            return Err(err);
        }
    };

    if let Some(backup) = backup.as_ref() {
        if let Err(err) = tokio::fs::rename(dest, backup).await.with_context(|| {
            format!(
                "staging previous attachment materialization {}",
                dest.display()
            )
        }) {
            cleanup_materialized_temp(data_root, temp).await;
            return Err(err);
        }
    }

    if let Err(err) = tokio::fs::rename(temp, dest)
        .await
        .with_context(|| format!("installing attachment materialization {}", dest.display()))
    {
        cleanup_materialized_temp(data_root, temp).await;
        if let Some(backup) = backup.as_ref() {
            if let Err(restore_err) = tokio::fs::rename(backup, dest).await.with_context(|| {
                format!(
                    "restoring previous attachment materialization {}",
                    dest.display()
                )
            }) {
                return Err(err).context(format!(
                    "failed to restore previous attachment materialization after install failure: {restore_err:#}"
                ));
            }
        }
        return Err(err);
    }

    if let Some(backup) = backup.as_ref() {
        remove_materialized_path_if_exists(data_root, backup).await?;
    }
    Ok(())
}

pub(super) async fn cleanup_materialized_temp(data_root: &Path, temp: &Path) {
    let _ = remove_materialized_path_if_exists(data_root, temp).await;
}

fn unique_materialized_backup_path(dest: &Path) -> Result<PathBuf> {
    unique_materialized_sibling_path(dest, "materialize-old")
}

fn unique_materialized_sibling_path(dest: &Path, label: &str) -> Result<PathBuf> {
    let parent = dest
        .parent()
        .ok_or_else(|| anyhow::anyhow!("attachment materialization path missing parent"))?;
    let file_name = dest
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("attachment materialization path missing file name"))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for attempt in 0..100 {
        let mut temp_name = OsString::from(".");
        temp_name.push(file_name);
        temp_name.push(format!(
            ".{label}.{}.{}.{}",
            std::process::id(),
            nanos,
            attempt
        ));
        let candidate = parent.join(temp_name);
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => continue,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "checking attachment materialization temp {}",
                        candidate.display()
                    )
                });
            }
        }
    }
    anyhow::bail!(
        "unable to allocate sibling attachment materialization path for {}",
        dest.display()
    );
}
