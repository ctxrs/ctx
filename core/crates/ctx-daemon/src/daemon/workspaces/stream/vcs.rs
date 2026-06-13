use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ctx_core::ids::{WorkspaceId, WorktreeId};
use ctx_core::models::{
    Worktree, WorktreeVcsComputeState, WorktreeVcsFreshness, WorktreeVcsSnapshot,
    WorktreeVcsStreamTier, WorktreeVcsTouchedFilesState,
};
use ctx_observability::perf_telemetry::{PerfMetric, PerfMetricKind};
use ctx_route_contracts::workspaces::{WorkspaceStreamRouteError, WorkspaceStreamRouteParams};
use ctx_workspace_stream_service::vcs as stream_vcs_service;
pub use ctx_workspace_stream_service::vcs::{
    plan_workspace_vcs_lag_reseed, route_workspace_vcs_snapshot, WorkspaceVcsDemandState,
    WorkspaceVcsLagReseedPlan, WorkspaceVcsRefreshPlan, WorkspaceVcsSnapshotRoute,
    WorkspaceVcsSnapshotSeed, WorkspaceVcsSubscriptionPlan,
};
use ctx_worktree_vcs_service::{
    published_worktree_vcs_snapshot_cache_entry, WorktreeVcsRuntimeState,
    WorktreeVcsSnapshotCacheEntry,
};
use tokio::sync::broadcast;
use tokio::sync::Mutex;

use crate::daemon::state::{TimedEntry, WorkspaceRuntime};
use crate::daemon::WorkspaceVcsStreamHandle;

use super::access::workspace_stream_route_error_from_access;
use super::{WorkspaceStreamAccessError, WorkspaceStreamRouteAdmission};

#[derive(Clone)]
pub struct WorkspaceVcsStreamRuntime {
    worktree_vcs_enabled: bool,
    worktree_vcs_snapshots:
        Arc<Mutex<HashMap<WorktreeId, TimedEntry<WorktreeVcsSnapshotCacheEntry>>>>,
    worktree_vcs_active: Arc<Mutex<HashMap<WorktreeId, usize>>>,
    worktree_vcs_open_panes: Arc<Mutex<HashMap<WorktreeId, usize>>>,
    worktree_vcs_summary_gen: Arc<Mutex<HashMap<WorktreeId, u64>>>,
    worktree_vcs_runtime: Arc<Mutex<HashMap<WorktreeId, WorktreeVcsRuntimeState>>>,
    worktree_vcs_events: broadcast::Sender<WorktreeVcsSnapshot>,
}

impl WorkspaceVcsStreamRuntime {
    pub(in crate::daemon) fn from_workspace_runtime(runtime: &WorkspaceRuntime) -> Self {
        Self {
            worktree_vcs_enabled: runtime.worktree_vcs_enabled,
            worktree_vcs_snapshots: Arc::clone(&runtime.worktree_vcs_snapshots),
            worktree_vcs_active: Arc::clone(&runtime.worktree_vcs_active),
            worktree_vcs_open_panes: Arc::clone(&runtime.worktree_vcs_open_panes),
            worktree_vcs_summary_gen: Arc::clone(&runtime.worktree_vcs_summary_gen),
            worktree_vcs_runtime: Arc::clone(&runtime.worktree_vcs_runtime),
            worktree_vcs_events: runtime.worktree_vcs_events.clone(),
        }
    }

    pub(in crate::daemon) fn worktree_vcs_enabled(&self) -> bool {
        self.worktree_vcs_enabled
    }

    async fn cache_worktree_vcs_snapshot(&self, snapshot: WorktreeVcsSnapshot) {
        if !self.worktree_vcs_enabled {
            return;
        }
        let worktree_id = snapshot.worktree_id;
        let now = std::time::Instant::now();
        let mut cache = self.worktree_vcs_snapshots.lock().await;
        cache.insert(
            worktree_id,
            TimedEntry::new(published_worktree_vcs_snapshot_cache_entry(snapshot, now)),
        );
    }

