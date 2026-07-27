use std::{fs, path::Path};

use anyhow::{Context, Result};

use super::super::durability::backup_file_for_atomic_replace;
use super::PublishedPathKind;

pub(super) fn backup_target(
    target: &Path,
    backup: &Path,
    label: &str,
    kind: PublishedPathKind,
) -> Result<()> {
    match kind {
        PublishedPathKind::File => backup_file_for_atomic_replace(target, backup, label),
        PublishedPathKind::Directory => fs::rename(target, backup).with_context(|| {
            format!(
                "backup {label} {} to {}",
                target.display(),
                backup.display()
            )
        }),
    }
}

pub(super) fn publish_staged(staged: &Path, target: &Path, label: &str) -> Result<()> {
    fs::rename(staged, target).with_context(|| {
        format!(
            "publish {label} {} to {}",
            staged.display(),
            target.display()
        )
    })
}

pub(super) fn remove_published(
    target: &Path,
    backup: &Path,
    label: &str,
    kind: PublishedPathKind,
) -> Result<()> {
    remove_path(target, kind).with_context(|| {
        format!(
            "remove newly published {label} at {}; recoverable backup is {}",
            target.display(),
            backup.display()
        )
    })
}

pub(super) fn restore_backup(backup: &Path, target: &Path, label: &str) -> Result<()> {
    fs::rename(backup, target).with_context(|| {
        format!(
            "restore {label} {} from recoverable backup {}",
            target.display(),
            backup.display()
        )
    })
}

pub(super) fn discard_backup(backup: &Path, label: &str, kind: PublishedPathKind) -> Result<()> {
    remove_path(backup, kind)
        .with_context(|| format!("remove previous {label} {}", backup.display()))
}

pub(super) fn retain_backup_as(backup: &Path, durable_backup: &Path, label: &str) -> Result<()> {
    if durable_backup.exists() {
        fs::remove_file(durable_backup)
            .with_context(|| format!("remove old ctx backup {}", durable_backup.display()))?;
    }
    fs::rename(backup, durable_backup).with_context(|| {
        format!(
            "retain previous {label} {} at {}",
            backup.display(),
            durable_backup.display()
        )
    })
}

pub(super) fn remove_path(path: &Path, kind: PublishedPathKind) -> std::io::Result<()> {
    match kind {
        PublishedPathKind::File => fs::remove_file(path),
        PublishedPathKind::Directory => fs::remove_dir_all(path),
    }
}
