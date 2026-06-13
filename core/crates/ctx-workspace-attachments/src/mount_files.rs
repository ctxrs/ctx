use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::workspace_attachments;
use anyhow::{Context, Result};
use ctx_core::models::{AttachmentMode, WorkspaceAttachment};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SymlinkCopyMode {
    Preserve,
    Reject,
}

pub async fn remove_mount_path(target: &Path) -> Result<()> {
    if let Ok(meta) = tokio::fs::symlink_metadata(target).await {
        if meta.file_type().is_symlink() || meta.is_file() {
            let target = target.to_path_buf();
            let target_for_clear = target.clone();
            tokio::task::spawn_blocking(move || clear_read_only_mode(&target_for_clear)).await??;
            tokio::fs::remove_file(target).await?;
        } else if meta.is_dir() {
            let target = target.to_path_buf();
            let target_for_clear = target.clone();
            tokio::task::spawn_blocking(move || clear_read_only_mode(&target_for_clear)).await??;
            tokio::fs::remove_dir_all(target).await?;
        }
    }
    Ok(())
}

pub async fn remove_mount_path_in_worktree(worktree_root: &Path, target: &Path) -> Result<()> {
    validate_mount_parent_chain(worktree_root, target, true)?;
    remove_mount_path(target).await
}

pub fn validate_mount_path_in_worktree(worktree_root: &Path, target: &Path) -> Result<()> {
    validate_mount_parent_chain(worktree_root, target, true)
}

pub async fn ensure_mount_in_worktree(
    worktree_root: &Path,
    mount_relpath: &Path,
    source: &Path,
    mode: AttachmentMode,
) -> Result<PathBuf> {
    let target = ensure_mount_parent_chain(worktree_root, mount_relpath)?;
    ensure_mount(&target, source, mode).await?;
    Ok(target)
}

fn ensure_mount_parent_chain(worktree_root: &Path, mount_relpath: &Path) -> Result<PathBuf> {
    validate_safe_relative_mount_path(mount_relpath)?;
    let target = worktree_root.join(mount_relpath);
    let parent = mount_relpath
        .parent()
        .ok_or_else(|| anyhow::anyhow!("attachment mount path must have a parent"))?;
    let mut current = worktree_root.to_path_buf();
    for component in parent.components() {
        let std::path::Component::Normal(segment) = component else {
            anyhow::bail!(
                "attachment mount path contains unsupported component: {}",
                mount_relpath.display()
            );
        };
        current.push(segment);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    anyhow::bail!(
                        "attachment mount parent must not be a symlink: {}",
                        current.display()
                    );
                }
                if !meta.is_dir() {
                    anyhow::bail!(
                        "attachment mount parent must be a directory: {}",
                        current.display()
                    );
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current).with_context(|| {
                    format!("creating attachment mount parent {}", current.display())
                })?;
                let meta = std::fs::symlink_metadata(&current).with_context(|| {
                    format!("verifying attachment mount parent {}", current.display())
                })?;
                if meta.file_type().is_symlink() || !meta.is_dir() {
                    anyhow::bail!(
                        "attachment mount parent was not created as a directory: {}",
                        current.display()
                    );
                }
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("reading attachment mount parent {}", current.display())
                });
            }
        }
    }
    Ok(target)
}

fn validate_mount_parent_chain(
    worktree_root: &Path,
    target: &Path,
    allow_missing: bool,
) -> Result<()> {
    let mount_relpath = target.strip_prefix(worktree_root).with_context(|| {
        format!(
            "attachment mount path {} is outside worktree {}",
            target.display(),
            worktree_root.display()
        )
    })?;
    validate_safe_relative_mount_path(mount_relpath)?;
    let parent = mount_relpath
        .parent()
        .ok_or_else(|| anyhow::anyhow!("attachment mount path must have a parent"))?;
    let mut current = worktree_root.to_path_buf();
    for component in parent.components() {
        let std::path::Component::Normal(segment) = component else {
            anyhow::bail!(
                "attachment mount path contains unsupported component: {}",
                mount_relpath.display()
            );
        };
        current.push(segment);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    anyhow::bail!(
                        "attachment mount parent must not be a symlink: {}",
                        current.display()
                    );
                }
                if !meta.is_dir() {
                    anyhow::bail!(
                        "attachment mount parent must be a directory: {}",
                        current.display()
                    );
                }
            }
            Err(err) if allow_missing && err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(());
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("reading attachment mount parent {}", current.display())
                });
            }
        }
    }
    Ok(())
}

fn validate_safe_relative_mount_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        anyhow::bail!("attachment mount path must not be empty");
    }
    if path.is_absolute() {
        anyhow::bail!("attachment mount path must be relative: {}", path.display());
    }
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) => {}
            _ => anyhow::bail!(
                "attachment mount path contains unsupported component: {}",
                path.display()
            ),
        }
    }
    Ok(())
}