    #[cfg(test)]
    pub(in crate::daemon) async fn cache_worktree_vcs_snapshot_for_test(
        &self,
        snapshot: WorktreeVcsSnapshot,
    ) {
        self.cache_worktree_vcs_snapshot(snapshot).await;
    }

    async fn get_cached_worktree_vcs_snapshot(
        &self,
        worktree_id: WorktreeId,
    ) -> Option<WorktreeVcsSnapshot> {
        if !self.worktree_vcs_enabled {
            return None;
        }
        let mut cache = self.worktree_vcs_snapshots.lock().await;
        cache.get_mut(&worktree_id).map(|entry| {
            entry.touch();
            entry.value.snapshot.clone()
        })
    }

    async fn update_worktree_vcs_activity(
        &self,
        previous: &HashSet<WorktreeId>,
        next: &HashSet<WorktreeId>,
    ) {
        if !self.worktree_vcs_enabled || previous == next {
            return;
        }
        let mut evicted = Vec::new();
        {
            let mut active = self.worktree_vcs_active.lock().await;
            for worktree_id in previous.difference(next) {
                if let Some(count) = active.get_mut(worktree_id) {
                    if *count <= 1 {
                        active.remove(worktree_id);
                        evicted.push(*worktree_id);
                    } else {
                        *count -= 1;
                    }
                }
            }
            for worktree_id in next.difference(previous) {
                let entry = active.entry(*worktree_id).or_insert(0);
                *entry += 1;
            }
        }
        if !evicted.is_empty() {
            {
                let mut cache = self.worktree_vcs_snapshots.lock().await;
                for worktree_id in &evicted {
                    cache.remove(worktree_id);
                }
            }
            {
                let mut gens = self.worktree_vcs_summary_gen.lock().await;
                for worktree_id in &evicted {
                    gens.remove(worktree_id);
                }
            }
            {
                let mut runtime = self.worktree_vcs_runtime.lock().await;
                for worktree_id in &evicted {
                    runtime.remove(worktree_id);
                }
            }
        }
    }

    async fn update_worktree_vcs_open_panes(
        &self,
        previous: &HashSet<WorktreeId>,
        next: &HashSet<WorktreeId>,
    ) {
        if !self.worktree_vcs_enabled || previous == next {
            return;
        }
        let mut open = self.worktree_vcs_open_panes.lock().await;
        for worktree_id in previous.difference(next) {
            if let Some(count) = open.get_mut(worktree_id) {
                if *count <= 1 {
                    open.remove(worktree_id);
                } else {
                    *count -= 1;
                }
            }
        }
        for worktree_id in next.difference(previous) {
            let entry = open.entry(*worktree_id).or_insert(0);
            *entry += 1;
        }
    }

    async fn is_worktree_vcs_active(&self, worktree_id: WorktreeId) -> bool {
        if !self.worktree_vcs_enabled {
            return false;
        }
        let active = self.worktree_vcs_active.lock().await;
        active.get(&worktree_id).copied().unwrap_or(0) > 0
    }

    #[cfg(test)]
    async fn is_worktree_vcs_pane_open(&self, worktree_id: WorktreeId) -> bool {
        if !self.worktree_vcs_enabled {
            return false;
        }
        let open = self.worktree_vcs_open_panes.lock().await;
        open.get(&worktree_id).copied().unwrap_or(0) > 0
    }

    fn subscribe_worktree_vcs_events(&self) -> broadcast::Receiver<WorktreeVcsSnapshot> {
        self.worktree_vcs_events.subscribe()
    }
}

pub async fn filter_workspace_worktree_ids(
    handle: &WorkspaceVcsStreamHandle,
    workspace_id: WorkspaceId,
    worktree_ids: Vec<WorktreeId>,
) -> Vec<WorktreeId> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for worktree_id in worktree_ids {
        if !seen.insert(worktree_id) {
            continue;
        }
        let Some(worktree) = load_worktree(handle, worktree_id).await else {
            continue;
        };
        if worktree.workspace_id == workspace_id {
            out.push(worktree_id);
        }
    }
    out.sort_by_key(|worktree_id| worktree_id.0);
    out
}

