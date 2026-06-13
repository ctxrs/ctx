use super::*;

pub(super) async fn cleanup_orphaned_provisioned_worktree(
    handles: &TaskSessionHandles,
    store: &Store,
    workspace: &Workspace,
    task_id: TaskId,
    worktree_id: WorktreeId,
) {
    let worktree = match store.get_worktree(worktree_id).await {
        Ok(Some(worktree)) => worktree,
        Ok(None) => return,
        Err(err) => {
            tracing::warn!(
                task_id = %task_id.0,
                worktree_id = %worktree_id.0,
                "failed to load provisioned worktree for rollback cleanup: {err:#}"
            );
            return;
        }
    };
    let sandbox_binding = match store.get_sandbox_binding(worktree_id).await {
        Ok(binding) => binding,
        Err(err) => {
            tracing::warn!(
                task_id = %task_id.0,
                worktree_id = %worktree_id.0,
                "failed to load sandbox binding for provisioned worktree rollback: {err:#}"
            );
            None
        }
    };
    let cleanup_targets = [TaskWorktreeCleanupTarget {
        managed_root: handles
            .admission
            .managed_worktree_root(workspace, &worktree),
        sandbox_binding,
        worktree: worktree.clone(),
        destroy_worktree_on_cleanup: true,
    }];
    let cleanup_errors = handles
        .admission
        .cleanup_task_worktrees(
            workspace,
            task_id,
            &cleanup_targets,
            BranchCleanupErrorMode::Report,
        )
        .await;
    if !cleanup_errors.is_empty() {
        tracing::warn!(
            task_id = %task_id.0,
            worktree_id = %worktree_id.0,
            cleanup_errors = cleanup_errors.len(),
            "provisioned worktree rollback had cleanup errors"
        );
        return;
    }
    let deleted_worktree_row = match store.delete_worktree(worktree_id).await {
        Ok(deleted) => deleted,
        Err(err) => {
            tracing::warn!(
                task_id = %task_id.0,
                worktree_id = %worktree_id.0,
                "failed to delete provisioned worktree row during rollback: {err:#}"
            );
            false
        }
    };
    if !deleted_worktree_row {
        tracing::warn!(
            task_id = %task_id.0,
            worktree_id = %worktree_id.0,
            "skipping worktree index deletion because provisioned worktree row was not deleted"
        );
        return;
    }
    if let Err(err) = handles
        .admission
        .delete_workspace_worktree_index(worktree_id)
        .await
    {
        tracing::warn!(
            task_id = %task_id.0,
            worktree_id = %worktree_id.0,
            "failed to delete provisioned worktree index during rollback: {err:#}"
        );
    }
}
