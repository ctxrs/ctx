use ctx_core::models::Worktree;
use ctx_worktree_vcs_service::{
    is_no_vcs_repo_error, load_diff_touched_entries_from_source,
    worktree_vcs_touched_files_error_fallback, worktree_vcs_touched_files_from_entries,
    worktree_vcs_touched_files_large_change_set, worktree_vcs_touched_files_reuse,
    WorktreeDiffBaseResolution, WorktreeVcsProjectionCacheState,
    WorktreeVcsTouchedFilesRefreshPlan, WorktreeVcsTouchedFilesRefreshResult,
};

use crate::daemon::git_status::source::HttpWorktreeVcsSource;

pub(super) enum TouchedFilesRefreshOutcome {
    Ready(WorktreeVcsTouchedFilesRefreshResult),
    NoRepo,
}

pub(super) async fn refresh_touched_files_projection(
    source: &HttpWorktreeVcsSource<'_>,
    worktree: &Worktree,
    resolution: &WorktreeDiffBaseResolution,
    cached: &WorktreeVcsProjectionCacheState,
    touched_plan: WorktreeVcsTouchedFilesRefreshPlan,
) -> TouchedFilesRefreshOutcome {
    match touched_plan {
        WorktreeVcsTouchedFilesRefreshPlan::LargeChangeSet { file_count } => {
            TouchedFilesRefreshOutcome::Ready(worktree_vcs_touched_files_large_change_set(
                file_count,
            ))
        }
        WorktreeVcsTouchedFilesRefreshPlan::LoadDiff => {
            match load_diff_touched_entries_from_source(source, &resolution.base_commit_sha).await {
                Ok(entries) => TouchedFilesRefreshOutcome::Ready(
                    worktree_vcs_touched_files_from_entries(&entries),
                ),
                Err(err) if is_no_vcs_repo_error(&err) => TouchedFilesRefreshOutcome::NoRepo,
                Err(err) => {
                    tracing::warn!(
                        worktree_id = %worktree.id.0,
                        "worktree touched-file refresh failed: {err:#}"
                    );
                    TouchedFilesRefreshOutcome::Ready(worktree_vcs_touched_files_error_fallback(
                        cached,
                    ))
                }
            }
        }
        WorktreeVcsTouchedFilesRefreshPlan::Reuse => {
            TouchedFilesRefreshOutcome::Ready(worktree_vcs_touched_files_reuse(cached))
        }
    }
}