pub async fn plan_workspace_vcs_subscription_update(
    handle: &WorkspaceVcsStreamHandle,
    workspace_id: WorkspaceId,
    current: WorkspaceVcsDemandState,
    summary_worktree_ids: Vec<WorktreeId>,
    detail_worktree_ids: Vec<WorktreeId>,
) -> WorkspaceVcsSubscriptionPlan {
    let previous_active = current.active_worktree_ids();
    let previous_details = current.detail_worktree_ids.clone();
    let summary_worktree_ids =
        filter_workspace_worktree_ids(handle, workspace_id, summary_worktree_ids).await;
    let detail_worktree_ids =
        filter_workspace_worktree_ids(handle, workspace_id, detail_worktree_ids).await;
    let plan = stream_vcs_service::plan_workspace_vcs_subscription_update(
        current,
        summary_worktree_ids,
        detail_worktree_ids,
    );
    let next_active = plan.state.active_worktree_ids();

    handle
        .update_worktree_vcs_activity(&previous_active, &next_active)
        .await;
    handle
        .update_worktree_vcs_open_panes(&previous_details, &plan.state.detail_worktree_ids)
        .await;

    plan
}

pub async fn plan_workspace_vcs_refresh(
    handle: &WorkspaceVcsStreamHandle,
    workspace_id: WorkspaceId,
    worktree_ids: Vec<WorktreeId>,
    tier: WorktreeVcsStreamTier,
) -> WorkspaceVcsRefreshPlan {
    let worktree_ids = filter_workspace_worktree_ids(handle, workspace_id, worktree_ids).await;
    stream_vcs_service::plan_workspace_vcs_refresh(worktree_ids, tier)
}

pub async fn release_workspace_vcs_demand(
    handle: &WorkspaceVcsStreamHandle,
    demand: &WorkspaceVcsDemandState,
) {
    let active = demand.active_worktree_ids();
    if active.is_empty() && demand.detail_worktree_ids.is_empty() {
        return;
    }
    handle
        .update_worktree_vcs_activity(&active, &HashSet::new())
        .await;
    handle
        .update_worktree_vcs_open_panes(&demand.detail_worktree_ids, &HashSet::new())
        .await;
}

pub async fn refresh_worktree_vcs_for_worktrees(
    handle: &WorkspaceVcsStreamHandle,
    summary_worktree_ids: &[WorktreeId],
    detail_worktree_ids: &[WorktreeId],
) {
    if !handle.runtime().worktree_vcs_enabled() {
        return;
    }
    if summary_worktree_ids.is_empty() && detail_worktree_ids.is_empty() {
        return;
    }
    let mut worktrees: HashMap<WorktreeId, (Worktree, bool)> = HashMap::new();
    for worktree_id in summary_worktree_ids {
        if let Some(worktree) = load_worktree(handle, *worktree_id).await {
            worktrees.entry(worktree.id).or_insert((worktree, false));
        }
    }
    for worktree_id in detail_worktree_ids {
        if let Some(worktree) = load_worktree(handle, *worktree_id).await {
            worktrees
                .entry(worktree.id)
                .and_modify(|(_, details)| *details = true)
                .or_insert((worktree, true));
        }
    }

    for (worktree_id, (worktree, details)) in worktrees {
        handle
            .ensure_loaded_worktree_vcs_watcher(worktree.clone())
            .await;
        let should_refresh = !matches!(
            handle.get_worktree_vcs_snapshot(worktree.id).await,
            Some(snapshot)
                if snapshot.freshness == WorktreeVcsFreshness::Fresh
                    && snapshot.available
                    && (!details
                        || matches!(
                            snapshot.touched_files_state,
                            ctx_core::models::WorktreeVcsTouchedFilesState::Ready
                        ))
        );
        if should_refresh {
            if let Err(err) = handle
                .refresh_loaded_worktree_vcs(worktree, true, details)
                .await
            {
                tracing::warn!(
                    worktree_id = %worktree_id.0,
                    "worktree vcs refresh failed: {err:#}"
                );
            }
        }
    }
}

