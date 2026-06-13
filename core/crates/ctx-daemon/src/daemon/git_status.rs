use std::sync::Arc;

use anyhow::Result;

use ctx_core::models::Worktree;

mod host;
mod projection;
mod sandbox;
mod scheduler;
mod snapshot;
mod source;
mod watch;
use ctx_worktree_vcs_service::{
    worktree_has_vcs_repo_from_source, worktree_vcs_dirty_transient_snapshot,
    worktree_vcs_driver_for_kind, worktree_vcs_refresh_transient_snapshot, WorktreeVcsDirtyBits,
    WorktreeVcsDriver,
};
pub(in crate::daemon) use host::{
    WorktreeVcsExecutionHost, WorktreeVcsRuntimeHost, WorktreeVcsSandboxTarget,
};
pub(in crate::daemon) use projection::load_git_status_snapshot;
use projection::{publish_transient_worktree_vcs_snapshot, refresh_worktree_vcs_projection};
use scheduler::ensure_worktree_vcs_scheduler_started;
pub(in crate::daemon) use source::HttpWorktreeVcsSource;

fn vcs_driver_for_worktree(worktree: &Worktree) -> Arc<WorktreeVcsDriver> {
    worktree_vcs_driver_for_kind(worktree.vcs_kind.clone())
}

pub(in crate::daemon) async fn worktree_has_vcs_repo(
    execution: &WorktreeVcsExecutionHost,
    worktree: &Worktree,
) -> Result<bool> {
    let source = source::HttpWorktreeVcsSource::new(execution, worktree);
    worktree_has_vcs_repo_from_source(&source).await
}

pub(in crate::daemon) async fn request_worktree_vcs_refresh(
    runtime: &WorktreeVcsRuntimeHost,
    execution: &WorktreeVcsExecutionHost,
    worktree: &Worktree,
    summary: bool,
    touched_files: bool,
) -> Result<()> {
    request_worktree_vcs_refresh_inner(runtime, execution, worktree, summary, touched_files, true)
        .await
}

pub(in crate::daemon) async fn request_worktree_vcs_refresh_without_transient(
    runtime: &WorktreeVcsRuntimeHost,
    execution: &WorktreeVcsExecutionHost,
    worktree: &Worktree,
    summary: bool,
    touched_files: bool,
) -> Result<()> {
    request_worktree_vcs_refresh_inner(runtime, execution, worktree, summary, touched_files, false)
        .await
}

async fn request_worktree_vcs_refresh_inner(
    runtime: &WorktreeVcsRuntimeHost,
    execution: &WorktreeVcsExecutionHost,
    worktree: &Worktree,
    summary: bool,
    touched_files: bool,
    publish_transient: bool,
) -> Result<()> {
    if !runtime.enabled() {
        return Ok(());
    }
    if !runtime.is_worktree_vcs_active(worktree.id).await {
        return Ok(());
    }
    ensure_worktree_vcs_scheduler_started(runtime.clone(), execution.clone()).await;

    runtime
        .queue_worktree_vcs_refresh(worktree.id, summary, touched_files)
        .await;

    if publish_transient {
        if let Some(snapshot) = runtime
            .get_worktree_vcs_snapshot(execution, worktree.id)
            .await
        {
            let snapshot =
                worktree_vcs_refresh_transient_snapshot(snapshot, summary, touched_files);
            publish_transient_worktree_vcs_snapshot(runtime, worktree, snapshot).await;
        }
    }

    runtime.notify_worktree_vcs_scheduler();
    Ok(())
}

pub(in crate::daemon) async fn mark_worktree_vcs_dirty(
    runtime: &WorktreeVcsRuntimeHost,
    execution: &WorktreeVcsExecutionHost,
    worktree: &Worktree,
    dirty_bits: WorktreeVcsDirtyBits,
    candidate_paths: Vec<String>,
) -> Result<()> {
    if !runtime.enabled() {
        return Ok(());
    }
    if !runtime.is_worktree_vcs_active(worktree.id).await {
        return Ok(());
    }
    let pane_open = runtime.is_worktree_vcs_pane_open(worktree.id).await;
    runtime
        .mark_worktree_vcs_dirty(worktree.id, dirty_bits, candidate_paths, pane_open)
        .await;

    if let Some(snapshot) = runtime
        .get_worktree_vcs_snapshot(execution, worktree.id)
        .await
    {
        let snapshot = worktree_vcs_dirty_transient_snapshot(snapshot);
        publish_transient_worktree_vcs_snapshot(runtime, worktree, snapshot).await;
    }

    request_worktree_vcs_refresh(runtime, execution, worktree, true, pane_open).await
}

#[cfg(any(test, feature = "test-support"))]
pub(in crate::daemon) async fn refresh_worktree_vcs_summary(
    runtime: WorktreeVcsRuntimeHost,
    execution: WorktreeVcsExecutionHost,
    worktree: Worktree,
) -> Result<()> {
    if !runtime.enabled() {
        return Ok(());
    }
    refresh_worktree_vcs_projection(&runtime, &execution, &worktree, true, false, false).await
}

pub(in crate::daemon) async fn emit_worktree_vcs_snapshot_for_worktree(
    runtime: &WorktreeVcsRuntimeHost,
    execution: &WorktreeVcsExecutionHost,
    worktree: &Worktree,
    force_emit: bool,
) -> Result<()> {
    if !runtime.enabled() {
        return Ok(());
    }
    let refresh_touched_files = runtime.is_worktree_vcs_pane_open(worktree.id).await;
    refresh_worktree_vcs_projection(
        runtime,
        execution,
        worktree,
        true,
        refresh_touched_files,
        force_emit,
    )
    .await
}

pub(in crate::daemon) async fn run_git_status_watcher(
    runtime: WorktreeVcsRuntimeHost,
    execution: WorktreeVcsExecutionHost,
    worktree: Worktree,
) -> Result<()> {
    if !runtime.enabled() {
        return Ok(());
    }
    watch::run_git_status_watcher(runtime, execution, worktree).await
}