pub(crate) async fn ensure_mount(target: &Path, source: &Path, mode: AttachmentMode) -> Result<()> {
    if let Ok(meta) = tokio::fs::symlink_metadata(target).await {
        if meta.file_type().is_symlink() {
            if let Ok(current) = tokio::fs::read_link(target).await {
                if current == source && mode == AttachmentMode::Rw {
                    return Ok(());
                }
            }
        } else if meta.is_dir() {
            if mode == AttachmentMode::Rw {
                let target = target.to_path_buf();
                let target_for_clear = target.clone();
                tokio::task::spawn_blocking(move || clear_read_only_mode(&target_for_clear))
                    .await??;
                tokio::fs::remove_dir_all(target).await?;
            }
        } else if mode == AttachmentMode::Rw {
            let target = target.to_path_buf();
            let target_for_clear = target.clone();
            tokio::task::spawn_blocking(move || clear_read_only_mode(&target_for_clear)).await??;
            tokio::fs::remove_file(target).await?;
        }
        if meta.file_type().is_symlink() && mode == AttachmentMode::Rw {
            tokio::fs::remove_file(target).await?;
        }
    }

    if mode == AttachmentMode::Ro {
        let source = source.to_path_buf();
        let target = target.to_path_buf();
        tokio::task::spawn_blocking(move || copy_path_recursive_read_only_atomic(&source, &target))
            .await??;
        return Ok(());
    }

    if let Err(err) = try_symlink_path(source, target).await {
        tracing::debug!("symlink failed ({err}); falling back to copy");
        let source = source.to_path_buf();
        let target = target.to_path_buf();
        tokio::task::spawn_blocking(move || {
            copy_path_recursive(&source, &target, SymlinkCopyMode::Preserve)
        })
        .await??;
    }
    Ok(())
}

async fn try_symlink_path(source: &Path, target: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        tokio::task::spawn_blocking({
            let source = source.to_path_buf();
            let target = target.to_path_buf();
            move || symlink(source, target)
        })
        .await??;
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{symlink_dir, symlink_file};
        let source_meta = tokio::fs::metadata(source)
            .await
            .with_context(|| format!("stat attachment source {}", source.display()))?;
        tokio::task::spawn_blocking({
            let source = source.to_path_buf();
            let target = target.to_path_buf();
            move || {
                if source_meta.is_dir() {
                    symlink_dir(source, target)
                } else {
                    symlink_file(source, target)
                }
            }
        })
        .await??;
        Ok(())
    }
}

fn copy_path_recursive(source: &Path, target: &Path, symlink_mode: SymlinkCopyMode) -> Result<()> {
    copy_path_recursive_inner(source, target, symlink_mode)
}

fn copy_path_recursive_read_only_atomic(source: &Path, target: &Path) -> Result<()> {
    copy_path_recursive_read_only_atomic_with(
        source,
        target,
        apply_read_only_mode_before_rename,
        apply_read_only_mode_root,
    )
}

fn copy_path_recursive_read_only_atomic_with<F, G>(
    source: &Path,
    target: &Path,
    apply_before_rename: F,
    apply_after_rename: G,
) -> Result<()>
where
    F: FnOnce(&Path) -> Result<()>,
    G: FnOnce(&Path) -> Result<()>,
{
    let temp = unique_copy_temp_path(target)?;
    if let Err(err) = copy_path_recursive(source, &temp, SymlinkCopyMode::Reject) {
        remove_path_after_failed_copy(&temp);
        return Err(err);
    }
    if let Err(err) = apply_before_rename(&temp) {
        remove_path_after_failed_copy(&temp);
        return Err(err);
    }
    let backup = prepare_existing_mount_backup(target)?;
    if let Err(err) = std::fs::rename(&temp, target)
        .with_context(|| format!("installing read-only attachment copy {}", target.display()))
    {
        remove_path_after_failed_copy(&temp);
        restore_mount_backup(target, backup.as_deref(), &err)?;
        return Err(err);
    }
    if let Err(err) = apply_after_rename(target) {
        remove_path_after_failed_copy(target);
        restore_mount_backup(target, backup.as_deref(), &err)?;
        return Err(err);
    }
    if let Some(backup) = backup.as_ref() {
        remove_path_for_swap(backup)?;
    }
    Ok(())
}

fn unique_copy_temp_path(target: &Path) -> Result<PathBuf> {
    unique_copy_sibling_path(target, "copy-tmp")
}

fn unique_copy_backup_path(target: &Path) -> Result<PathBuf> {
    unique_copy_sibling_path(target, "copy-old")
}

fn unique_copy_sibling_path(target: &Path, label: &str) -> Result<PathBuf> {
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("attachment mount target must have a parent"))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating attachment mount parent {}", parent.display()))?;
    let file_name = target
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("attachment mount target must have a file name"))?;
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
                    format!("checking attachment copy temp {}", candidate.display())
                });
            }
        }
    }
    anyhow::bail!(
        "unable to allocate sibling attachment copy path for {}",
        target.display()
    );
}

fn prepare_existing_mount_backup(target: &Path) -> Result<Option<PathBuf>> {
    match std::fs::symlink_metadata(target) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("reading attachment mount {}", target.display()));
        }
    }
    let backup = unique_copy_backup_path(target)?;
    std::fs::rename(target, &backup)
        .with_context(|| format!("staging previous attachment mount {}", target.display()))?;
    Ok(Some(backup))
}

