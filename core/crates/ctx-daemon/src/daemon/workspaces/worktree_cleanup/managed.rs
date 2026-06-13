use std::path::Path;

use ctx_core::ids::TaskId;
use ctx_core::models::{Workspace, Worktree};
use ctx_worktree_vcs_service::is_git_worktree;

use super::branches::{collect_worktree_branch_for_cleanup, WorktreeBranchCleanup};
use removal::{
    remove_git_worktree, remove_non_git_worktree_dir, remove_orphaned_worktree_dir,
    remove_standalone_managed_worktree,
};

#[path = "managed/removal.rs"]
mod removal;

pub(super) async fn cleanup_managed_worktree_target(
    workspace: &Workspace,
    task_id: TaskId,
    worktree: &Worktree,
    root: &Path,
    workspace_root_exists: bool,
    branch_cleanup: &mut WorktreeBranchCleanup,
    errors: &mut Vec<anyhow::Error>,
) {
    let branch = worktree
        .git_branch
        .as_deref()
        .filter(|name| name.starts_with("ctx/"));
    if !workspace_root_exists {
        remove_orphaned_worktree_dir(workspace, task_id, worktree, root, errors).await;
        return;
    }
    if tokio::fs::metadata(root).await.is_err() {
        if branch.is_some() {
            branch_cleanup.mark_needs_prune();
        }
        collect_branch_if_present(branch_cleanup, branch);
        return;
    }
    let embedded_git_dir = tokio::fs::metadata(root.join(".git"))
        .await
        .map(|meta| meta.is_dir())
        .unwrap_or(false);
    let is_git = embedded_git_dir || is_git_worktree(root).await.unwrap_or(false);
    let should_collect_branch = if embedded_git_dir {
        remove_standalone_managed_worktree(task_id, worktree, root, branch_cleanup, errors).await;
        true
    } else if is_git {
        remove_git_worktree(workspace, task_id, worktree, root, branch_cleanup, errors).await
    } else {
        remove_non_git_worktree_dir(task_id, worktree, root, errors).await;
        true
    };
    if should_collect_branch {
        collect_branch_if_present(branch_cleanup, branch);
    }
}

fn collect_branch_if_present(branch_cleanup: &mut WorktreeBranchCleanup, branch: Option<&str>) {
    if let Some(branch) = branch {
        collect_worktree_branch_for_cleanup(branch_cleanup, branch);
    }
}
