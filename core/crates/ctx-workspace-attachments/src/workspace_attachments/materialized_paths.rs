use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use ctx_core::models::{WorkspaceAttachment, WorkspaceAttachmentKind};

pub fn materialized_root_for_attachment(
    data_root: &Path,
    attachment: &WorkspaceAttachment,
) -> PathBuf {
    match attachment.kind {
        WorkspaceAttachmentKind::ReferenceRepo => attachment_store_root(data_root)
            .join("reference-repos")
            .join("checkouts")
            .join(attachment.id.0.to_string()),
        WorkspaceAttachmentKind::DocMirror => attachment_store_root(data_root)
            .join("doc-mirrors")
            .join(attachment.id.0.to_string()),
    }
}

pub fn materialized_path_for_attachment(
    data_root: &Path,
    attachment: &WorkspaceAttachment,
) -> PathBuf {
    materialized_root_for_attachment(data_root, attachment).join(revision_key(attachment))
}

pub fn sanitize_mount_relpath(value: &str) -> Result<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("mount_relpath must not be empty");
    }
    if value.contains('\\') {
        anyhow::bail!("mount_relpath must use '/' separators: {value}");
    }
    for segment in value.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            anyhow::bail!("mount_relpath contains unsupported component: {value}");
        }
    }
    let path = PathBuf::from(value);
    if path.is_absolute() {
        anyhow::bail!("mount_relpath must be relative: {value}");
    }
    for part in path.components() {
        if !matches!(part, Component::Normal(_)) {
            anyhow::bail!("mount_relpath contains unsupported component: {value}");
        }
    }
    Ok(path)
}

pub fn sanitize_attachment_subpath(value: &str) -> Result<PathBuf> {
    if value.trim().is_empty() {
        anyhow::bail!("subpath must not be empty");
    }
    let path = PathBuf::from(value);
    if path.is_absolute() {
        anyhow::bail!("subpath must be relative: {value}");
    }
    for part in path.components() {
        if matches!(part, Component::ParentDir | Component::Prefix(_)) {
            anyhow::bail!("subpath must not escape the attachment root: {value}");
        }
    }
    Ok(path)
}

pub fn revision_key(attachment: &WorkspaceAttachment) -> String {
    let base = attachment.revision.as_deref().unwrap_or("default");
    sanitize_name(base)
}

pub async fn remove_materialized_root_if_exists(
    data_root: &Path,
    attachment: &WorkspaceAttachment,
) -> Result<()> {
    let root = materialized_root_for_attachment(data_root, attachment);
    remove_materialized_path_if_exists(data_root, &root).await
}

pub async fn validate_materialized_path(
    data_root: &Path,
    attachment: &WorkspaceAttachment,
) -> Result<()> {
    let path = materialized_path_for_attachment(data_root, attachment);
    let data_root = data_root.to_path_buf();
    tokio::task::spawn_blocking(move || validate_materialized_path_sync(&data_root, &path))
        .await
        .context("joining attachment materialization validation task")?
}

pub(crate) async fn ensure_materialized_revision_parent(
    data_root: &Path,
    attachment: &WorkspaceAttachment,
) -> Result<()> {
    let dest = materialized_path_for_attachment(data_root, attachment);
    ensure_materialized_parent(data_root, &dest).await
}

pub(super) async fn remove_materialized_path_if_exists(
    data_root: &Path,
    path: &Path,
) -> Result<()> {
    let data_root = data_root.to_path_buf();
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || remove_materialized_path_if_exists_sync(&data_root, &path))
        .await
        .context("joining attachment materialization cleanup task")?
}

pub(super) async fn ensure_materialized_parent(data_root: &Path, path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("attachment materialization path missing parent"))?
        .to_path_buf();
    let data_root = data_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        ensure_materialized_chain_sync(&data_root, &parent, MissingMaterializedDir::Create)
    })
    .await
    .context("joining attachment materialization parent task")?
}

pub(super) async fn materialized_path_exists_for_replace(
    data_root: &Path,
    path: &Path,
) -> Result<bool> {
    let data_root = data_root.to_path_buf();
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        materialized_path_exists_for_replace_sync(&data_root, &path)
    })
    .await
    .context("joining attachment materialization replacement validation task")?
}

pub(super) fn default_mount_relpath(kind: &WorkspaceAttachmentKind, name: &str) -> String {
    let safe_name = sanitize_name(name);
    match kind {
        WorkspaceAttachmentKind::ReferenceRepo => format!(".ctx/attachments/refs/{safe_name}"),
        WorkspaceAttachmentKind::DocMirror => format!(".ctx/attachments/docs/{safe_name}"),
    }
}