fn restore_mount_backup(
    target: &Path,
    backup: Option<&Path>,
    install_err: &anyhow::Error,
) -> Result<()> {
    let Some(backup) = backup else {
        return Ok(());
    };
    if let Err(restore_err) = std::fs::rename(backup, target)
        .with_context(|| format!("restoring previous attachment mount {}", target.display()))
    {
        return Err(anyhow::anyhow!("{install_err:#}")).context(format!(
            "failed to restore previous attachment mount after install failure: {restore_err:#}"
        ));
    }
    Ok(())
}

fn copy_path_recursive_inner(
    source: &Path,
    target: &Path,
    symlink_mode: SymlinkCopyMode,
) -> Result<()> {
    let metadata = std::fs::symlink_metadata(source)
        .with_context(|| format!("reading attachment source metadata {}", source.display()))?;
    if metadata.file_type().is_symlink() {
        match symlink_mode {
            SymlinkCopyMode::Preserve => copy_symlink(source, target)?,
            SymlinkCopyMode::Reject => {
                anyhow::bail!(
                    "read-only attachment copy refuses symlink: {}",
                    source.display()
                );
            }
        }
        return Ok(());
    }
    if metadata.is_dir() {
        std::fs::create_dir_all(target)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let dest = target.join(entry.file_name());
            copy_path_recursive_inner(&entry.path(), &dest, symlink_mode)?;
        }
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(source, target)
        .with_context(|| format!("copying attachment file {}", source.display()))?;
    Ok(())
}

fn remove_path_after_failed_copy(path: &Path) {
    let _ = remove_path_for_swap(path);
}

fn remove_path_for_swap(path: &Path) -> Result<()> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    clear_read_only_mode(path)?;
    if metadata.file_type().is_symlink() || metadata.is_file() {
        std::fs::remove_file(path)
            .with_context(|| format!("removing attachment path {}", path.display()))?;
    } else if metadata.is_dir() {
        std::fs::remove_dir_all(path)
            .with_context(|| format!("removing attachment path {}", path.display()))?;
    }
    Ok(())
}

fn apply_read_only_mode_before_rename(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("reading attachment metadata {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)
            .with_context(|| format!("reading attachment dir {}", path.display()))?
        {
            let entry = entry?;
            apply_read_only_mode_recursive(&entry.path())?;
        }
        return Ok(());
    }
    set_read_only_permissions(path, &metadata)
}

fn apply_read_only_mode_root(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("reading attachment metadata {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    set_read_only_permissions(path, &metadata)
}

fn clear_read_only_mode(path: &Path) -> Result<()> {
    clear_read_only_mode_recursive(path)
}

fn apply_read_only_mode_recursive(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("reading attachment metadata {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)
            .with_context(|| format!("reading attachment dir {}", path.display()))?
        {
            let entry = entry?;
            apply_read_only_mode_recursive(&entry.path())?;
        }
    }
    set_read_only_permissions(path, &metadata)
}

fn set_read_only_permissions(path: &Path, metadata: &std::fs::Metadata) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() & !0o222);
        std::fs::set_permissions(path, permissions)
            .with_context(|| format!("setting read-only permissions on {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = metadata.permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(path, permissions)
            .with_context(|| format!("setting read-only permissions on {}", path.display()))?;
    }
    Ok(())
}

fn clear_read_only_mode_recursive(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("reading attachment metadata {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)
            .with_context(|| format!("reading attachment dir {}", path.display()))?
        {
            let entry = entry?;
            clear_read_only_mode_recursive(&entry.path())?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o200);
        std::fs::set_permissions(path, permissions)
            .with_context(|| format!("setting writable permissions on {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = metadata.permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions)
            .with_context(|| format!("setting writable permissions on {}", path.display()))?;
    }
    Ok(())
}

fn copy_symlink(source: &Path, target: &Path) -> Result<()> {
    let link_target = std::fs::read_link(source)
        .with_context(|| format!("reading attachment symlink {}", source.display()))?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&link_target, target)
            .with_context(|| format!("copying attachment symlink {}", source.display()))?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{symlink_dir, symlink_file};
        let target_metadata = std::fs::metadata(source)
            .with_context(|| format!("reading attachment symlink target {}", source.display()))?;
        if target_metadata.is_dir() {
            symlink_dir(&link_target, target)
                .with_context(|| format!("copying attachment symlink {}", source.display()))?;
        } else {
            symlink_file(&link_target, target)
                .with_context(|| format!("copying attachment symlink {}", source.display()))?;
        }
    }
    Ok(())
}

pub fn materialized_path_for_attachment(
    data_root: &Path,
    attachment: &WorkspaceAttachment,
) -> PathBuf {
    workspace_attachments::materialized_path_for_attachment(data_root, attachment)
}

pub fn sanitize_mount_relpath(value: &str) -> Result<PathBuf> {
    workspace_attachments::sanitize_mount_relpath(value)
}

pub fn revision_key(attachment: &WorkspaceAttachment) -> String {
    workspace_attachments::revision_key(attachment)
}

#[cfg(test)]
mod tests;