pub async fn ensure_worktree_vcs_watchers_for_worktrees(
    handle: &WorkspaceVcsStreamHandle,
    summary_worktree_ids: &[WorktreeId],
    detail_worktree_ids: &[WorktreeId],
) {
    if !handle.runtime().worktree_vcs_enabled() {
        return;
    }
    if summary_worktree_ids.is_empty() && detail_worktree_ids.is_empty() {
        return;
    }
    let mut worktrees: HashMap<WorktreeId, Worktree> = HashMap::new();
    for worktree_id in summary_worktree_ids {
        if let Some(worktree) = load_worktree(handle, *worktree_id).await {
            worktrees.entry(worktree.id).or_insert(worktree);
        }
    }
    for worktree_id in detail_worktree_ids {
        if let Some(worktree) = load_worktree(handle, *worktree_id).await {
            worktrees.entry(worktree.id).or_insert(worktree);
        }
    }
    for worktree in worktrees.into_values() {
        handle.ensure_loaded_worktree_vcs_watcher(worktree).await;
    }
}

async fn load_worktree(
    handle: &WorkspaceVcsStreamHandle,
    worktree_id: WorktreeId,
) -> Option<Worktree> {
    let store = handle.store_for_worktree(worktree_id).await.ok()?;
    store.get_worktree(worktree_id).await.ok().flatten()
}

impl WorkspaceVcsStreamHandle {
    pub async fn require_workspace_vcs_stream_access(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<(), WorkspaceStreamAccessError> {
        let exists = self
            .global_store()
            .get_workspace(workspace_id)
            .await
            .map_err(WorkspaceStreamAccessError::Internal)?
            .is_some();
        if !exists {
            return Err(WorkspaceStreamAccessError::NotFound);
        }
        Ok(())
    }

    pub async fn admit_workspace_vcs_stream_for_route(
        &self,
        params: WorkspaceStreamRouteParams,
    ) -> Result<WorkspaceStreamRouteAdmission, WorkspaceStreamRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.require_workspace_vcs_stream_access(workspace_id)
            .await
            .map_err(workspace_stream_route_error_from_access)?;
        Ok(WorkspaceStreamRouteAdmission::new(workspace_id))
    }

    pub async fn get_worktree_vcs_snapshot(
        &self,
        worktree_id: WorktreeId,
    ) -> Option<WorktreeVcsSnapshot> {
        if !self.runtime().worktree_vcs_enabled() {
            return None;
        }
        if let Some(snapshot) = self
            .runtime()
            .get_cached_worktree_vcs_snapshot(worktree_id)
            .await
        {
            return Some(snapshot);
        }
        if !self.runtime().is_worktree_vcs_active(worktree_id).await {
            return None;
        }
        let store = self.store_for_worktree(worktree_id).await.ok()?;
        let mut snapshot = store
            .get_worktree_vcs_snapshot_cache(worktree_id)
            .await
            .ok()
            .flatten()?;
        snapshot.compute_state = if snapshot.summary.file_count.is_some() {
            WorktreeVcsComputeState::Ready
        } else {
            WorktreeVcsComputeState::Computing
        };
        snapshot.freshness = if snapshot.summary.file_count.is_some() {
            WorktreeVcsFreshness::Stale
        } else {
            WorktreeVcsFreshness::Refreshing
        };
        snapshot.touched_files = Default::default();
        snapshot.touched_files_state = WorktreeVcsTouchedFilesState::NotLoaded;
        self.runtime()
            .cache_worktree_vcs_snapshot(snapshot.clone())
            .await;
        Some(snapshot)
    }

    pub fn subscribe_worktree_vcs_events(&self) -> broadcast::Receiver<WorktreeVcsSnapshot> {
        self.runtime().subscribe_worktree_vcs_events()
    }

