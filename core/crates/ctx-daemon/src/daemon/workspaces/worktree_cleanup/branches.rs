use ctx_core::ids::TaskId;
use ctx_worktree_vcs_service::{delete_worktree_branch, prune_worktrees};

use super::BranchCleanupErrorMode;

#[derive(Default)]
pub(super) struct WorktreeBranchCleanup {
    needs_prune: bool,
    branches_to_delete: Vec<String>,
}

impl WorktreeBranchCleanup {
    pub(super) fn mark_needs_prune(&mut self) {
        self.needs_prune = true;
    }
}

pub(super) fn collect_worktree_branch_for_cleanup(
    cleanup: &mut WorktreeBranchCleanup,
    branch: &str,
) {
    cleanup.branches_to_delete.push(branch.to_string());
}

pub(super) async fn cleanup_collected_worktree_branches(
    workspace_root: &str,
    task_id: TaskId,
    mut cleanup: WorktreeBranchCleanup,
    branch_cleanup_error_mode: BranchCleanupErrorMode,
    errors: &mut Vec<anyhow::Error>,
) {
    if cleanup.needs_prune {
        if let Err(err) = prune_worktrees(workspace_root).await {
            tracing::warn!(task_id = %task_id.0, "failed to prune worktrees: {err:#}");
            errors.push(err);
        }
    }
    cleanup.branches_to_delete.sort();
    cleanup.branches_to_delete.dedup();
    for branch in cleanup.branches_to_delete {
        if let Err(err) = delete_worktree_branch(workspace_root, &branch).await {
            tracing::warn!(
                task_id = %task_id.0,
                branch,
                "failed to delete worktree branch: {err:#}"
            );
            if matches!(branch_cleanup_error_mode, BranchCleanupErrorMode::Report) {
                errors.push(err);
            }
        }
    }
}
