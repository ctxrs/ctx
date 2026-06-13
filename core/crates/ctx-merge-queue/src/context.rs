use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

use ctx_core::ids::{SessionId, WorktreeId};
use ctx_core::models::{VcsKind, Workspace, Worktree};
use ctx_fs::vcs::{self, VcsDriver};

use crate::MergeQueueHost;

pub(super) struct MergeQueueWorktreeContext {
    pub(super) workspace: Workspace,
    pub(super) worktree: Option<Worktree>,
    pub(super) worktree_root: PathBuf,
    pub(super) vcs: Arc<dyn VcsDriver>,
}

pub(super) async fn resolve_merge_queue_context<H: MergeQueueHost>(
    state: &Arc<H>,
    session_id: Option<SessionId>,
    worktree_id: Option<WorktreeId>,
    worktree_root: Option<String>,
) -> Result<MergeQueueWorktreeContext> {
    if let Some(worktree_id) = worktree_id {
        let store = H::worktree_store(state.as_ref(), worktree_id).await?;
        let worktree = store
            .get_worktree(worktree_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("worktree not found"))?;
        let workspace = H::get_workspace(state.as_ref(), worktree.workspace_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("workspace not found"))?;
        let vcs = super::vcs_driver_for_worktree(&worktree);
        let worktree_root = PathBuf::from(&worktree.root_path);
        return Ok(MergeQueueWorktreeContext {
            workspace,
            worktree: Some(worktree),
            worktree_root,
            vcs,
        });
    }

    let session_id = session_id.ok_or_else(|| anyhow::anyhow!("session_id is required"))?;
    let store = H::session_store(state.as_ref(), session_id).await?;
    let session = store
        .get_session(session_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("session not found"))?;
    let workspace = H::get_workspace(state.as_ref(), session.workspace_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("workspace not found"))?;

    if let Some(root) = worktree_root {
        let root_path = PathBuf::from(root.trim());
        if !root_path.is_absolute() {
            bail!("worktree_root must be an absolute path");
        }
        let root_string = root_path.to_string_lossy().to_string();
        if let Some(worktree) = store
            .get_worktree_for_root(workspace.id, &root_string)
            .await?
        {
            let vcs = super::vcs_driver_for_worktree(&worktree);
            let worktree_root = PathBuf::from(&worktree.root_path);
            return Ok(MergeQueueWorktreeContext {
                workspace,
                worktree: Some(worktree),
                worktree_root,
                vcs,
            });
        }
        let vcs = vcs::driver_for_path(&root_path).await?;
        return Ok(MergeQueueWorktreeContext {
            workspace,
            worktree: None,
            worktree_root: root_path,
            vcs,
        });
    }

    let worktree = store
        .get_worktree(session.worktree_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("worktree not found"))?;
    let vcs = super::vcs_driver_for_worktree(&worktree);
    let worktree_root = PathBuf::from(&worktree.root_path);
    Ok(MergeQueueWorktreeContext {
        workspace,
        worktree: Some(worktree),
        worktree_root,
        vcs,
    })
}

pub(super) async fn resolve_target_head(
    vcs: &dyn VcsDriver,
    workspace_root: &str,
    target_branch: &str,
) -> Result<String> {
    match vcs.kind() {
        VcsKind::Git => vcs
            .rev_parse_ref(Path::new(workspace_root), target_branch)
            .await
            .context("resolving target branch"),
        VcsKind::Jj => jj_rev_parse_bookmark(Path::new(workspace_root), target_branch)
            .await
            .context("resolving target bookmark"),
        VcsKind::Hg | VcsKind::Svn | VcsKind::P4 | VcsKind::Other => {
            bail!("merge queue does not support {:?}", vcs.kind());
        }
    }
}

pub(super) async fn jj_rev_parse_bookmark(root: &Path, bookmark: &str) -> Result<String> {
    let output = Command::new("jj")
        .arg("-R")
        .arg(root)
        .arg("--color=never")
        .arg("--no-pager")
        .args(["log", "-r", bookmark, "--no-graph", "-T", "commit_id"])
        .output()
        .await
        .context("running jj log")?;
    if !output.status.success() {
        bail!(
            "jj log failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let revision = stdout
        .split_whitespace()
        .last()
        .ok_or_else(|| anyhow::anyhow!("jj log produced no revision output"))?;
    Ok(revision.to_string())
}

pub(super) async fn find_checked_out_worktree_for_branch<H: MergeQueueHost>(
    state: &H,
    entry: &ctx_core::models::MergeQueueEntry,
    workspace_root: &Path,
    target_branch: &str,
) -> Result<Option<String>> {
    let mut cmd = super::merge_queue_command(
        state,
        entry,
        "git worktree list --porcelain",
        "git",
        Some(workspace_root),
        &[],
    )
    .await;
    let output = cmd
        .arg("-C")
        .arg(workspace_root)
        .args(["worktree", "list", "--porcelain"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .context("running git worktree list --porcelain")?;
    if !output.status.success() {
        bail!(
            "git worktree list failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut current_path: Option<String> = None;
    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(path.trim().to_string());
            continue;
        }
        if let Some(branch) = line.strip_prefix("branch ") {
            let branch = branch.trim();
            if branch == format!("refs/heads/{target_branch}") {
                if let Some(path) = current_path.clone() {
                    return Ok(Some(path));
                }
            }
            continue;
        }
        if line.trim().is_empty() {
            current_path = None;
        }
    }
    Ok(None)
}
