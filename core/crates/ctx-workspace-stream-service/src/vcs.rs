use std::collections::HashSet;

use ctx_core::ids::WorktreeId;
use ctx_core::models::WorktreeVcsStreamTier;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceVcsDemandState {
    pub demand_generation: i64,
    pub summary_worktree_ids: HashSet<WorktreeId>,
    pub detail_worktree_ids: HashSet<WorktreeId>,
}

impl WorkspaceVcsDemandState {
    pub fn active_worktree_ids(&self) -> HashSet<WorktreeId> {
        self.summary_worktree_ids
            .union(&self.detail_worktree_ids)
            .copied()
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceVcsSubscriptionPlan {
    pub state: WorkspaceVcsDemandState,
    pub summary_seed_worktree_ids: HashSet<WorktreeId>,
    pub detail_seed_worktree_ids: HashSet<WorktreeId>,
    pub seed_plan: WorkspaceVcsLagReseedPlan,
    pub summary_refresh_worktree_ids: Vec<WorktreeId>,
    pub detail_refresh_worktree_ids: Vec<WorktreeId>,
    pub summary_subscribed_worktree_ids: Vec<WorktreeId>,
    pub detail_subscribed_worktree_ids: Vec<WorktreeId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceVcsRefreshPlan {
    pub summary_refresh_worktree_ids: Vec<WorktreeId>,
    pub detail_refresh_worktree_ids: Vec<WorktreeId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceVcsSnapshotRoute {
    Drop,
    Summary,
    Details,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceVcsSnapshotSeed {
    pub worktree_id: WorktreeId,
    pub tier: WorktreeVcsStreamTier,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceVcsLagReseedPlan {
    pub seeds: Vec<WorkspaceVcsSnapshotSeed>,
}

pub fn plan_workspace_vcs_subscription_update(
    current: WorkspaceVcsDemandState,
    summary_worktree_ids: Vec<WorktreeId>,
    detail_worktree_ids: Vec<WorktreeId>,
) -> WorkspaceVcsSubscriptionPlan {
    let previous_active = current.active_worktree_ids();
    let previous_details = current.detail_worktree_ids.clone();
    let summary_worktree_ids = normalized_worktree_ids(summary_worktree_ids);
    let detail_worktree_ids = normalized_worktree_ids(detail_worktree_ids);
    let summary_set = summary_worktree_ids.iter().copied().collect::<HashSet<_>>();
    let detail_set = detail_worktree_ids.iter().copied().collect::<HashSet<_>>();
    let next = WorkspaceVcsDemandState {
        demand_generation: current.demand_generation + 1,
        summary_worktree_ids: summary_set,
        detail_worktree_ids: detail_set,
    };

    let summary_seed_worktree_ids = next
        .summary_worktree_ids
        .difference(&previous_active)
        .copied()
        .collect::<HashSet<_>>();
    let detail_seed_worktree_ids = next
        .detail_worktree_ids
        .difference(&previous_details)
        .copied()
        .collect::<HashSet<_>>();
    let summary_refresh_worktree_ids = sorted_worktree_ids(
        next.summary_worktree_ids
            .difference(&previous_active)
            .copied(),
    );
    let detail_refresh_worktree_ids = sorted_worktree_ids(
        next.detail_worktree_ids
            .difference(&previous_details)
            .copied(),
    );
    let seed_plan = workspace_vcs_seed_plan(&summary_seed_worktree_ids, &detail_seed_worktree_ids);

    WorkspaceVcsSubscriptionPlan {
        state: next,
        summary_seed_worktree_ids,
        detail_seed_worktree_ids,
        seed_plan,
        summary_refresh_worktree_ids,
        detail_refresh_worktree_ids,
        summary_subscribed_worktree_ids: summary_worktree_ids,
        detail_subscribed_worktree_ids: detail_worktree_ids,
    }
}

pub fn plan_workspace_vcs_refresh(
    worktree_ids: Vec<WorktreeId>,
    tier: WorktreeVcsStreamTier,
) -> WorkspaceVcsRefreshPlan {
    let worktree_ids = normalized_worktree_ids(worktree_ids);
    match tier {
        WorktreeVcsStreamTier::Summary => WorkspaceVcsRefreshPlan {
            summary_refresh_worktree_ids: worktree_ids,
            detail_refresh_worktree_ids: Vec::new(),
        },
        WorktreeVcsStreamTier::Details => WorkspaceVcsRefreshPlan {
            summary_refresh_worktree_ids: Vec::new(),
            detail_refresh_worktree_ids: worktree_ids,
        },
    }
}

pub fn route_workspace_vcs_snapshot(
    demand: &WorkspaceVcsDemandState,
    worktree_id: WorktreeId,
) -> WorkspaceVcsSnapshotRoute {
    if demand.detail_worktree_ids.contains(&worktree_id) {
        WorkspaceVcsSnapshotRoute::Details
    } else if demand.summary_worktree_ids.contains(&worktree_id) {
        WorkspaceVcsSnapshotRoute::Summary
    } else {
        WorkspaceVcsSnapshotRoute::Drop
    }
}

pub fn plan_workspace_vcs_lag_reseed(
    demand: &WorkspaceVcsDemandState,
) -> WorkspaceVcsLagReseedPlan {
    workspace_vcs_seed_plan(&demand.summary_worktree_ids, &demand.detail_worktree_ids)
}

fn workspace_vcs_seed_plan(
    summary_worktree_ids: &HashSet<WorktreeId>,
    detail_worktree_ids: &HashSet<WorktreeId>,
) -> WorkspaceVcsLagReseedPlan {
    let mut worktree_ids = summary_worktree_ids
        .union(detail_worktree_ids)
        .copied()
        .collect::<Vec<_>>();
    worktree_ids.sort_by_key(|worktree_id| worktree_id.0);
    let seeds = worktree_ids
        .into_iter()
        .map(|worktree_id| {
            let tier = if detail_worktree_ids.contains(&worktree_id) {
                WorktreeVcsStreamTier::Details
            } else {
                WorktreeVcsStreamTier::Summary
            };
            WorkspaceVcsSnapshotSeed { worktree_id, tier }
        })
        .collect();
    WorkspaceVcsLagReseedPlan { seeds }
}

fn normalized_worktree_ids(ids: Vec<WorktreeId>) -> Vec<WorktreeId> {
    sorted_worktree_ids(ids.into_iter().collect::<HashSet<_>>())
}

fn sorted_worktree_ids<I>(ids: I) -> Vec<WorktreeId>
where
    I: IntoIterator<Item = WorktreeId>,
{
    let mut ids = ids.into_iter().collect::<Vec<_>>();
    ids.sort_by_key(|worktree_id| worktree_id.0);
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_pairs(plan: &WorkspaceVcsLagReseedPlan) -> Vec<(WorktreeId, WorktreeVcsStreamTier)> {
        plan.seeds
            .iter()
            .map(|seed| (seed.worktree_id, seed.tier))
            .collect()
    }

    #[test]
    fn subscription_plan_dedupes_and_sorts_requested_worktrees() {
        let first = WorktreeId::new();
        let second = WorktreeId::new();
        let third = WorktreeId::new();

        let plan = plan_workspace_vcs_subscription_update(
            WorkspaceVcsDemandState::default(),
            vec![second, first, second],
            vec![third, first, third],
        );

        let mut expected_summary = vec![first, second];
        expected_summary.sort_by_key(|worktree_id| worktree_id.0);
        let mut expected_detail = vec![first, third];
        expected_detail.sort_by_key(|worktree_id| worktree_id.0);
        let mut expected_seed_pairs = expected_summary
            .iter()
            .copied()
            .chain(expected_detail.iter().copied())
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|worktree_id| {
                let tier = if expected_detail.contains(&worktree_id) {
                    WorktreeVcsStreamTier::Details
                } else {
                    WorktreeVcsStreamTier::Summary
                };
                (worktree_id, tier)
            })
            .collect::<Vec<_>>();
        expected_seed_pairs.sort_by_key(|(worktree_id, _)| worktree_id.0);
        assert_eq!(plan.summary_subscribed_worktree_ids, expected_summary);
        assert_eq!(plan.detail_subscribed_worktree_ids, expected_detail);
        assert_eq!(plan.state.demand_generation, 1);
        assert_eq!(seed_pairs(&plan.seed_plan), expected_seed_pairs);
    }

    #[test]
    fn subscription_plan_preserves_repeat_and_tier_transition_semantics() {
        let worktree_id = WorktreeId::new();

        let initial = plan_workspace_vcs_subscription_update(
            WorkspaceVcsDemandState::default(),
            vec![worktree_id],
            Vec::new(),
        );
        let repeat = plan_workspace_vcs_subscription_update(
            initial.state.clone(),
            vec![worktree_id],
            Vec::new(),
        );
        assert_eq!(repeat.state.demand_generation, 2);
        assert!(repeat.summary_seed_worktree_ids.is_empty());
        assert!(repeat.detail_seed_worktree_ids.is_empty());
        assert!(repeat.seed_plan.seeds.is_empty());
        assert!(repeat.summary_refresh_worktree_ids.is_empty());
        assert!(repeat.detail_refresh_worktree_ids.is_empty());

        let upgrade = plan_workspace_vcs_subscription_update(
            repeat.state.clone(),
            vec![worktree_id],
            vec![worktree_id],
        );
        assert_eq!(upgrade.state.demand_generation, 3);
        assert!(upgrade.summary_seed_worktree_ids.is_empty());
        assert_eq!(
            upgrade.detail_seed_worktree_ids,
            HashSet::from([worktree_id])
        );
        assert_eq!(
            seed_pairs(&upgrade.seed_plan),
            vec![(worktree_id, WorktreeVcsStreamTier::Details)]
        );
        assert!(upgrade.summary_refresh_worktree_ids.is_empty());
        assert_eq!(upgrade.detail_refresh_worktree_ids, vec![worktree_id]);

        let demotion = plan_workspace_vcs_subscription_update(
            upgrade.state.clone(),
            vec![worktree_id],
            Vec::new(),
        );
        assert_eq!(demotion.state.demand_generation, 4);
        assert!(demotion.summary_seed_worktree_ids.is_empty());
        assert!(demotion.detail_seed_worktree_ids.is_empty());
        assert!(demotion.seed_plan.seeds.is_empty());
        assert!(demotion.summary_refresh_worktree_ids.is_empty());
        assert!(demotion.detail_refresh_worktree_ids.is_empty());
    }

    #[test]
    fn snapshot_route_prefers_detail_over_summary_and_drops_unsubscribed() {
        let summary = WorktreeId::new();
        let detail = WorktreeId::new();
        let both = WorktreeId::new();
        let demand = WorkspaceVcsDemandState {
            demand_generation: 7,
            summary_worktree_ids: HashSet::from([summary, both]),
            detail_worktree_ids: HashSet::from([detail, both]),
        };

        assert_eq!(
            route_workspace_vcs_snapshot(&demand, summary),
            WorkspaceVcsSnapshotRoute::Summary,
        );
        assert_eq!(
            route_workspace_vcs_snapshot(&demand, detail),
            WorkspaceVcsSnapshotRoute::Details,
        );
        assert_eq!(
            route_workspace_vcs_snapshot(&demand, both),
            WorkspaceVcsSnapshotRoute::Details,
        );
        assert_eq!(
            route_workspace_vcs_snapshot(&demand, WorktreeId::new()),
            WorkspaceVcsSnapshotRoute::Drop,
        );
    }

    #[test]
    fn lag_reseed_sorts_and_prefers_detail_tier() {
        let summary = WorktreeId::new();
        let detail = WorktreeId::new();
        let both = WorktreeId::new();
        let demand = WorkspaceVcsDemandState {
            demand_generation: 9,
            summary_worktree_ids: HashSet::from([summary, both]),
            detail_worktree_ids: HashSet::from([detail, both]),
        };
        let mut expected = vec![
            (summary, WorktreeVcsStreamTier::Summary),
            (detail, WorktreeVcsStreamTier::Details),
            (both, WorktreeVcsStreamTier::Details),
        ];
        expected.sort_by_key(|(worktree_id, _)| worktree_id.0);

        assert_eq!(
            seed_pairs(&plan_workspace_vcs_lag_reseed(&demand)),
            expected
        );
    }

    #[test]
    fn refresh_plan_dedupes_sorts_and_respects_tier() {
        let first = WorktreeId::new();
        let second = WorktreeId::new();
        let mut expected = vec![first, second];
        expected.sort_by_key(|worktree_id| worktree_id.0);

        let summary =
            plan_workspace_vcs_refresh(vec![second, first, second], WorktreeVcsStreamTier::Summary);
        assert_eq!(summary.summary_refresh_worktree_ids, expected);
        assert!(summary.detail_refresh_worktree_ids.is_empty());

        let details =
            plan_workspace_vcs_refresh(vec![second, first, second], WorktreeVcsStreamTier::Details);
        assert!(details.summary_refresh_worktree_ids.is_empty());
        assert_eq!(details.detail_refresh_worktree_ids, expected);
    }
}
