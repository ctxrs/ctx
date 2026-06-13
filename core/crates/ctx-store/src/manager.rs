use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(test)]
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Serialize;
use tokio::sync::Mutex;

use ctx_core::ids::{SessionId, TaskId, WorkspaceId, WorktreeId};
use ctx_core::models::Workspace;
use ctx_fs::permissions::ensure_private_dir;

use crate::Store;

mod leases;
use leases::WorkspaceStoreLeaseRegistry;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_eviction;

const DEFAULT_WORKSPACE_MAX_CONNECTIONS: u32 = 2;
const DEFAULT_MAX_CACHED_WORKSPACES: usize = 12;

#[derive(Clone, Debug)]
pub struct StoreManagerConfig {
    pub max_connections: Option<u32>,
    pub workspace_max_connections: Option<u32>,
    pub max_cached_workspaces: usize,
}

impl Default for StoreManagerConfig {
    fn default() -> Self {
        Self {
            max_connections: None,
            workspace_max_connections: None,
            max_cached_workspaces: DEFAULT_MAX_CACHED_WORKSPACES,
        }
    }
}

#[derive(Clone)]
pub struct StoreManager {
    global: Store,
    data_root: PathBuf,
    workspace_stores: Arc<Mutex<HashMap<WorkspaceId, TimedStoreEntry>>>,
    workspace_delete_barriers: Arc<Mutex<HashSet<WorkspaceId>>>,
    store_leases: Arc<WorkspaceStoreLeaseRegistry>,
    next_store_instance_id: Arc<AtomicU64>,
    config: StoreManagerConfig,
}

#[derive(Clone, Debug, Serialize)]
pub struct StoreManagerStats {
    pub global_pool_size: usize,
    pub global_pool_idle: usize,
    pub workspace_store_count: usize,
    pub workspace_pool_size_total: usize,
    pub workspace_pool_idle_total: usize,
    pub workspace_pool_size_max: usize,
    pub workspace_pool_idle_max: usize,
}

#[derive(Clone)]
struct TimedStoreEntry {
    workspace_id: WorkspaceId,
    store: Store,
    last_access: Instant,
    instance_id: u64,
}

pub struct WorkspaceStoreAccess {
    pub store: Store,
    pub kind: WorkspaceStoreAccessKind,
}

pub enum WorkspaceStoreAccessOutcome {
    Access(WorkspaceStoreAccess),
    Missing,
    Deleting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceStoreAccessKind {
    Cached,
    ColdOpen,
    Reactivated,
}

impl WorkspaceStoreAccessKind {
    pub fn triggers_open_side_effects(self) -> bool {
        matches!(self, Self::ColdOpen | Self::Reactivated)
    }
}

impl TimedStoreEntry {
    fn new(workspace_id: WorkspaceId, store: Store, instance_id: u64) -> Self {
        Self {
            workspace_id,
            store,
            last_access: Instant::now(),
            instance_id,
        }
    }

    fn touch(&mut self) {
        self.last_access = Instant::now();
    }
}

impl StoreManager {
    pub async fn open(data_root: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_config(data_root, StoreManagerConfig::default()).await
    }

    pub async fn open_with_config(
        data_root: impl AsRef<Path>,
        mut config: StoreManagerConfig,
    ) -> Result<Self> {
        let data_root = data_root.as_ref().to_path_buf();
        ensure_private_dir(&data_root).await?;
        let db_dir = data_root.join("db");
        ensure_private_dir(&db_dir).await?;
        let global_db_path = db_dir.join("db.sqlite");
        config.max_cached_workspaces = config.max_cached_workspaces.max(1);
        let global = Store::open_sqlite(&global_db_path, config.max_connections).await?;

        Ok(Self {
            global,
            data_root,
            workspace_stores: Arc::new(Mutex::new(HashMap::new())),
            workspace_delete_barriers: Arc::new(Mutex::new(HashSet::new())),
            store_leases: Arc::new(WorkspaceStoreLeaseRegistry::new()?),
            next_store_instance_id: Arc::new(AtomicU64::new(1)),
            config,
        })
    }

    async fn is_workspace_delete_blocked(&self, workspace_id: WorkspaceId) -> bool {
        self.workspace_delete_barriers
            .lock()
            .await
            .contains(&workspace_id)
    }

    pub async fn is_workspace_deleting(&self, workspace_id: WorkspaceId) -> bool {
        self.is_workspace_delete_blocked(workspace_id).await
    }

    pub async fn begin_workspace_delete(&self, workspace_id: WorkspaceId) {
        self.workspace_delete_barriers
            .lock()
            .await
            .insert(workspace_id);
    }

    pub async fn finish_workspace_delete(&self, workspace_id: WorkspaceId) {
        self.workspace_delete_barriers
            .lock()
            .await
            .remove(&workspace_id);
    }

