use anyhow::Result;
use ctx_core::models::{Worktree, WorktreeVcsGitStatusSummary};
use ctx_worktree_vcs_service::{
    build_git_status_entries, build_git_status_summary, is_no_vcs_repo_error, GitStatusSnapshot,
};

use super::super::loading::load_git_status_snapshot;
use crate::daemon::git_status::WorktreeVcsExecutionHost;

pub(super) enum StatusProjectionOutcome {
    Ready(Box<StatusProjection>),
    NoRepo,
}

pub(super) struct StatusProjection {
    pub(super) git_snapshot: GitStatusSnapshot,
    pub(super) git_status: WorktreeVcsGitStatusSummary,
}

pub(super) async fn load_status_projection(
    execution: &WorktreeVcsExecutionHost,
    worktree: &Worktree,
    include_status_inventory: bool,
) -> Result<StatusProjectionOutcome> {
    let git_snapshot = match load_git_status_snapshot(
        execution,
        worktree,
        include_status_inventory,
        include_status_inventory,
    )
    .await
    {
        Ok(snapshot) => snapshot,
        Err(err) if is_no_vcs_repo_error(&err) => return Ok(StatusProjectionOutcome::NoRepo),
        Err(err) => return Err(err),
    };
    let entries = if include_status_inventory {
        build_git_status_entries(&git_snapshot.entries)
    } else {
        Vec::new()
    };
    let git_status = build_git_status_summary(&git_snapshot, entries);

    Ok(StatusProjectionOutcome::Ready(Box::new(StatusProjection {
        git_snapshot,
        git_status,
    })))
}
