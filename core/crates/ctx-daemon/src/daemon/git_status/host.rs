use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::Instant;

use anyhow::Result;
use async_trait::async_trait;
use ctx_core::ids::{WorkspaceId, WorktreeId};
use ctx_core::models::{
    Worktree, WorktreeVcsComputeState, WorktreeVcsFreshness, WorktreeVcsSnapshot,
    WorktreeVcsTouchedFiles, WorktreeVcsTouchedFilesState,
};
use ctx_settings_model::{ContainerRuntimeKind, ExecutionSettings};
use ctx_store::Store;
use ctx_workspace_container::workspace_container_name;
use ctx_workspace_runtime::HarnessRuntimeManager;
use ctx_worktree_data_plane::{
    apply_data_plane_to_execution_settings,
    resolve_worktree_data_plane_with_host as resolve_worktree_data_plane, WorktreeDataPlaneHost,
};
use ctx_worktree_vcs_service::{
    claim_next_worktree_vcs_job, finish_worktree_vcs_job, finish_worktree_vcs_refresh,
    mark_worktree_vcs_runtime_dirty, pending_worktree_vcs_snapshot_cache_entry,
    publish_worktree_vcs_snapshot_cache_entry, published_worktree_vcs_snapshot_cache_entry,
    queue_worktree_vcs_refresh, GitStatusSnapshot, WorktreeVcsDirtyBits, WorktreeVcsRuntimeState,
    WorktreeVcsSchedulerJob, WorktreeVcsSchedulerRuntime, WorktreeVcsSnapshotCacheEntry,
    WorktreeVcsSnapshotPublishPolicy,
};
use tokio::sync::{broadcast, Mutex, OwnedSemaphorePermit};

use crate::daemon::state::{TimedEntry, WorkspaceRuntime};
use crate::daemon::ProtectedWorkspaceStoreLookup;

#[derive(Clone)]
pub(in crate::daemon) struct WorktreeVcsExecutionHost {
    data_root: PathBuf,
    daemon_url: String,
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    harness: Arc<HarnessRuntimeManager>,
}

impl WorktreeVcsExecutionHost {
    pub(in crate::daemon) fn new(
        data_root: PathBuf,
        daemon_url: String,
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        harness: Arc<HarnessRuntimeManager>,
    ) -> Self {
        Self {
            data_root,
            daemon_url,
            global_store,
            workspace_stores,
            harness,
        }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) async fn store_for_worktree(
        &self,
        worktree_id: WorktreeId,
    ) -> Result<Store> {
        self.workspace_stores.store_for_worktree(worktree_id).await
    }

    pub(in crate::daemon) async fn store_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store> {
        self.workspace_stores
            .store_for_workspace(workspace_id)
            .await
    }

    async fn effective_execution_settings(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<ExecutionSettings> {
        let store = self.store_for_workspace(workspace_id).await?;
        ctx_settings_service::effective_execution_settings_classified(&self.global_store, &store)
            .await
            .map_err(ctx_settings_service::EffectiveExecutionSettingsError::into_inner)
    }

    pub(in crate::daemon) async fn sandbox_context(
        &self,
        worktree: &Worktree,
    ) -> Result<WorktreeVcsSandboxContext> {
        let data_plane = resolve_worktree_data_plane(self, worktree).await?;
        let effective = self
            .effective_execution_settings(data_plane.workspace.id)
            .await?;
        let effective = apply_data_plane_to_execution_settings(&effective, &data_plane)?;
        self.harness
            .ensure_workspace_container_for_worktree(
                &data_plane.workspace,
                worktree,
                &effective,
                &self.daemon_url,
            )
            .await?;
        let target = if matches!(
            effective.container.runtime,
            ContainerRuntimeKind::SharedVmContainer
        ) {
            WorktreeVcsSandboxTarget::SharedVmContainer
        } else {
            WorktreeVcsSandboxTarget::NativeContainer {
                container_name: workspace_container_name(worktree.workspace_id),
            }
        };
        Ok(WorktreeVcsSandboxContext {
            live_worktree_root: data_plane.live_worktree_root,
            target,
        })
    }
}

#[async_trait]
impl WorktreeDataPlaneHost for WorktreeVcsExecutionHost {
    async fn get_workspace(
        state: &Self,
        workspace_id: WorkspaceId,
    ) -> Result<Option<ctx_core::models::Workspace>> {
        state.global_store.get_workspace(workspace_id).await
    }