    pub fn global(&self) -> &Store {
        &self.global
    }

    pub async fn stats(&self) -> StoreManagerStats {
        let global_stats = self.global.stats();
        let stores = self.workspace_stores.lock().await;
        let workspace_store_count = stores.len();
        let mut workspace_pool_size_total: usize = 0;
        let mut workspace_pool_idle_total: usize = 0;
        let mut workspace_pool_size_max: usize = 0;
        let mut workspace_pool_idle_max: usize = 0;
        for entry in stores.values() {
            let stats = entry.store.stats();
            workspace_pool_size_total = workspace_pool_size_total.saturating_add(stats.pool_size);
            workspace_pool_idle_total = workspace_pool_idle_total.saturating_add(stats.pool_idle);
            if stats.pool_size > workspace_pool_size_max {
                workspace_pool_size_max = stats.pool_size;
            }
            if stats.pool_idle > workspace_pool_idle_max {
                workspace_pool_idle_max = stats.pool_idle;
            }
        }
        StoreManagerStats {
            global_pool_size: global_stats.pool_size,
            global_pool_idle: global_stats.pool_idle,
            workspace_store_count,
            workspace_pool_size_total,
            workspace_pool_idle_total,
            workspace_pool_size_max,
            workspace_pool_idle_max,
        }
    }

    pub async fn workspace(&self, workspace_id: WorkspaceId) -> Result<Store> {
        match self.workspace_access_outcome(workspace_id).await? {
            WorkspaceStoreAccessOutcome::Access(access) => Ok(access.store),
            WorkspaceStoreAccessOutcome::Missing | WorkspaceStoreAccessOutcome::Deleting => {
                anyhow::bail!("workspace {} not found", workspace_id.0)
            }
        }
    }

