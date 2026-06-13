use std::path::Path;

use anyhow::Context;
use ctx_core::ids::TaskId;
use ctx_core::models::{Workspace, Worktree};
use ctx_worktree_vcs_service::remove_worktree;

use super::super::branches::WorktreeBranchCleanup;

pub(super) async fn remove_orphaned_worktree_dir(
    workspace: &Workspace,
    task_id: TaskId,
    worktree: &Worktree,
    root: &Path,
    errors: &mut Vec<anyhow::Error>,
) {
    if tokio::fs::metadata(root).await.is_err() {
        return;
    }
    if let Err(err) = tokio::fs::remove_dir_all(root)
        .await
        .with_context(|| format!("removing orphaned worktree dir at {}", root.display()))
    {
        tracing::warn!(
            task_id = %task_id.0,
            worktree_id = %worktree.id.0,
            workspace_root = %workspace.root_path,
            "failed to remove orphaned worktree dir after workspace root disappeared: {err:#}"
        );
        errors.push(err);
    }
}

pub(super) async fn remove_standalone_managed_worktree(
    task_id: TaskId,
    worktree: &Worktree,
    root: &Path,
    branch_cleanup: &mut WorktreeBranchCleanup,
    errors: &mut Vec<anyhow::Error>,
) {
    branch_cleanup.mark_needs_prune();
    if let Err(err) = tokio::fs::remove_dir_all(root)
        .await
        .with_context(|| format!("removing standalone managed worktree at {}", root.display()))
    {
        tracing::warn!(
            task_id = %task_id.0,
            worktree_id = %worktree.id.0,
            "failed to remove standalone managed worktree dir: {err:#}"
        );
        errors.push(err);
    }
}

pub(super) async fn remove_git_worktree(
    workspace: &Workspace,
    task_id: TaskId,
    worktree: &Worktree,
    root: &Path,
    branch_cleanup: &mut WorktreeBranchCleanup,
    errors: &mut Vec<anyhow::Error>,
) -> bool {
    branch_cleanup.mark_needs_prune();
    if let Err(err) = remove_worktree(&workspace.root_path, root).await {
        tracing::warn!(
            task_id = %task_id.0,
            worktree_id = %worktree.id.0,
            "failed to remove worktree: {err:#}"
        );
        errors.push(err);
        return false;
    }
    if tokio::fs::metadata(root).await.is_ok() {
        if let Err(err) = tokio::fs::remove_dir_all(root)
            .await
            .with_context(|| format!("removing worktree dir at {}", root.display()))
        {
            tracing::warn!(
                task_id = %task_id.0,
                worktree_id = %worktree.id.0,
                "failed to remove worktree dir: {err:#}"
            );
            errors.push(err);
        }
    }
    true
}

pub(super) async fn remove_non_git_worktree_dir(
    task_id: TaskId,
    worktree: &Worktree,
    root: &Path,
    errors: &mut Vec<anyhow::Error>,
) {
    if let Err(err) = tokio::fs::remove_dir_all(root)
        .await
        .with_context(|| format!("removing non-git worktree dir at {}", root.display()))
    {
        tracing::warn!(
            task_id = %task_id.0,
            worktree_id = %worktree.id.0,
            "failed to remove worktree dir: {err:#}"
        );
        errors.push(err);
    }
}
