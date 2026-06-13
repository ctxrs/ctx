use anyhow::Result;
use ctx_core::models::Worktree;
use ctx_worktree_vcs_service::{load_git_status_snapshot_from_source, GitStatusSnapshot};

use super::super::source::HttpWorktreeVcsSource;
use super::super::WorktreeVcsExecutionHost;

pub(in crate::daemon) async fn load_git_status_snapshot(
    execution: &WorktreeVcsExecutionHost,
    worktree: &Worktree,
    include_untracked_files: bool,
    include_entries: bool,
) -> Result<GitStatusSnapshot> {
    let source = HttpWorktreeVcsSource::new(execution, worktree);
    load_git_status_snapshot_from_source(&source, include_untracked_files, include_entries).await
}
