use std::path::Path;

use anyhow::{bail, Context, Result};

use ctx_core::models::{MergeQueueEntry, VcsKind, Workspace};
use ctx_fs::git::rev_parse_ref;
use tokio::fs;

use ctx_workspace_config::MergeQueueConfig;

use super::context::jj_rev_parse_bookmark;
use super::{
    maybe_update_worktree_base_commit_for_path, merge_queue_command, reset_worktree_to_commit,
    write_log_line, MergeQueueHost, QueueError,
};

#[allow(clippy::too_many_arguments)]
pub(super) async fn finalize_target_branch<H: MergeQueueHost>(
    state: &H,
    workspace: &Workspace,
    entry: &MergeQueueEntry,
    cfg: &MergeQueueConfig,
    vcs: &dyn ctx_fs::vcs::VcsDriver,
    repo_root: &Path,
    target_checkout: Option<&str>,
    target_head: &str,
    commit_sha: &str,
    log_file: &mut fs::File,
) -> std::result::Result<(), QueueError> {
    let workspace_root = Path::new(&workspace.root_path);
    let vcs_kind = vcs.kind();
    if cfg.push_on_success && vcs_kind == VcsKind::Git {
        ensure_git_target_branch_head(repo_root, &entry.target_branch, target_head, commit_sha)
            .await?;
        write_log_line(
            log_file,
            &format!(
                "push {} {}:{}\n",
                cfg.push_remote, commit_sha, cfg.push_branch
            ),
        )
        .await
        .map_err(|e| QueueError::fail(e.to_string(), None, Some(commit_sha.to_string())))?;
        if let Err(err) = push_target_branch(
            state,
            entry,
            repo_root,
            workspace_root,
            vcs_kind.clone(),
            &cfg.push_remote,
            commit_sha,
            &cfg.push_branch,
            commit_sha,
        )
        .await
        {
            return Err(QueueError::fail(
                format!("push_failed: {err}"),
                None,
                Some(commit_sha.to_string()),
            ));
        }
    }

    write_log_line(
        log_file,
        &format!("advance target branch {}\n", entry.target_branch),
    )
    .await
    .map_err(|e| QueueError::fail(e.to_string(), None, Some(commit_sha.to_string())))?;
    if let Some(path) = target_checkout {
        let previous_head = vcs
            .rev_parse_ref(Path::new(path), "HEAD")
            .await
            .map_err(|e| QueueError::fail(e.to_string(), None, Some(commit_sha.to_string())))?;
        if previous_head != target_head {
            return Err(QueueError::fail(
                format!(
                    "failed to update target branch: expected {target_head}, found {previous_head}"
                ),
                None,
                Some(commit_sha.to_string()),
            ));
        }
        reset_worktree_to_commit(state, entry, path, commit_sha)
            .await
            .map_err(|e| QueueError::fail(e.to_string(), None, Some(commit_sha.to_string())))?;
        let _ = maybe_update_worktree_base_commit_for_path(state, workspace.id, path, commit_sha)
            .await
            .map_err(|e| QueueError::fail(e.to_string(), None, Some(commit_sha.to_string())))?;
    } else {
        update_target_branch(
            state,
            entry,
            repo_root,
            workspace_root,
            vcs_kind.clone(),
            &entry.target_branch,
            commit_sha,
            target_head,
        )
        .await?;
    }

    if cfg.push_on_success && vcs_kind != VcsKind::Git {
        write_log_line(
            log_file,
            &format!(
                "push {} {}:{}\n",
                cfg.push_remote, entry.target_branch, cfg.push_branch
            ),
        )
        .await
        .map_err(|e| QueueError::fail(e.to_string(), None, Some(commit_sha.to_string())))?;
        if let Err(err) = push_target_branch(
            state,
            entry,
            repo_root,
            workspace_root,
            vcs_kind.clone(),
            &cfg.push_remote,
            &entry.target_branch,
            &cfg.push_branch,
            commit_sha,
        )
        .await
        {
            return Err(QueueError::fail(
                format!("push_failed: {err}"),
                None,
                Some(commit_sha.to_string()),
            ));
        }
    }

    Ok(())
}