    async fn workspace_store(state: &Self, workspace_id: WorkspaceId) -> Result<Store> {
        state.store_for_workspace(workspace_id).await
    }
}

pub(in crate::daemon) enum WorktreeVcsSandboxTarget {
    NativeContainer { container_name: String },
    SharedVmContainer,
}

pub(in crate::daemon) struct WorktreeVcsSandboxContext {
    pub(in crate::daemon) live_worktree_root: PathBuf,
    pub(in crate::daemon) target: WorktreeVcsSandboxTarget,
}

#[derive(Clone)]
pub(in crate::daemon) struct WorktreeVcsRuntimeHost {
    worktree_vcs_enabled: bool,
    worktree_vcs_snapshots:
        Arc<Mutex<HashMap<WorktreeId, TimedEntry<WorktreeVcsSnapshotCacheEntry>>>>,
    worktree_vcs_active: Arc<Mutex<HashMap<WorktreeId, usize>>>,
    worktree_vcs_refresh_locks: Arc<Mutex<HashMap<WorktreeId, Weak<Mutex<()>>>>>,
    worktree_vcs_open_panes: Arc<Mutex<HashMap<WorktreeId, usize>>>,
    #[cfg(any(test, feature = "test-support"))]
    worktree_vcs_summary_gen: Arc<Mutex<HashMap<WorktreeId, u64>>>,
    worktree_vcs_runtime: Arc<Mutex<HashMap<WorktreeId, WorktreeVcsRuntimeState>>>,
    worktree_vcs_scheduler: WorktreeVcsSchedulerRuntime,
    worktree_vcs_events: broadcast::Sender<WorktreeVcsSnapshot>,
    git_status_watchers: Arc<Mutex<HashSet<WorktreeId>>>,
}

impl WorktreeVcsRuntimeHost {
    pub(in crate::daemon) fn from_workspace_runtime(runtime: &WorkspaceRuntime) -> Self {
        Self {
            worktree_vcs_enabled: runtime.worktree_vcs_enabled,
            worktree_vcs_snapshots: Arc::clone(&runtime.worktree_vcs_snapshots),
            worktree_vcs_active: Arc::clone(&runtime.worktree_vcs_active),
            worktree_vcs_refresh_locks: Arc::clone(&runtime.worktree_vcs_refresh_locks),
            worktree_vcs_open_panes: Arc::clone(&runtime.worktree_vcs_open_panes),
            #[cfg(any(test, feature = "test-support"))]
            worktree_vcs_summary_gen: Arc::clone(&runtime.worktree_vcs_summary_gen),
            worktree_vcs_runtime: Arc::clone(&runtime.worktree_vcs_runtime),
            worktree_vcs_scheduler: runtime.worktree_vcs_scheduler.clone(),
            worktree_vcs_events: runtime.worktree_vcs_events.clone(),
            git_status_watchers: Arc::clone(&runtime.git_status_watchers),
        }
    }

    pub(in crate::daemon) fn enabled(&self) -> bool {
        self.worktree_vcs_enabled
    }

    pub(in crate::daemon) async fn cache_worktree_vcs_snapshot(
        &self,
        snapshot: WorktreeVcsSnapshot,
    ) {
        if !self.worktree_vcs_enabled {
            return;
        }
        let worktree_id = snapshot.worktree_id;
        let now = Instant::now();
        let mut cache = self.worktree_vcs_snapshots.lock().await;
        cache.insert(
            worktree_id,
            TimedEntry::new(published_worktree_vcs_snapshot_cache_entry(snapshot, now)),
        );
    }

