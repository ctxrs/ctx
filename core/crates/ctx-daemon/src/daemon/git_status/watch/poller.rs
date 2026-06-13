use std::time::Duration;

use anyhow::Result;
use ctx_core::models::Worktree;
use ctx_worktree_vcs_service::{WorktreeVcsDirtyBits, WORKTREE_VCS_POLL_INTERVAL_MS};

use super::super::{mark_worktree_vcs_dirty, WorktreeVcsExecutionHost, WorktreeVcsRuntimeHost};

pub(super) async fn run_git_status_poller(
    runtime: WorktreeVcsRuntimeHost,
    execution: WorktreeVcsExecutionHost,
    worktree: Worktree,
) -> Result<()> {
    let mut interval = tokio::time::interval(Duration::from_millis(WORKTREE_VCS_POLL_INTERVAL_MS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        if let Err(err) = mark_worktree_vcs_dirty(
            &runtime,
            &execution,
            &worktree,
            WorktreeVcsDirtyBits {
                worktree_fs: true,
                vcs_meta: true,
            },
            Vec::new(),
        )
        .await
        {
            tracing::warn!(worktree_id = %worktree.id.0, "git status invalidation failed: {err:#}");
        }
    }
}