pub(super) async fn ensure_git_target_branch_head(
    repo_root: &Path,
    target_branch: &str,
    expected_head: &str,
    commit_sha: &str,
) -> std::result::Result<(), QueueError> {
    let current = rev_parse_ref(repo_root, target_branch)
        .await
        .map_err(|e| QueueError::fail(e.to_string(), None, Some(commit_sha.to_string())))?;
    if current.trim() != expected_head.trim() {
        return Err(QueueError::fail(
            format!("target branch advanced (expected {expected_head}, found {current})"),
            None,
            Some(commit_sha.to_string()),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn update_target_branch<H: MergeQueueHost>(
    state: &H,
    entry: &MergeQueueEntry,
    repo_root: &Path,
    workspace_root: &Path,
    vcs_kind: VcsKind,
    target_branch: &str,
    commit_sha: &str,
    expected_old: &str,
) -> std::result::Result<(), QueueError> {
    match vcs_kind {
        VcsKind::Git => {
            let mut cmd =
                merge_queue_command(state, entry, "git update-ref", "git", Some(repo_root), &[])
                    .await;
            let output = cmd
                .arg("-C")
                .arg(repo_root)
                .args([
                    "update-ref",
                    &format!("refs/heads/{target_branch}"),
                    commit_sha,
                    expected_old,
                ])
                .output()
                .await
                .map_err(|e| QueueError::fail(e.to_string(), None, Some(commit_sha.to_string())))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                return Err(QueueError::fail(
                    format!("failed to update target branch: {stderr}"),
                    Some(output.status.code().unwrap_or(1) as i64),
                    Some(commit_sha.to_string()),
                ));
            }
            Ok(())
        }
        VcsKind::Jj => {
            let current = jj_rev_parse_bookmark(workspace_root, target_branch)
                .await
                .map_err(|e| QueueError::fail(e.to_string(), None, Some(commit_sha.to_string())))?;
            if current.trim() != expected_old.trim() {
                return Err(QueueError::fail(
                    format!("target branch advanced (expected {expected_old}, found {current})"),
                    None,
                    Some(commit_sha.to_string()),
                ));
            }
            let mut cmd = merge_queue_command(
                state,
                entry,
                "jj bookmark set",
                "jj",
                Some(workspace_root),
                &[],
            )
            .await;
            let output = cmd
                .arg("-R")
                .arg(workspace_root)
                .arg("--color=never")
                .arg("--no-pager")
                .args(["bookmark", "set", target_branch, "-r", commit_sha])
                .output()
                .await
                .map_err(|e| QueueError::fail(e.to_string(), None, Some(commit_sha.to_string())))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                return Err(QueueError::fail(
                    format!("failed to update target bookmark: {stderr}"),
                    Some(output.status.code().unwrap_or(1) as i64),
                    Some(commit_sha.to_string()),
                ));
            }
            Ok(())
        }
        VcsKind::Hg | VcsKind::Svn | VcsKind::P4 | VcsKind::Other => Err(QueueError::fail(
            format!("merge queue does not support {vcs_kind:?} branches"),
            None,
            Some(commit_sha.to_string()),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn push_target_branch<H: MergeQueueHost>(
    state: &H,
    entry: &MergeQueueEntry,
    repo_root: &Path,
    workspace_root: &Path,
    vcs_kind: VcsKind,
    remote: &str,
    source_ref: &str,
    push_branch: &str,
    commit_sha: &str,
) -> Result<()> {
    match vcs_kind {
        VcsKind::Git => {
            let mut cmd =
                merge_queue_command(state, entry, "git push", "git", Some(repo_root), &[]).await;
            let output = cmd
                .arg("-C")
                .arg(repo_root)
                .args(["push", remote, &format!("{source_ref}:{push_branch}")])
                .output()
                .await
                .context("running git push")?;
            if !output.status.success() {
                bail!(
                    "git push failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Ok(())
        }
        VcsKind::Jj => {
            if source_ref != push_branch {
                let mut cmd = merge_queue_command(
                    state,
                    entry,
                    "jj bookmark set",
                    "jj",
                    Some(workspace_root),
                    &[],
                )
                .await;
                let output = cmd
                    .arg("-R")
                    .arg(workspace_root)
                    .arg("--color=never")
                    .arg("--no-pager")
                    .args(["bookmark", "set", push_branch, "-r", commit_sha])
                    .output()
                    .await
                    .context("running jj bookmark set")?;
                if !output.status.success() {
                    bail!(
                        "jj bookmark set failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
            let mut cmd =
                merge_queue_command(state, entry, "jj git push", "jj", Some(workspace_root), &[])
                    .await;
            let output = cmd
                .arg("-R")
                .arg(workspace_root)
                .arg("--color=never")
                .arg("--no-pager")
                .args(["git", "push", "--remote", remote, "--bookmark", push_branch])
                .output()
                .await
                .context("running jj git push")?;
            if !output.status.success() {
                bail!(
                    "jj git push failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Ok(())
        }
        VcsKind::Hg | VcsKind::Svn | VcsKind::P4 | VcsKind::Other => {
            bail!("merge queue does not support {vcs_kind:?} pushes");
        }
    }
}
