use std::sync::Arc;

use super::super::buffer::{VcsPendingBuffer, VcsSnapshotKey};
use super::super::metrics::VcsStreamMetrics;
use ctx_core::ids::WorkspaceId;
use ctx_core::models::{WorktreeVcsSnapshot, WorktreeVcsStreamMessage, WorktreeVcsStreamTier};
use ctx_daemon::daemon::WorkspaceVcsStreamHandle;
use ctx_workspace_stream_service::vcs::WorkspaceVcsLagReseedPlan;

pub(in crate::api::ws::workspace_vcs) async fn seed_current_vcs_snapshots(
    state: &WorkspaceVcsStreamHandle,
    workspace_id: WorkspaceId,
    pending: &Arc<VcsPendingBuffer>,
    metrics: &Arc<VcsStreamMetrics>,
    demand_generation: i64,
    plan: &WorkspaceVcsLagReseedPlan,
) {
    for seed in &plan.seeds {
        let Some(snapshot) = state.get_worktree_vcs_snapshot(seed.worktree_id).await else {
            continue;
        };
        queue_vcs_snapshot(
            pending,
            metrics,
            workspace_id,
            demand_generation,
            seed.tier,
            snapshot,
        )
        .await;
    }
}

pub(in crate::api::ws::workspace_vcs) async fn queue_vcs_snapshot(
    pending: &Arc<VcsPendingBuffer>,
    metrics: &Arc<VcsStreamMetrics>,
    workspace_id: WorkspaceId,
    demand_generation: i64,
    tier: WorktreeVcsStreamTier,
    snapshot: WorktreeVcsSnapshot,
) {
    let worktree_id = snapshot.worktree_id;
    let message = if snapshot.available {
        match tier {
            WorktreeVcsStreamTier::Summary => WorktreeVcsStreamMessage::SummarySnapshot {
                workspace_id,
                worktree_id,
                demand_generation,
                snapshot,
            },
            WorktreeVcsStreamTier::Details => WorktreeVcsStreamMessage::DetailsSnapshot {
                workspace_id,
                worktree_id,
                demand_generation,
                snapshot,
            },
        }
    } else {
        WorktreeVcsStreamMessage::UnavailableSnapshot {
            workspace_id,
            worktree_id,
            demand_generation,
            snapshot,
        }
    };
    let coalesced = pending
        .push_snapshot(VcsSnapshotKey { worktree_id, tier }, message)
        .await;
    metrics.snapshot_queued(coalesced);
}