    pub async fn workspace_access(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<WorkspaceStoreAccess> {
        match self.workspace_access_outcome(workspace_id).await? {
            WorkspaceStoreAccessOutcome::Access(access) => Ok(access),
            WorkspaceStoreAccessOutcome::Missing | WorkspaceStoreAccessOutcome::Deleting => {
                anyhow::bail!("workspace {} not found", workspace_id.0)
            }
        }
    }

    pub async fn workspace_access_outcome(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<WorkspaceStoreAccessOutcome> {
        loop {
            if self.is_workspace_delete_blocked(workspace_id).await {
                return Ok(WorkspaceStoreAccessOutcome::Deleting);
            }
            loop {
                {
                    let mut stores = self.workspace_stores.lock().await;
                    if let Some(entry) = stores.get_mut(&workspace_id) {
                        entry.touch();
                        return Ok(WorkspaceStoreAccessOutcome::Access(WorkspaceStoreAccess {
                            store: entry.store.with_lease_guard(
                                self.store_leases.acquire(workspace_id, entry.instance_id),
                            ),
                            kind: WorkspaceStoreAccessKind::Cached,
                        }));
                    }
                }
                if self.store_leases.has_pending_close_store(workspace_id) {
                    if let Some(reactivated) = self
                        .store_leases
                        .reactivate_pending_close_store(workspace_id)
                    {
                        let leased_store = reactivated.store.with_lease_guard(
                            self.store_leases
                                .acquire(workspace_id, reactivated.instance_id),
                        );
                        match self.global.get_workspace(workspace_id).await {
                            Ok(Some(_)) => {}
                            Ok(None) => {
                                self.store_leases.restore_pending_close_store(
                                    workspace_id,
                                    reactivated.instance_id,
                                    reactivated.store,
                                );
                                drop(leased_store);
                                self.store_leases
                                    .wait_for_workspace_close(workspace_id)
                                    .await;
                                continue;
                            }
                            Err(err) => {
                                self.store_leases.restore_pending_close_store(
                                    workspace_id,
                                    reactivated.instance_id,
                                    reactivated.store,
                                );
                                drop(leased_store);
                                return Err(err);
                            }
                        }
                        if self.is_workspace_delete_blocked(workspace_id).await {
                            self.store_leases.restore_pending_close_store(
                                workspace_id,
                                reactivated.instance_id,
                                reactivated.store,
                            );
                            drop(leased_store);
                            return Ok(WorkspaceStoreAccessOutcome::Deleting);
                        }
                        let mut stores = self.workspace_stores.lock().await;
                        if let Some(existing) = stores.get_mut(&workspace_id) {
                            existing.touch();
                            self.store_leases
                                .publish_reactivated_store(workspace_id, &reactivated.notify);
                            drop(leased_store);
                            return Ok(WorkspaceStoreAccessOutcome::Access(WorkspaceStoreAccess {
                                store: existing.store.with_lease_guard(
                                    self.store_leases
                                        .acquire(workspace_id, existing.instance_id),
                                ),
                                kind: WorkspaceStoreAccessKind::Cached,
                            }));
                        }
                        stores.insert(
                            workspace_id,
                            TimedStoreEntry::new(
                                workspace_id,
                                reactivated.store.clone(),
                                reactivated.instance_id,
                            ),
                        );
                        self.store_leases
                            .publish_reactivated_store(workspace_id, &reactivated.notify);
                        drop(stores);
                        return Ok(WorkspaceStoreAccessOutcome::Access(WorkspaceStoreAccess {
                            store: leased_store,
                            kind: WorkspaceStoreAccessKind::Reactivated,
                        }));
                    }
                    self.store_leases
                        .wait_for_workspace_close(workspace_id)
                        .await;
                    continue;
                }
                let is_closing = self.store_leases.is_workspace_closing(workspace_id);
                if !is_closing {
                    break;
                }
                self.store_leases
                    .wait_for_workspace_close(workspace_id)
                    .await;
            }
            let Some(workspace) = self.global.get_workspace(workspace_id).await? else {
                return Ok(WorkspaceStoreAccessOutcome::Missing);
            };
            let store = self.open_workspace_store(&workspace).await?;
            if self.is_workspace_delete_blocked(workspace_id).await {
                store.close().await;
                return Ok(WorkspaceStoreAccessOutcome::Deleting);
            }
            let instance_id = self.next_store_instance_id.fetch_add(1, Ordering::Relaxed);
            let mut stores = self.workspace_stores.lock().await;
            if self.store_leases.is_workspace_closing(workspace_id) {
                drop(stores);
                store.close().await;
                self.store_leases
                    .wait_for_workspace_close(workspace_id)
                    .await;
                continue;
            }
            if let Some(existing) = stores.get_mut(&workspace_id) {
                existing.touch();
                let existing = existing.store.with_lease_guard(
                    self.store_leases
                        .acquire(workspace_id, existing.instance_id),
                );
                drop(stores);
                store.close().await;
                return Ok(WorkspaceStoreAccessOutcome::Access(WorkspaceStoreAccess {
                    store: existing,
                    kind: WorkspaceStoreAccessKind::Cached,
                }));
            }
            let leased_store =
                store.with_lease_guard(self.store_leases.acquire(workspace_id, instance_id));
            stores.insert(
                workspace_id,
                TimedStoreEntry::new(workspace_id, store.clone(), instance_id),
            );
            drop(stores);
            return Ok(WorkspaceStoreAccessOutcome::Access(WorkspaceStoreAccess {
                store: leased_store,
                kind: WorkspaceStoreAccessKind::ColdOpen,
            }));
        }
    }

    pub async fn workspace_uncached(&self, workspace_id: WorkspaceId) -> Result<Store> {
        if self.is_workspace_delete_blocked(workspace_id).await {
            anyhow::bail!("workspace {} not found", workspace_id.0);
        }
        if self.store_leases.is_workspace_closing(workspace_id) {
            self.store_leases
                .wait_for_workspace_close(workspace_id)
                .await;
        }
        let workspace = self
            .global
            .get_workspace(workspace_id)
            .await?
            .with_context(|| format!("workspace {} not found", workspace_id.0))?;
        self.open_workspace_store(&workspace).await
    }

    pub async fn workspace_transient(&self, workspace_id: WorkspaceId) -> Result<Store> {
        match self.workspace_access_outcome(workspace_id).await? {
            WorkspaceStoreAccessOutcome::Access(access) => {
                // Transient callers want a handle that does not remain in the workspace cache.
                // If we reopened an already cached store, queue it for close immediately and let
                // the returned lease-backed handle keep it alive until the caller drops it.
                self.evict_workspace(workspace_id).await;
                Ok(access.store)
            }
            WorkspaceStoreAccessOutcome::Missing | WorkspaceStoreAccessOutcome::Deleting => {
                anyhow::bail!("workspace {} not found", workspace_id.0)
            }
        }
    }

    pub async fn store_for_task(&self, task_id: TaskId) -> Result<Store> {
        let workspace_id = self
            .global
            .get_workspace_id_for_task(task_id)
            .await?
            .with_context(|| format!("workspace missing for task {}", task_id.0))?;
        self.workspace(workspace_id).await
    }

    pub async fn store_for_session(&self, session_id: SessionId) -> Result<Store> {
        let workspace_id = self
            .global
            .get_workspace_id_for_session(session_id)
            .await?
            .with_context(|| format!("workspace missing for session {}", session_id.0))?;
        self.workspace(workspace_id).await
    }

    pub async fn store_for_worktree(&self, worktree_id: WorktreeId) -> Result<Store> {
        let workspace_id = self
            .global
            .get_workspace_id_for_worktree(worktree_id)
            .await?
            .with_context(|| format!("workspace missing for worktree {}", worktree_id.0))?;
        self.workspace(workspace_id).await
    }

    pub async fn evict_workspace(&self, workspace_id: WorkspaceId) {
        let store = {
            let mut stores = self.workspace_stores.lock().await;
            let close = stores.get(&workspace_id).and_then(|entry| {
                self.store_leases.queue_close(
                    entry.workspace_id,
                    entry.instance_id,
                    entry.store.clone(),
                )
            });
            stores.remove(&workspace_id);
            close
        };
        self.close_pending_entries(store.into_iter().collect());
    }

    pub async fn evict_workspace_and_wait_closed(&self, workspace_id: WorkspaceId) {
        self.evict_workspace(workspace_id).await;
        if self.store_leases.is_workspace_closing(workspace_id) {
            self.store_leases
                .wait_for_workspace_close(workspace_id)
                .await;
        }
    }

    pub async fn evict_idle_workspaces(
        &self,
        max_idle: Duration,
        active_workspaces: &HashSet<WorkspaceId>,
    ) -> usize {
        let now = Instant::now();
        let (evicted, expired_entries) = {
            let mut stores = self.workspace_stores.lock().await;
            let expired: Vec<WorkspaceId> = stores
                .iter()
                .filter_map(|(workspace_id, entry)| {
                    if active_workspaces.contains(workspace_id) {
                        return None;
                    }
                    if now.duration_since(entry.last_access) >= max_idle {
                        Some(*workspace_id)
                    } else {
                        None
                    }
                })
                .collect();
            let evicted = expired.len();
            let closes = expired
                .into_iter()
                .filter_map(|workspace_id| {
                    let close = stores.get(&workspace_id).and_then(|entry| {
                        self.store_leases.queue_close(
                            entry.workspace_id,
                            entry.instance_id,
                            entry.store.clone(),
                        )
                    });
                    stores.remove(&workspace_id);
                    close
                })
                .collect::<Vec<_>>();
            (evicted, closes)
        };
        self.close_pending_entries(expired_entries);
        evicted
    }

    pub async fn evict_workspaces_to_cap(
        &self,
        protected_workspaces: &HashSet<WorkspaceId>,
    ) -> usize {
        let max_cached = self.config.max_cached_workspaces.max(1);
        let (evicted, expired_entries) = {
            let mut stores = self.workspace_stores.lock().await;
            let overflow = stores.len().saturating_sub(max_cached);
            if overflow == 0 {
                (0, Vec::new())
            } else {
                let mut victims = stores
                    .iter()
                    .filter(|(workspace_id, _)| !protected_workspaces.contains(workspace_id))
                    .map(|(workspace_id, entry)| (*workspace_id, entry.last_access))
                    .collect::<Vec<_>>();
                victims.sort_by_key(|(_, last_access)| *last_access);
                let victims = victims.into_iter().take(overflow).collect::<Vec<_>>();
                let evicted = victims.len();
                let closes = victims
                    .into_iter()
                    .filter_map(|(workspace_id, _)| {
                        let close = stores.get(&workspace_id).and_then(|entry| {
                            self.store_leases.queue_close(
                                entry.workspace_id,
                                entry.instance_id,
                                entry.store.clone(),
                            )
                        });
                        stores.remove(&workspace_id);
                        close
                    })
                    .collect::<Vec<_>>();
                (evicted, closes)
            }
        };
        self.close_pending_entries(expired_entries);
        evicted
    }

    fn close_pending_entries(&self, entries: Vec<leases::PendingWorkspaceStoreClose>) {
        for close in entries {
            self.store_leases.start_close(close);
        }
    }

    async fn open_workspace_store(&self, workspace: &Workspace) -> Result<Store> {
        let path = self.workspace_db_path(workspace.id);
        if let Some(parent) = path.parent() {
            ensure_private_dir(parent).await?;
        }
        let workspace_max_connections = self
            .config
            .workspace_max_connections
            .or(self.config.max_connections)
            .or(Some(DEFAULT_WORKSPACE_MAX_CONNECTIONS));
        let store = Store::open_sqlite(&path, workspace_max_connections).await?;
        store.upsert_workspace(workspace).await?;
        Ok(store)
    }

    fn workspace_db_path(&self, workspace_id: WorkspaceId) -> PathBuf {
        self.data_root
            .join("db")
            .join("workspaces")
            .join(workspace_id.0.to_string())
            .join("db.sqlite")
    }
}

#[cfg(test)]
pub(crate) fn close_lifecycle_test_lock() -> &'static Arc<tokio::sync::Mutex<()>> {
    static LOCK: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
    LOCK.get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
}
