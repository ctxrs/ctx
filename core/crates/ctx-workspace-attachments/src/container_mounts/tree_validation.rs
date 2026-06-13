use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::AttachmentSourceSymlinkPolicy;

pub(super) async fn validate_read_only_attachment_import_tree(root: &Path) -> Result<()> {
    let root_canonical = tokio::fs::canonicalize(root).await.with_context(|| {
        format!(
            "canonicalizing attachment materialized root {}",
            root.display()
        )
    })?;
    validate_attachment_tree_within_root(
        &root_canonical,
        &root_canonical,
        AttachmentSourceSymlinkPolicy::Reject,
    )
    .await
    .with_context(|| {
        format!(
            "validating read-only attachment import tree {}",
            root.display()
        )
    })
}

pub(super) async fn validate_attachment_tree_within_root(
    root: &Path,
    candidate: &Path,
    symlink_policy: AttachmentSourceSymlinkPolicy,
) -> Result<()> {
    let root = root.to_path_buf();
    let candidate = candidate.to_path_buf();
    tokio::task::spawn_blocking(move || {
        validate_attachment_tree_within_root_blocking(&root, &candidate, symlink_policy)
    })
    .await??;
    Ok(())
}

fn validate_attachment_tree_within_root_blocking(
    root: &Path,
    candidate: &Path,
    symlink_policy: AttachmentSourceSymlinkPolicy,
) -> Result<()> {
    let metadata = std::fs::symlink_metadata(candidate)
        .with_context(|| format!("reading attachment metadata {}", candidate.display()))?;
    if metadata.file_type().is_symlink() {
        match symlink_policy {
            AttachmentSourceSymlinkPolicy::AllowInternal => {
                validate_attachment_symlink_target(root, candidate)?;
            }
            AttachmentSourceSymlinkPolicy::Reject => {
                anyhow::bail!(
                    "read-only attachment copy refuses symlink: {}",
                    candidate.display()
                );
            }
        }
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(candidate)
            .with_context(|| format!("reading attachment dir {}", candidate.display()))?
        {
            let entry = entry?;
            validate_attachment_tree_within_root_blocking(root, &entry.path(), symlink_policy)?;
        }
    }
    Ok(())
}

fn validate_attachment_symlink_target(root: &Path, path: &Path) -> Result<()> {
    let target = std::fs::read_link(path)
        .with_context(|| format!("reading attachment symlink {}", path.display()))?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("attachment path missing parent for symlink validation"))?;
    let lexical = resolve_attachment_path_lexical(root, parent, &target)?;
    if let Ok(canonical) = std::fs::canonicalize(&lexical) {
        if !canonical.starts_with(root) {
            anyhow::bail!(
                "attachment symlink escapes the materialized root: {} -> {}",
                path.display(),
                target.display()
            );
        }
    }
    Ok(())
}

fn resolve_attachment_path_lexical(root: &Path, base: &Path, target: &Path) -> Result<PathBuf> {
    let candidate = if target.is_absolute() {
        target.to_path_buf()
    } else {
        base.join(target)
    };
    let mut is_abs = false;
    let mut parts = Vec::new();
    for component in candidate.components() {
        use std::path::Component;
        match component {
            Component::Prefix(_) => anyhow::bail!("unsupported attachment path prefix"),
            Component::RootDir => {
                is_abs = true;
                parts.clear();
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.is_empty() {
                    continue;
                }
                parts.pop();
            }
            Component::Normal(segment) => parts.push(segment.to_os_string()),
        }
    }
    let mut normalized = PathBuf::new();
    if is_abs {
        normalized.push(std::path::MAIN_SEPARATOR.to_string());
    }
    for part in parts {
        normalized.push(part);
    }
    if !normalized.starts_with(root) {
        anyhow::bail!(
            "attachment symlink escapes the materialized root: {}",
            target.display()
        );
    }
    Ok(normalized)
}
