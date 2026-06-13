use super::super::buffer::VcsPendingBuffer;
use super::super::metrics::VcsStreamMetrics;
use super::runtime::WorkspaceVcsRuntime;
use super::snapshots::seed_current_vcs_snapshots;
use ctx_core::ids::{WorkspaceId, WorktreeId};
use ctx_core::models::{WorktreeVcsStreamClientMessage, WorktreeVcsStreamMessage};
use ctx_daemon::daemon::WorkspaceVcsStreamHandle;
use std::sync::Arc;

pub(in crate::api::ws::workspace_vcs) async fn handle_workspace_vcs_client_message(
    state: &WorkspaceVcsStreamHandle,
    workspace_id: WorkspaceId,
    pending: &Arc<VcsPendingBuffer>,
    metrics: &Arc<VcsStreamMetrics>,
    runtime: &mut WorkspaceVcsRuntime,
    message: WorktreeVcsStreamClientMessage,
) {
    match message {
        WorktreeVcsStreamClientMessage::ReplaceSubscription {
            summary_worktree_ids,
            detail_worktree_ids,
        } => {
            replace_workspace_vcs_subscription(
                state,
                workspace_id,
                pending,
                metrics,
                runtime,
                summary_worktree_ids,
                detail_worktree_ids,
            )
            .await;
        }
        WorktreeVcsStreamClientMessage::Refresh { worktree_ids, tier } => {
            let plan = state
                .plan_workspace_vcs_refresh(workspace_id, worktree_ids, tier)
                .await;
            state
                .refresh_worktree_vcs_for_worktrees(
                    &plan.summary_refresh_worktree_ids,
                    &plan.detail_refresh_worktree_ids,
                )
                .await;
        }
    }
}

async fn replace_workspace_vcs_subscription(
    state: &WorkspaceVcsStreamHandle,
    workspace_id: WorkspaceId,
    pending: &Arc<VcsPendingBuffer>,
    metrics: &Arc<VcsStreamMetrics>,
    runtime: &mut WorkspaceVcsRuntime,
    summary_worktree_ids: Vec<WorktreeId>,
    detail_worktree_ids: Vec<WorktreeId>,
) {
    let plan = state
        .plan_workspace_vcs_subscription_update(
            workspace_id,
            runtime.clone(),
            summary_worktree_ids,
            detail_worktree_ids,
        )
        .await;
    *runtime = plan.state;

    state
        .ensure_worktree_vcs_watchers_for_worktrees(
            &plan.summary_subscribed_worktree_ids,
            &plan.detail_subscribed_worktree_ids,
        )
        .await;

    pending
        .push_control(WorktreeVcsStreamMessage::Subscribed {
            workspace_id,
            demand_generation: runtime.demand_generation,
            summary_worktree_ids: plan.summary_subscribed_worktree_ids.clone(),
            detail_worktree_ids: plan.detail_subscribed_worktree_ids.clone(),
        })
        .await;
    seed_current_vcs_snapshots(
        state,
        workspace_id,
        pending,
        metrics,
        runtime.demand_generation,
        &plan.seed_plan,
    )
    .await;
    state
        .refresh_worktree_vcs_for_worktrees(
            &plan.summary_refresh_worktree_ids,
            &plan.detail_refresh_worktree_ids,
        )
        .await;
}
