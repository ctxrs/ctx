use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::Arc;

use super::subscription::queue_vcs_snapshot;
use super::*;

fn test_snapshot(worktree_id: WorktreeId, rev: i64) -> WorktreeVcsSnapshot {
    WorktreeVcsSnapshot {
        worktree_id,
        rev,
        emitted_at_ms: rev,
        base_commit_sha: "base".to_string(),
        head_commit_sha: "head".to_string(),
        target_branch: Some("origin/main".to_string()),
        target_branch_commit_sha: Some("target".to_string()),
        base_resolution: WorktreeVcsBaseResolution::default(),
        compute_state: WorktreeVcsComputeState::Ready,
        summary: WorktreeVcsSummary {
            file_count: Some(rev),
            line_additions: Some(rev),
            line_deletions: Some(0),
            line_count: Some(rev),
        },
        git_status: WorktreeVcsGitStatusSummary::default(),
        touched_files: WorktreeVcsTouchedFiles::default(),
        touched_files_state: WorktreeVcsTouchedFilesState::Ready,
        freshness: WorktreeVcsFreshness::Fresh,
        available: true,
        unavailable_reason: None,
        schema_version: 1,
    }
}

#[tokio::test]
async fn pending_buffer_coalesces_ctx_ui_sized_vcs_storm_latest_wins() {
    let pending = Arc::new(VcsPendingBuffer::new());
    let metrics = Arc::new(VcsStreamMetrics::default());
    let workspace_id = WorkspaceId::new();
    let worktree_id = WorktreeId::new();
    let ctx_ui_vcs_event_count = 10_804;

    for rev in 1..=ctx_ui_vcs_event_count {
        queue_vcs_snapshot(
            &pending,
            &metrics,
            workspace_id,
            7,
            WorktreeVcsStreamTier::Summary,
            test_snapshot(worktree_id, rev),
        )
        .await;
    }

    let Some(message) = pending.pop().await else {
        panic!("expected latest VCS snapshot");
    };
    match message {
        WorktreeVcsStreamMessage::SummarySnapshot { snapshot, .. } => {
            assert_eq!(snapshot.worktree_id, worktree_id);
            assert_eq!(snapshot.rev, ctx_ui_vcs_event_count);
        }
        other => panic!("expected summary snapshot, got {other:?}"),
    }
    assert!(pending.is_empty().await);
    assert_eq!(
        metrics.snapshot_queued_count.load(AtomicOrdering::Relaxed),
        ctx_ui_vcs_event_count as u64
    );
    assert_eq!(
        metrics
            .snapshot_coalesced_count
            .load(AtomicOrdering::Relaxed),
        (ctx_ui_vcs_event_count - 1) as u64
    );
}
