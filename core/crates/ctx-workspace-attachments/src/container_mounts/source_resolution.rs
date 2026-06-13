use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ctx_core::models::AttachmentMode;

use super::validate_attachment_tree_within_root;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum AttachmentSourceSymlinkPolicy {
    AllowInternal,
    Reject,
}

pub(super) fn symlink_policy_for_mode(mode: &AttachmentMode) -> AttachmentSourceSymlinkPolicy {
    match mode {
        AttachmentMode::Ro => AttachmentSourceSymlinkPolicy::Reject,
        AttachmentMode::Rw => AttachmentSourceSymlinkPolicy::AllowInternal,
    }
}

pub(super) async fn resolve_attachment_source_path(
    root: &Path,
    subpath: Option<&str>,
    symlink_policy: AttachmentSourceSymlinkPolicy,
) -> Result<PathBuf> {
    let root_canonical = tokio::fs::canonicalize(root)
        .await
        .unwrap_or_else(|_| root.to_path_buf());
    let candidate = match subpath {
        Some(subpath) => root.join(subpath),
        None => root.to_path_buf(),
    };
    let candidate_canonical = tokio::fs::canonicalize(&candidate)
        .await
        .with_context(|| format!("attachment source not found at {}", candidate.display()))?;
    if !candidate_canonical.starts_with(&root_canonical) {
        anyhow::bail!("attachment subpath escapes the materialized root");
    }
    validate_attachment_tree_within_root(&root_canonical, &candidate_canonical, symlink_policy)
        .await?;
    Ok(candidate_canonical)
}

pub(super) fn container_path_for_resolved_source(
    materialized_root: &Path,
    resolved_source: &Path,
    imported_root: &Path,
) -> Result<PathBuf> {
    let root_canonical = std::fs::canonicalize(materialized_root).with_context(|| {
        format!(
            "canonicalizing attachment materialized root {}",
            materialized_root.display()
        )
    })?;
    let relative = resolved_source
        .strip_prefix(&root_canonical)
        .with_context(|| {
            format!(
                "resolved attachment source {} is outside materialized root {}",
                resolved_source.display(),
                root_canonical.display()
            )
        })?;
    if relative.as_os_str().is_empty() {
        Ok(imported_root.to_path_buf())
    } else {
        Ok(imported_root.join(relative))
    }
}