fn attachment_store_root(data_root: &Path) -> PathBuf {
    data_root.join("attachments")
}

fn sanitize_name(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "attachment".to_string()
    } else {
        trimmed.to_string()
    }
}

fn remove_materialized_path_if_exists_sync(data_root: &Path, path: &Path) -> Result<()> {
    validate_materialized_child_path(data_root, path)?;
    if let Some(parent) = path.parent() {
        ensure_materialized_chain_sync(data_root, parent, MissingMaterializedDir::AllowMissing)?;
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure_directory_metadata(&meta, path, "attachment materialization path")?;
            std::fs::remove_dir_all(path)
                .with_context(|| format!("removing attachment materialization {}", path.display()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err)
            .with_context(|| format!("reading attachment materialization {}", path.display())),
    }
}

fn validate_materialized_path_sync(data_root: &Path, path: &Path) -> Result<()> {
    validate_materialized_child_path(data_root, path)?;
    if let Some(parent) = path.parent() {
        ensure_materialized_chain_sync(data_root, parent, MissingMaterializedDir::AllowMissing)?;
    }
    let meta = std::fs::symlink_metadata(path)
        .with_context(|| format!("reading attachment materialization {}", path.display()))?;
    ensure_directory_metadata(&meta, path, "attachment materialization path")
}

fn materialized_path_exists_for_replace_sync(data_root: &Path, path: &Path) -> Result<bool> {
    validate_materialized_child_path(data_root, path)?;
    if let Some(parent) = path.parent() {
        ensure_materialized_chain_sync(data_root, parent, MissingMaterializedDir::AllowMissing)?;
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure_directory_metadata(&meta, path, "attachment materialization path")?;
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err)
            .with_context(|| format!("reading attachment materialization {}", path.display())),
    }
}

enum MissingMaterializedDir {
    Create,
    AllowMissing,
}

fn ensure_materialized_chain_sync(
    data_root: &Path,
    path: &Path,
    missing: MissingMaterializedDir,
) -> Result<()> {
    validate_materialized_child_path(data_root, path)?;
    let rel = path.strip_prefix(data_root).with_context(|| {
        format!(
            "attachment materialization path {} is outside data root {}",
            path.display(),
            data_root.display()
        )
    })?;
    let mut current = data_root.to_path_buf();
    for component in rel.components() {
        let Component::Normal(segment) = component else {
            anyhow::bail!(
                "attachment materialization path contains unsupported component: {}",
                path.display()
            );
        };
        current.push(segment);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                ensure_directory_metadata(&meta, &current, "attachment materialization parent")?
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => match missing {
                MissingMaterializedDir::Create => create_materialized_dir(&current)?,
                MissingMaterializedDir::AllowMissing => return Ok(()),
            },
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "reading attachment materialization parent {}",
                        current.display()
                    )
                });
            }
        }
    }
    Ok(())
}

fn create_materialized_dir(path: &Path) -> Result<()> {
    std::fs::create_dir(path).with_context(|| {
        format!(
            "creating attachment materialization parent {}",
            path.display()
        )
    })?;
    let meta = std::fs::symlink_metadata(path).with_context(|| {
        format!(
            "verifying attachment materialization parent {}",
            path.display()
        )
    })?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        anyhow::bail!(
            "attachment materialization parent was not created as a directory: {}",
            path.display()
        );
    }
    Ok(())
}

fn ensure_directory_metadata(meta: &std::fs::Metadata, path: &Path, subject: &str) -> Result<()> {
    if meta.file_type().is_symlink() {
        anyhow::bail!("{subject} must not be a symlink: {}", path.display());
    }
    if !meta.is_dir() {
        anyhow::bail!("{subject} must be a directory: {}", path.display());
    }
    Ok(())
}

fn validate_materialized_child_path(data_root: &Path, path: &Path) -> Result<()> {
    let store_root = attachment_store_root(data_root);
    if !path.starts_with(&store_root) {
        anyhow::bail!(
            "attachment materialization path {} is outside attachment store {}",
            path.display(),
            store_root.display()
        );
    }
    let rel = path.strip_prefix(data_root).with_context(|| {
        format!(
            "attachment materialization path {} is outside data root {}",
            path.display(),
            data_root.display()
        )
    })?;
    for component in rel.components() {
        if !matches!(component, Component::Normal(_)) {
            anyhow::bail!(
                "attachment materialization path contains unsupported component: {}",
                path.display()
            );
        }
    }
    Ok(())
}
