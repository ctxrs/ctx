use std::path::{Path as StdPath, PathBuf};

use anyhow::Context;
use ctx_core::ids::TaskId;
use ctx_core::models::{SandboxBinding, Workspace, Worktree};

pub(super) enum SandboxCleanupOutcome {
    Complete { errors: Vec<anyhow::Error> },
    SkipRemainingTarget { error: anyhow::Error },
}

pub(super) async fn cleanup_sandbox_materialization(
    data_root: &StdPath,
    workspace: &Workspace,
    worktree: &Worktree,
    binding: &SandboxBinding,
    task_id: TaskId,
) -> SandboxCleanupOutcome {
    let mut errors = Vec::new();
    let sandbox_mode = match ctx_harness_runtime::selected_sandbox_command_mode(data_root) {
        Ok(mode) => mode,
        Err(err) => {
            tracing::warn!(
                task_id = %task_id.0,
                worktree_id = %worktree.id.0,
                "failed to resolve sandbox command mode for cleanup: {err:#}"
            );
            return SandboxCleanupOutcome::SkipRemainingTarget { error: err };
        }
    };
    if let Err(err) = ctx_sandbox_materialization::remove_live_worktree_root(
        data_root,
        &sandbox_mode,
        workspace.id,
        StdPath::new(&binding.live_worktree_root),
    )
    .await
    {
        tracing::warn!(
            task_id = %task_id.0,
            worktree_id = %worktree.id.0,
            live_worktree_root = binding.live_worktree_root,
            "failed to remove sandbox live worktree root: {err:#}"
        );
        errors.push(err);
    }
    if let Some(host_materialization_root) = binding.host_materialization_root.as_deref() {
        if let Err(err) =
            cleanup_host_materialization_root(task_id, worktree, host_materialization_root).await
        {
            errors.push(err);
        }
    }
    SandboxCleanupOutcome::Complete { errors }
}

async fn cleanup_host_materialization_root(
    task_id: TaskId,
    worktree: &Worktree,
    host_materialization_root: &str,
) -> anyhow::Result<()> {
    let host_materialization_root = PathBuf::from(host_materialization_root);
    if tokio::fs::metadata(&host_materialization_root)
        .await
        .is_err()
    {
        return Ok(());
    }
    if let Err(err) = tokio::fs::remove_dir_all(&host_materialization_root)
        .await
        .with_context(|| {
            format!(
                "removing sandbox host materialization root at {}",
                host_materialization_root.display()
            )
        })
    {
        tracing::warn!(
            task_id = %task_id.0,
            worktree_id = %worktree.id.0,
            host_materialization_root = %host_materialization_root.display(),
            "failed to remove sandbox host materialization root: {err:#}"
        );
        return Err(err);
    }
    Ok(())
}
