use std::time::Instant;

use ctx_core::models::Worktree;
use ctx_worktree_vcs_service::{
    is_no_vcs_repo_error, load_diff_file_count_from_source, plan_worktree_vcs_summary_refresh,
    worktree_vcs_summary_refresh_error_fallback, worktree_vcs_summary_refresh_from_file_count,
    worktree_vcs_summary_refresh_no_repo, WorktreeDiffBaseResolution,
    WorktreeVcsProjectionCacheState, WorktreeVcsSummaryRefreshPlan,
    WorktreeVcsSummaryRefreshResult,
};

use crate::daemon::git_status::source::HttpWorktreeVcsSource;

pub(super) async fn refresh_summary(
    source: &HttpWorktreeVcsSource<'_>,
    worktree: &Worktree,
    resolution: &WorktreeDiffBaseResolution,
    cached: &WorktreeVcsProjectionCacheState,
    refresh_summary: bool,
) -> (WorktreeVcsSummaryRefreshResult, Option<Instant>) {
    match plan_worktree_vcs_summary_refresh(cached, refresh_summary) {
        WorktreeVcsSummaryRefreshPlan::LoadFileCount => {
            match load_diff_file_count_from_source(source, &resolution.base_commit_sha).await {
                Ok(file_count) => (
                    worktree_vcs_summary_refresh_from_file_count(file_count),
                    Some(Instant::now()),
                ),
                Err(err) if is_no_vcs_repo_error(&err) => {
                    (worktree_vcs_summary_refresh_no_repo(), None)
                }
                Err(err) => {
                    tracing::warn!(
                        worktree_id = %worktree.id.0,
                        "worktree diff file-count refresh failed: {err:#}"
                    );
                    (worktree_vcs_summary_refresh_error_fallback(cached), None)
                }
            }
        }
        WorktreeVcsSummaryRefreshPlan::Reuse(result) => (result, None),
    }
}