    pub(in crate::daemon) async fn get_cached_worktree_vcs_snapshot(
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

    pub(in crate::daemon) async fn get_worktree_vcs_snapshot(
        &self,
        execution: &WorktreeVcsExecutionHost,
        worktree_id: WorktreeId,
    ) -> Option<WorktreeVcsSnapshot> {
        if let Some(snapshot) = self.get_cached_worktree_vcs_snapshot(worktree_id).await {
            return Some(snapshot);
        }
        if !self.is_worktree_vcs_active(worktree_id).await {
            return None;
        }
        let store = execution.store_for_worktree(worktree_id).await.ok()?;
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
        self.cache_worktree_vcs_snapshot(snapshot.clone()).await;
        Some(snapshot)
    }

    pub(in crate::daemon) async fn queue_worktree_vcs_refresh(
        &self,
        worktree_id: WorktreeId,
        summary: bool,
        touched_files: bool,
    ) {
        if !self.worktree_vcs_enabled {
            return;
        }
        let mut runtime = self.worktree_vcs_runtime.lock().await;
        let entry = runtime.entry(worktree_id).or_default();
        queue_worktree_vcs_refresh(entry, summary, touched_files);
    }

    pub(in crate::daemon) async fn mark_worktree_vcs_dirty(
        &self,
        worktree_id: WorktreeId,
        dirty_bits: WorktreeVcsDirtyBits,
        candidate_paths: Vec<String>,
        pane_open: bool,
    ) {
        if !self.worktree_vcs_enabled {
            return;
        }
        let mut runtime = self.worktree_vcs_runtime.lock().await;
        let entry = runtime.entry(worktree_id).or_default();
        mark_worktree_vcs_runtime_dirty(entry, dirty_bits, candidate_paths, pane_open);
    }

    pub(in crate::daemon) async fn finish_worktree_vcs_refresh(
        &self,
        worktree_id: WorktreeId,
        git_snapshot: GitStatusSnapshot,
        touched_files: WorktreeVcsTouchedFiles,
        touched_files_state: WorktreeVcsTouchedFilesState,
    ) {
        if !self.worktree_vcs_enabled {
            return;
        }
        let mut runtime = self.worktree_vcs_runtime.lock().await;
        let entry = runtime.entry(worktree_id).or_default();
        finish_worktree_vcs_refresh(entry, git_snapshot, touched_files, touched_files_state);
    }

    pub(in crate::daemon) async fn upsert_worktree_vcs_snapshot(
        &self,
        snapshot: WorktreeVcsSnapshot,
        force_emit: bool,
        summary_at: Option<Instant>,
    ) -> Option<WorktreeVcsSnapshot> {
        if !self.worktree_vcs_enabled {
            return None;
        }
        let now = Instant::now();
        let policy = WorktreeVcsSnapshotPublishPolicy::default();
        let active = self.worktree_vcs_active.lock().await;
        if active.get(&snapshot.worktree_id).copied().unwrap_or(0) == 0 {
            return None;
        }
        let mut cache = self.worktree_vcs_snapshots.lock().await;
        let entry = cache.entry(snapshot.worktree_id).or_insert_with(|| {
            TimedEntry::new(pending_worktree_vcs_snapshot_cache_entry(
                snapshot.clone(),
                now,
                policy,
            ))
        });
        entry.touch_at(now);
        publish_worktree_vcs_snapshot_cache_entry(
            &mut entry.value,
            snapshot,
            now,
            force_emit,
            summary_at,
            policy,
        )
    }

    pub(in crate::daemon) fn publish_worktree_vcs_event(&self, snapshot: WorktreeVcsSnapshot) {
        let _ = self.worktree_vcs_events.send(snapshot);
    }

    pub(in crate::daemon) async fn claim_next_worktree_vcs_job(
        &self,
    ) -> Option<WorktreeVcsSchedulerJob> {
        if !self.worktree_vcs_enabled {
            return None;
        }
        let active = self.worktree_vcs_active.lock().await;
        let open = self.worktree_vcs_open_panes.lock().await;
        let mut runtime = self.worktree_vcs_runtime.lock().await;
        claim_next_worktree_vcs_job(&mut runtime, &active, &open)
    }

    pub(in crate::daemon) async fn finish_worktree_vcs_job(&self, worktree_id: WorktreeId) -> bool {
        if !self.worktree_vcs_enabled {
            return false;
        }
        let mut runtime = self.worktree_vcs_runtime.lock().await;
        finish_worktree_vcs_job(&mut runtime, worktree_id)
    }

    pub(in crate::daemon) async fn wait_worktree_vcs_scheduler_notification(&self) {
        self.worktree_vcs_scheduler.notify.notified().await;
    }

    pub(in crate::daemon) fn notify_worktree_vcs_scheduler(&self) {
        self.worktree_vcs_scheduler.notify.notify_one();
    }

    pub(in crate::daemon) fn try_acquire_worktree_vcs_scheduler_permit(
        &self,
    ) -> Option<OwnedSemaphorePermit> {
        self.worktree_vcs_scheduler
            .permits
            .clone()
            .try_acquire_owned()
            .ok()
    }

    pub(in crate::daemon) fn mark_worktree_vcs_scheduler_started(&self) -> bool {
        !self
            .worktree_vcs_scheduler
            .started
            .swap(true, Ordering::AcqRel)
    }

    pub(in crate::daemon) async fn worktree_vcs_refresh_lock(
        &self,
        worktree_id: WorktreeId,
    ) -> Arc<Mutex<()>> {
        let mut locks = self.worktree_vcs_refresh_locks.lock().await;
        match locks.get(&worktree_id).and_then(Weak::upgrade) {
            Some(lock) => lock,
            None => {
                let lock = Arc::new(Mutex::new(()));
                locks.insert(worktree_id, Arc::downgrade(&lock));
                lock
            }
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(in crate::daemon) async fn update_worktree_vcs_activity(
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
        if evicted.is_empty() {
            return;
        }
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

    #[cfg(any(test, feature = "test-support"))]
    pub(in crate::daemon) async fn update_worktree_vcs_open_panes(
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

    pub(in crate::daemon) async fn is_worktree_vcs_active(&self, worktree_id: WorktreeId) -> bool {
        if !self.worktree_vcs_enabled {
            return false;
        }
        let active = self.worktree_vcs_active.lock().await;
        active.get(&worktree_id).copied().unwrap_or(0) > 0
    }

    pub(in crate::daemon) async fn is_worktree_vcs_pane_open(
        &self,
        worktree_id: WorktreeId,
    ) -> bool {
        if !self.worktree_vcs_enabled {
            return false;
        }
        let open = self.worktree_vcs_open_panes.lock().await;
        open.get(&worktree_id).copied().unwrap_or(0) > 0
    }

    pub(in crate::daemon) async fn register_git_status_watcher(
        &self,
        worktree_id: WorktreeId,
    ) -> bool {
        if !self.worktree_vcs_enabled {
            return false;
        }
        let mut watchers = self.git_status_watchers.lock().await;
        watchers.insert(worktree_id)
    }

    pub(in crate::daemon) async fn release_git_status_watcher(&self, worktree_id: WorktreeId) {
        let mut watchers = self.git_status_watchers.lock().await;
        watchers.remove(&worktree_id);
    }

    pub(in crate::daemon) async fn ensure_git_status_watcher(
        &self,
        execution: WorktreeVcsExecutionHost,
        worktree: Worktree,
    ) {
        if !self.register_git_status_watcher(worktree.id).await {
            return;
        }
        let runtime = self.clone();
        let worktree_id = worktree.id;
        tokio::spawn(async move {
            if let Err(err) =
                super::run_git_status_watcher(runtime.clone(), execution, worktree).await
            {
                tracing::warn!(worktree_id = %worktree_id.0, "git status watcher failed: {err:#}");
            }
            runtime.release_git_status_watcher(worktree_id).await;
        });
    }
}
