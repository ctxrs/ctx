use std::time::Duration;

use anyhow::Result;
use notify::{RecursiveMode, Watcher};

use ctx_core::models::Worktree;
use ctx_worktree_vcs_service::{
    resolve_worktree_vcs_metadata_roots, WorktreeVcsGitCommand, WORKTREE_VCS_WATCH_DEBOUNCE_MS,
};

use ctx_settings_model::ExecutionMode;
use ctx_worktree_data_plane::resolve_worktree_data_plane_with_host as resolve_worktree_data_plane;

use super::sandbox::container_git_stdout;
use super::{vcs_driver_for_worktree, WorktreeVcsExecutionHost, WorktreeVcsRuntimeHost};

mod debounce;
mod poller;

use debounce::build_git_status_watcher;
use poller::run_git_status_poller;

pub(super) async fn run_git_status_watcher(
    runtime: WorktreeVcsRuntimeHost,
    execution: WorktreeVcsExecutionHost,
    worktree: Worktree,
) -> Result<()> {
    let data_plane = resolve_worktree_data_plane(&execution, &worktree).await?;
    let root = data_plane.live_worktree_root.as_path();
    if matches!(data_plane.execution_mode, ExecutionMode::Sandbox) {
        // Disk-isolated worktrees live inside the harness container; host filesystem watchers
        // cannot observe changes. Polling keeps VCS snapshots up to date.
        let _ = container_git_stdout(
            &execution,
            &worktree,
            WorktreeVcsGitCommand::IsInsideWorkTree,
        )
        .await?;
        return run_git_status_poller(runtime, execution, worktree).await;
    }
    let vcs = vcs_driver_for_worktree(&worktree);
    vcs.assert_repo(root).await?;
    let metadata_roots = resolve_worktree_vcs_metadata_roots(&worktree, root).await?;

    let mut watcher = build_git_status_watcher(
        runtime.clone(),
        execution.clone(),
        worktree.clone(),
        root.to_path_buf(),
        metadata_roots.clone(),
        Duration::from_millis(WORKTREE_VCS_WATCH_DEBOUNCE_MS),
    )?;
    if let Err(err) = watcher.watch(root, RecursiveMode::Recursive) {
        // On hosts with low watch limits (or many concurrent watchers), file watching can fail with
        // ENOSPC/too-many-watches. Falling back to polling keeps git status updates flowing and
        // avoids flaking tests that rely on live status changes.
        tracing::warn!(
            worktree_id = %worktree.id.0,
            "git status watcher unavailable; falling back to polling: {err:#}"
        );
        return run_git_status_poller(runtime, execution, worktree).await;
    }
    for metadata_root in metadata_roots
        .iter()
        .filter(|metadata_root| !metadata_root.starts_with(root))
    {
        if let Err(err) = watcher.watch(metadata_root, RecursiveMode::Recursive) {
            tracing::warn!(
                worktree_id = %worktree.id.0,
                metadata_root = %metadata_root.display(),
                "vcs metadata watcher unavailable; falling back to polling: {err:#}"
            );
            return run_git_status_poller(runtime, execution, worktree).await;
        }
    }
    std::future::pending::<Result<()>>().await
}