    pub async fn filter_workspace_worktree_ids(
        &self,
        workspace_id: WorkspaceId,
        worktree_ids: Vec<WorktreeId>,
    ) -> Vec<WorktreeId> {
        filter_workspace_worktree_ids(self, workspace_id, worktree_ids).await
    }

    pub async fn refresh_worktree_vcs_for_worktrees(
        &self,
        summary_worktree_ids: &[WorktreeId],
        detail_worktree_ids: &[WorktreeId],
    ) {
        refresh_worktree_vcs_for_worktrees(self, summary_worktree_ids, detail_worktree_ids).await;
    }

    pub async fn ensure_worktree_vcs_watchers_for_worktrees(
        &self,
        summary_worktree_ids: &[WorktreeId],
        detail_worktree_ids: &[WorktreeId],
    ) {
        ensure_worktree_vcs_watchers_for_worktrees(self, summary_worktree_ids, detail_worktree_ids)
            .await;
    }

    pub async fn update_worktree_vcs_activity(
        &self,
        previous: &HashSet<WorktreeId>,
        next: &HashSet<WorktreeId>,
    ) {
        self.runtime()
            .update_worktree_vcs_activity(previous, next)
            .await;
    }

    pub async fn update_worktree_vcs_open_panes(
        &self,
        previous: &HashSet<WorktreeId>,
        next: &HashSet<WorktreeId>,
    ) {
        self.runtime()
            .update_worktree_vcs_open_panes(previous, next)
            .await;
    }

    #[cfg(test)]
    pub async fn is_worktree_vcs_active_for_test(&self, worktree_id: WorktreeId) -> bool {
        self.runtime().is_worktree_vcs_active(worktree_id).await
    }

    #[cfg(test)]
    pub async fn is_worktree_vcs_pane_open_for_test(&self, worktree_id: WorktreeId) -> bool {
        self.runtime().is_worktree_vcs_pane_open(worktree_id).await
    }

    pub async fn plan_workspace_vcs_subscription_update(
        &self,
        workspace_id: WorkspaceId,
        current: WorkspaceVcsDemandState,
        summary_worktree_ids: Vec<WorktreeId>,
        detail_worktree_ids: Vec<WorktreeId>,
    ) -> WorkspaceVcsSubscriptionPlan {
        plan_workspace_vcs_subscription_update(
            self,
            workspace_id,
            current,
            summary_worktree_ids,
            detail_worktree_ids,
        )
        .await
    }

    pub async fn plan_workspace_vcs_refresh(
        &self,
        workspace_id: WorkspaceId,
        worktree_ids: Vec<WorktreeId>,
        tier: WorktreeVcsStreamTier,
    ) -> WorkspaceVcsRefreshPlan {
        plan_workspace_vcs_refresh(self, workspace_id, worktree_ids, tier).await
    }

    pub async fn release_workspace_vcs_demand(&self, demand: &WorkspaceVcsDemandState) {
        release_workspace_vcs_demand(self, demand).await;
    }

    pub fn route_workspace_vcs_snapshot(
        &self,
        demand: &WorkspaceVcsDemandState,
        worktree_id: WorktreeId,
    ) -> WorkspaceVcsSnapshotRoute {
        route_workspace_vcs_snapshot(demand, worktree_id)
    }

    pub fn plan_workspace_vcs_lag_reseed(
        &self,
        demand: &WorkspaceVcsDemandState,
    ) -> WorkspaceVcsLagReseedPlan {
        plan_workspace_vcs_lag_reseed(demand)
    }

    pub async fn record_workspace_vcs_stream_metric(&self, name: &str, value: u64) {
        let mut labels = HashMap::new();
        labels.insert("source".to_string(), "daemon".to_string());
        labels.insert("stream".to_string(), "workspace_vcs".to_string());
        let metric = PerfMetric {
            name: name.to_string(),
            kind: PerfMetricKind::Counter,
            unit: "count".to_string(),
            value: value as f64,
            labels,
        };
        self.perf_telemetry()
            .record_metric(metric, None, None, None)
            .await;
    }
}
