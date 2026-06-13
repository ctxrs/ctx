use anyhow::Result;
use ctx_core::models::{Worktree, WorktreeVcsBaseResolutionKind};
use ctx_worktree_vcs_service::{
    plan_worktree_vcs_touched_files_refresh, resolve_worktree_diff_base_from_source,
    worktree_vcs_projection_cache_state, WorktreeDiffBaseResolution, WorktreeVcsDiffBaseQuery,
};

use self::status::{load_status_projection, StatusProjectionOutcome};
use self::touched::{refresh_touched_files_projection, TouchedFilesRefreshOutcome};
use super::super::snapshot::{
    build_worktree_vcs_snapshot_from_parts, publish_no_repo_snapshot, publish_unavailable_snapshot,
};
use super::super::source::HttpWorktreeVcsSource;
use super::super::worktree_has_vcs_repo;
use super::super::{WorktreeVcsExecutionHost, WorktreeVcsRuntimeHost};
use super::publish::publish_worktree_vcs_snapshot;

mod status;
mod summary;
mod touched;

pub(in crate::daemon::git_status) async fn refresh_worktree_vcs_projection(
    runtime: &WorktreeVcsRuntimeHost,
    execution: &WorktreeVcsExecutionHost,
    worktree: &Worktree,
    refresh_summary: bool,
    refresh_touched_files: bool,
    force_emit: bool,
) -> Result<()> {
    if !runtime.enabled() {
        return Ok(());
    }
    let is_active = runtime.is_worktree_vcs_active(worktree.id).await;
    if !is_active {
        return Ok(());
    }
    let refresh_lock = runtime.worktree_vcs_refresh_lock(worktree.id).await;
    let _refresh_guard = refresh_lock.lock().await;

    let cached_snapshot = runtime
        .get_worktree_vcs_snapshot(execution, worktree.id)
        .await;
    let cached = worktree_vcs_projection_cache_state(cached_snapshot.as_ref());

    if !worktree_has_vcs_repo(execution, worktree).await? {
        return publish_no_repo_snapshot(
            runtime,
            execution,
            worktree,
            WorktreeDiffBaseResolution {
                base_commit_sha: worktree.base_commit_sha.clone(),
                head_commit_sha: None,
                target_branch_commit_sha: None,
                target_branch: None,
                target_source: None,
                kind: WorktreeVcsBaseResolutionKind::WorktreeBase,
                error: Some("worktree is not a vcs repository".to_string()),
                unavailable_reason: Some(ctx_core::models::DiffUnavailableReason::NoRepo),
                explicit_target: false,
            },
            force_emit,
        )
        .await;
    }

    let source = HttpWorktreeVcsSource::new(execution, worktree);
    let resolution = resolve_worktree_diff_base_from_source(
        &source,
        worktree,
        WorktreeVcsDiffBaseQuery::default(),
    )
    .await;
    if let Some(reason) = resolution.unavailable_reason.clone() {
        return publish_unavailable_snapshot(
            runtime, execution, worktree, resolution, force_emit, reason,
        )
        .await;
    }

    let (summary_result, summary_at) =
        summary::refresh_summary(&source, worktree, &resolution, &cached, refresh_summary).await;

    let touched_plan =
        plan_worktree_vcs_touched_files_refresh(&summary_result.summary, refresh_touched_files);
    let include_status_inventory = touched_plan.include_status_inventory();
    let status_projection =
        match load_status_projection(execution, worktree, include_status_inventory).await? {
            StatusProjectionOutcome::Ready(status_projection) => *status_projection,
            StatusProjectionOutcome::NoRepo => {
                return publish_no_repo_snapshot(
                    runtime, execution, worktree, resolution, force_emit,
                )
                .await;
            }
        };

    let touched_result = match refresh_touched_files_projection(
        &source,
        worktree,
        &resolution,
        &cached,
        touched_plan,
    )
    .await
    {
        TouchedFilesRefreshOutcome::Ready(result) => result,
        TouchedFilesRefreshOutcome::NoRepo => {
            return publish_no_repo_snapshot(runtime, execution, worktree, resolution, force_emit)
                .await;
        }
    };

    let snapshot = build_worktree_vcs_snapshot_from_parts(
        execution,
        worktree,
        status_projection.git_status,
        touched_result.touched_files.clone(),
        touched_result.touched_files_state.clone(),
        summary_result.summary.clone(),
        summary_result.compute_state.clone(),
        Some(resolution),
        summary_result.available,
        summary_result.unavailable_reason,
    )
    .await?;

    publish_worktree_vcs_snapshot(
        runtime, execution, worktree, snapshot, force_emit, summary_at,
    )
    .await;

    runtime
        .finish_worktree_vcs_refresh(
            worktree.id,
            status_projection.git_snapshot,
            touched_result.touched_files,
            touched_result.touched_files_state,
        )
        .await;
    Ok(())
}
