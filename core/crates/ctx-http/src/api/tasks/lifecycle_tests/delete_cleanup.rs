use super::*;
use ctx_daemon::test_support::standaloneize_worktree_git_dir;

#[path = "delete_cleanup/branch_reclaim.rs"]
mod branch_reclaim;
#[path = "delete_cleanup/cleanup_errors.rs"]
mod cleanup_errors;
#[path = "delete_cleanup/standalone.rs"]
mod standalone;
#[path = "delete_cleanup/unused_worktree.rs"]
mod unused_worktree;
