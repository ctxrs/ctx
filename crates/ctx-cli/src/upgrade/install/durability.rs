use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};

use super::super::download::DownloadedArtifact;
use super::super::path::ctx_binary_version;

pub(super) fn stage_downloaded_binary(
    staged: &Path,
    target: &Path,
    artifact: &mut DownloadedArtifact,
    expected_version: &str,
) -> Result<()> {
    let mut file = fs::File::create(staged)
        .with_context(|| format!("create staged artifact {}", staged.display()))?;
    artifact.copy_verified_to(&mut file)?;
    file.sync_all()?;
    drop(file);
    make_executable(staged, target)?;
    verify_staged_version(staged, expected_version)
}

fn verify_staged_version(staged: &Path, expected_version: &str) -> Result<()> {
    let version = ctx_binary_version(staged)
        .with_context(|| format!("run staged ctx {}", staged.display()))?;
    if !version.matches_exactly(expected_version) {
        return Err(anyhow!(
            "staged ctx version mismatch: expected {expected_version}, got {}",
            version.trim()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn make_executable(staged: &Path, target: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(target)
        .map(|metadata| metadata.permissions().mode())
        .unwrap_or(0o755)
        | 0o111;
    fs::set_permissions(staged, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_staged: &Path, _target: &Path) -> Result<()> {
    Ok(())
}

pub(super) fn backup_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("ctx");
    target.with_file_name(format!("{name}.previous"))
}

#[cfg(unix)]
pub(super) fn backup_file_for_atomic_replace(
    target: &Path,
    backup: &Path,
    label: &str,
) -> Result<()> {
    fs::hard_link(target, backup).with_context(|| {
        format!(
            "backup {label} {} to {} with a hard link",
            target.display(),
            backup.display()
        )
    })
}

#[cfg(unix)]
pub(super) fn sync_parent(parent: &Path) {
    let _ = sync_directory(parent);
}

#[cfg(not(unix))]
pub(super) fn sync_parent(_parent: &Path) {}

#[cfg(unix)]
pub(super) fn sync_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .with_context(|| format!("sync directory {}", path.display()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn hard_link_backup_failure_does_not_fall_back_to_copy() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("ctx");
        let backup = temp.path().join("missing").join("ctx.previous");
        fs::write(&target, b"old").unwrap();
        assert!(backup_file_for_atomic_replace(&target, &backup, "ctx binary").is_err());
        assert!(!backup.exists());
    }
}
