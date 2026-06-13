use ctx_core::models::Worktree;

use super::WorktreeVcsExecutionHost;

mod commit_lookup;
mod diff_base;
mod diff_path;
mod status;

pub(in crate::daemon) struct HttpWorktreeVcsSource<'a> {
    execution: &'a WorktreeVcsExecutionHost,
    worktree: &'a Worktree,
}

impl<'a> HttpWorktreeVcsSource<'a> {
    pub(in crate::daemon) fn new(
        execution: &'a WorktreeVcsExecutionHost,
        worktree: &'a Worktree,
    ) -> Self {
        Self {
            execution,
            worktree,
        }
    }
}
