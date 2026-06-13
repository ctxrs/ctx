use std::path::Path;

use anyhow::{Context, Result};
use ctx_core::models::WorkspaceAttachment;
use tokio::process::Command;

use super::materialized_install::{
    cleanup_materialized_temp, install_materialized_temp, unique_materialized_temp_path,
};
use super::materialized_paths::{ensure_materialized_revision_parent, validate_materialized_path};
use super::{materialized_path_for_attachment, revision_key, MaterializationResult};

pub(super) async fn materialize_reference_repo(
    data_root: &Path,
    attachment: &WorkspaceAttachment,
    refresh: bool,
) -> Result<MaterializationResult> {
    let revision = revision_key(attachment);
    let dest = materialized_path_for_attachment(data_root, attachment);
    let should_update = refresh || !dest.exists();
    if should_update {
        let temp = unique_materialized_temp_path(&dest)?;
        ensure_materialized_revision_parent(data_root, attachment).await?;
        if let Err(err) =
            clone_reference_repo(&attachment.source, attachment.revision.as_deref(), &temp).await
        {
            cleanup_materialized_temp(data_root, &temp).await;
            return Err(err);
        }
        install_materialized_temp(data_root, &temp, &dest).await?;
    } else {
        validate_materialized_path(data_root, attachment).await?;
    }
    Ok(MaterializationResult {
        path: dest,
        materialized_id: revision,
    })
}

pub(super) fn validate_reference_repo_source(source: &str) -> Result<()> {
    if looks_like_remote_repo_source(source) || Path::new(source).is_absolute() {
        return Ok(());
    }
    anyhow::bail!(
        "reference_repo local source must be an absolute path or repository URL: {source}"
    );
}

async fn clone_reference_repo(source: &str, revision: Option<&str>, dest: &Path) -> Result<()> {
    let mut clone_cmd = Command::new("git");
    clone_cmd
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--no-tags")
        .kill_on_drop(true);
    if let Some(rev) = revision {
        if !looks_like_sha(rev) {
            clone_cmd.arg("--branch").arg(rev);
        }
    }
    clone_cmd.arg(source).arg(dest);
    run_git_command(&mut clone_cmd, "running git clone", "git clone failed").await?;

    if let Some(rev) = revision.filter(|value| looks_like_sha(value)) {
        let mut fetch_cmd = Command::new("git");
        fetch_cmd
            .arg("-C")
            .arg(dest)
            .arg("fetch")
            .arg("--depth")
            .arg("1")
            .arg("origin")
            .arg(rev)
            .kill_on_drop(true);
        run_git_command(&mut fetch_cmd, "running git fetch", "git fetch failed").await?;

        let mut checkout_cmd = Command::new("git");
        checkout_cmd
            .arg("-C")
            .arg(dest)
            .arg("checkout")
            .arg(rev)
            .kill_on_drop(true);
        run_git_command(
            &mut checkout_cmd,
            "running git checkout",
            "git checkout failed",
        )
        .await?;
    }

    Ok(())
}

async fn run_git_command(cmd: &mut Command, context: &str, error_prefix: &str) -> Result<()> {
    let output = cmd.output().await.with_context(|| context.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    anyhow::bail!(
        "{error_prefix}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn looks_like_sha(value: &str) -> bool {
    let len = value.len();
    if !(7..=40).contains(&len) {
        return false;
    }
    value.chars().all(|c| c.is_ascii_hexdigit())
}

fn looks_like_remote_repo_source(source: &str) -> bool {
    if source.contains("://") {
        return true;
    }
    let Some((user_host, path)) = source.split_once(':') else {
        return false;
    };
    if path.is_empty() {
        return false;
    }
    if user_host.contains('/') || user_host.contains('\\') {
        return false;
    }
    if user_host == "." || user_host == ".." {
        return false;
    }
    if user_host.len() == 1 && user_host.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return false;
    }
    true
}
