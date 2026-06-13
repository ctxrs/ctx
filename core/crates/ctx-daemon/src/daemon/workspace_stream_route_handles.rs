use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ctx_core::ids::{SessionId, WorkspaceId, WorktreeId};
use ctx_core::models::{WorkspaceActiveHeadBatch, WorkspaceActiveSnapshot, Worktree};
use ctx_observability::perf_telemetry::PerfTelemetry;
use ctx_observability::telemetry::Telemetry;
use ctx_provider_runtime::ProviderRuntime;
use ctx_session_runtime::runtime::{SessionLifecycleHost, SessionRuntime};
use ctx_store::Store;
use ctx_workspace_active_snapshot::WorkspaceActiveSnapshotHub;

use super::state::{ProtectedWorkspaceStoreLookup, SessionStoreLookup};

#[cfg(test)]
use tokio::sync::Mutex;

pub(in crate::daemon) type WorkspaceActiveFuture<T> =
    Pin<Box<dyn Future<Output = T> + Send + 'static>>;
pub(in crate::daemon) type WorkspaceActiveHydrationEffect = Arc<
    dyn Fn(
            WorkspaceId,
        )
            -> WorkspaceActiveFuture<Result<(), crate::daemon::workspaces::WorkspaceHydrationError>>
        + Send
        + Sync,
>;
pub(in crate::daemon) type WorkspaceActiveUnitEffect =
    Arc<dyn Fn(WorkspaceId) -> WorkspaceActiveFuture<()> + Send + Sync>;
pub(in crate::daemon) type WorkspaceActiveSnapshotCacheEffect =
    Arc<dyn Fn(WorkspaceActiveSnapshot) -> WorkspaceActiveFuture<()> + Send + Sync>;
pub(in crate::daemon) type WorkspaceActiveHeadsCacheEffect =
    Arc<dyn Fn(WorkspaceActiveHeadBatch) -> WorkspaceActiveFuture<()> + Send + Sync>;

pub(in crate::daemon) struct WorkspaceActiveEffectsParts {
    pub(in crate::daemon) ensure_workspace_active_snapshot_hydrated: WorkspaceActiveHydrationEffect,
    pub(in crate::daemon) activate_workspace_merge_queue: WorkspaceActiveUnitEffect,
    pub(in crate::daemon) cache_workspace_active_snapshot: WorkspaceActiveSnapshotCacheEffect,
    pub(in crate::daemon) cache_workspace_active_heads: WorkspaceActiveHeadsCacheEffect,
}

pub(in crate::daemon) struct WorkspaceActiveEffects {
    ensure_workspace_active_snapshot_hydrated: WorkspaceActiveHydrationEffect,
    activate_workspace_merge_queue: WorkspaceActiveUnitEffect,
    cache_workspace_active_snapshot: WorkspaceActiveSnapshotCacheEffect,
    cache_workspace_active_heads: WorkspaceActiveHeadsCacheEffect,
}

impl WorkspaceActiveEffects {
    pub(in crate::daemon) fn new(parts: WorkspaceActiveEffectsParts) -> Arc<Self> {
        Arc::new(Self {
            ensure_workspace_active_snapshot_hydrated: parts
                .ensure_workspace_active_snapshot_hydrated,
            activate_workspace_merge_queue: parts.activate_workspace_merge_queue,
            cache_workspace_active_snapshot: parts.cache_workspace_active_snapshot,
            cache_workspace_active_heads: parts.cache_workspace_active_heads,
        })
    }
}

#[derive(Clone)]
pub struct WorkspaceActiveHandle {
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    effects: Arc<WorkspaceActiveEffects>,
}

pub(in crate::daemon) struct WorkspaceActiveHandleParts {
    pub(in crate::daemon) active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    pub(in crate::daemon) effects: Arc<WorkspaceActiveEffects>,
}

impl WorkspaceActiveHandle {
    pub(in crate::daemon) fn new(parts: WorkspaceActiveHandleParts) -> Self {
        Self {
            active_snapshot: parts.active_snapshot,
            effects: parts.effects,
        }
    }

    pub(in crate::daemon) async fn load_workspace_active_snapshot(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<WorkspaceActiveSnapshot, crate::daemon::workspaces::WorkspaceHydrationError> {
        (self.effects.ensure_workspace_active_snapshot_hydrated)(workspace_id).await?;
        (self.effects.activate_workspace_merge_queue)(workspace_id).await;
        let snapshot = self
            .active_snapshot
            .active_snapshot(workspace_id, i64::MAX)
            .await;
        (self.effects.cache_workspace_active_snapshot)(snapshot.clone()).await;
        Ok(snapshot)
    }

    pub(in crate::daemon) async fn load_workspace_active_heads(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<WorkspaceActiveHeadBatch, crate::daemon::workspaces::WorkspaceHydrationError> {
        (self.effects.ensure_workspace_active_snapshot_hydrated)(workspace_id).await?;
        (self.effects.activate_workspace_merge_queue)(workspace_id).await;
        let heads = self.active_snapshot.active_heads(workspace_id).await;
        (self.effects.cache_workspace_active_heads)(heads.clone()).await;
        Ok(heads)
    }
}

#[cfg(test)]
mod workspace_active_tests {
    use super::*;
    use ctx_route_contracts::workspaces::{WorkspaceRouteErrorKind, WorkspaceRouteParams};

    fn workspace_active_test_handle(
        events: Arc<Mutex<Vec<&'static str>>>,
        hydrate_succeeds: bool,
    ) -> WorkspaceActiveHandle {
        let ensure_workspace_active_snapshot_hydrated = Arc::new({
            let events = Arc::clone(&events);
            move |_workspace_id: WorkspaceId| {
                let events = Arc::clone(&events);
                Box::pin(async move {
                    events.lock().await.push("hydrate");
                    if hydrate_succeeds {
                        Ok(())
                    } else {
                        Err(crate::daemon::workspaces::WorkspaceHydrationError::NotFound)
                    }
                }) as WorkspaceActiveFuture<_>
            }
        })
            as WorkspaceActiveHydrationEffect;
        let activate_workspace_merge_queue = Arc::new({
            let events = Arc::clone(&events);
            move |_workspace_id: WorkspaceId| {
                let events = Arc::clone(&events);
                Box::pin(async move {
                    events.lock().await.push("activate_merge_queue");
                }) as WorkspaceActiveFuture<_>
            }
        }) as WorkspaceActiveUnitEffect;
        let cache_workspace_active_snapshot = Arc::new({
            let events = Arc::clone(&events);
            move |_snapshot: WorkspaceActiveSnapshot| {
                let events = Arc::clone(&events);
                Box::pin(async move {
                    events.lock().await.push("cache_snapshot");
                }) as WorkspaceActiveFuture<_>
            }
        }) as WorkspaceActiveSnapshotCacheEffect;
        let cache_workspace_active_heads = Arc::new({
            let events = Arc::clone(&events);
            move |_heads: WorkspaceActiveHeadBatch| {
                let events = Arc::clone(&events);
                Box::pin(async move {
                    events.lock().await.push("cache_heads");
                }) as WorkspaceActiveFuture<_>
            }
        }) as WorkspaceActiveHeadsCacheEffect;

        WorkspaceActiveHandle::new(WorkspaceActiveHandleParts {
            active_snapshot: Arc::new(WorkspaceActiveSnapshotHub::new()),
            effects: WorkspaceActiveEffects::new(WorkspaceActiveEffectsParts {
                ensure_workspace_active_snapshot_hydrated,
                activate_workspace_merge_queue,
                cache_workspace_active_snapshot,
                cache_workspace_active_heads,
            }),
        })
    }

    #[tokio::test]
    async fn workspace_active_snapshot_for_route_rejects_invalid_workspace_id() {
        let handle = workspace_active_test_handle(Arc::new(Mutex::new(Vec::new())), true);
        let error = handle
            .workspace_active_snapshot_for_route(WorkspaceRouteParams::new("not-a-workspace"))
            .await
            .unwrap_err();

        assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid workspace id");
    }

    #[tokio::test]
    async fn workspace_active_heads_for_route_rejects_invalid_workspace_id() {
        let handle = workspace_active_test_handle(Arc::new(Mutex::new(Vec::new())), true);
        let error = handle
            .workspace_active_heads_for_route(WorkspaceRouteParams::new("not-a-workspace"))
            .await
            .unwrap_err();

        assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid workspace id");
    }

    #[tokio::test]
    async fn workspace_active_snapshot_handle_preserves_effect_order() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let handle = workspace_active_test_handle(Arc::clone(&events), true);
        let workspace_id = WorkspaceId::new();

        let snapshot = handle
            .load_workspace_active_snapshot(workspace_id)
            .await
            .expect("active snapshot");

        assert_eq!(snapshot.workspace_id, workspace_id);
        assert_eq!(
            *events.lock().await,
            ["hydrate", "activate_merge_queue", "cache_snapshot"]
        );
    }

    #[tokio::test]
    async fn workspace_active_heads_handle_preserves_effect_order() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let handle = workspace_active_test_handle(Arc::clone(&events), true);
        let workspace_id = WorkspaceId::new();

        let heads = handle
            .load_workspace_active_heads(workspace_id)
            .await
            .expect("active heads");

        assert_eq!(heads.workspace_id, workspace_id);
        assert_eq!(
            *events.lock().await,
            ["hydrate", "activate_merge_queue", "cache_heads"]
        );
    }

    #[tokio::test]
    async fn workspace_active_snapshot_handle_short_circuits_after_hydration_failure() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let handle = workspace_active_test_handle(Arc::clone(&events), false);
        let error = handle
            .load_workspace_active_snapshot(WorkspaceId::new())
            .await
            .unwrap_err();

        assert_eq!(
            error.kind(),
            crate::daemon::workspaces::WorkspaceHydrationErrorKind::NotFound
        );
        assert_eq!(*events.lock().await, ["hydrate"]);
    }

    #[tokio::test]
    async fn workspace_active_heads_handle_short_circuits_after_hydration_failure() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let handle = workspace_active_test_handle(Arc::clone(&events), false);
        let error = handle
            .load_workspace_active_heads(WorkspaceId::new())
            .await
            .unwrap_err();

        assert_eq!(
            error.kind(),
            crate::daemon::workspaces::WorkspaceHydrationErrorKind::NotFound
        );
        assert_eq!(*events.lock().await, ["hydrate"]);
    }
}

pub(in crate::daemon) type WorkspaceStreamFuture<T> =
    Pin<Box<dyn Future<Output = T> + Send + 'static>>;
pub(in crate::daemon) type WorkspaceStreamHydrationEffect = Arc<
    dyn Fn(
            WorkspaceId,
        )
            -> WorkspaceStreamFuture<Result<(), crate::daemon::workspaces::WorkspaceHydrationError>>
        + Send
        + Sync,
>;
pub(in crate::daemon) type WorkspaceStreamUnitEffect =
    Arc<dyn Fn(WorkspaceId) -> WorkspaceStreamFuture<()> + Send + Sync>;

pub(in crate::daemon) struct WorkspaceStreamEffectsParts {
    pub(in crate::daemon) ensure_workspace_active_snapshot_hydrated: WorkspaceStreamHydrationEffect,
    pub(in crate::daemon) activate_workspace_merge_queue: WorkspaceStreamUnitEffect,
}

pub(in crate::daemon) struct WorkspaceStreamEffects {
    ensure_workspace_active_snapshot_hydrated: WorkspaceStreamHydrationEffect,
    activate_workspace_merge_queue: WorkspaceStreamUnitEffect,
}

impl WorkspaceStreamEffects {
    pub(in crate::daemon) fn new(parts: WorkspaceStreamEffectsParts) -> Arc<Self> {
        Arc::new(Self {
            ensure_workspace_active_snapshot_hydrated: parts
                .ensure_workspace_active_snapshot_hydrated,
            activate_workspace_merge_queue: parts.activate_workspace_merge_queue,
        })
    }
}

#[derive(Clone)]
pub struct WorkspaceStreamHandle {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    session_stores: SessionStoreLookup,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    sessions: Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
    lifecycle_host: Arc<WorkspaceStreamSessionLifecycleHost>,
    telemetry: Telemetry,
    perf_telemetry: PerfTelemetry,
    effects: Arc<WorkspaceStreamEffects>,
}

pub(in crate::daemon) struct WorkspaceStreamHandleParts {
    pub(in crate::daemon) global_store: Store,
    pub(in crate::daemon) workspace_stores: ProtectedWorkspaceStoreLookup,
    pub(in crate::daemon) session_stores: SessionStoreLookup,
    pub(in crate::daemon) active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    pub(in crate::daemon) sessions: Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
    pub(in crate::daemon) lifecycle_host: Arc<WorkspaceStreamSessionLifecycleHost>,
    pub(in crate::daemon) telemetry: Telemetry,
    pub(in crate::daemon) perf_telemetry: PerfTelemetry,
    pub(in crate::daemon) effects: Arc<WorkspaceStreamEffects>,
}

impl WorkspaceStreamHandle {
    pub(in crate::daemon) fn new(parts: WorkspaceStreamHandleParts) -> Self {
        Self {
            global_store: parts.global_store,
            workspace_stores: parts.workspace_stores,
            session_stores: parts.session_stores,
            active_snapshot: parts.active_snapshot,
            sessions: parts.sessions,
            lifecycle_host: parts.lifecycle_host,
            telemetry: parts.telemetry,
            perf_telemetry: parts.perf_telemetry,
            effects: parts.effects,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) async fn store_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Store> {
        self.workspace_stores
            .store_for_workspace(workspace_id)
            .await
    }

    pub(in crate::daemon) async fn session_store_allow_archived(
        &self,
        session_id: SessionId,
    ) -> Result<Store, crate::daemon::SessionStoreAccessError> {
        self.session_stores
            .existing_session_store_allow_archived(session_id)
            .await
    }

    pub(in crate::daemon) fn active_snapshot(&self) -> &WorkspaceActiveSnapshotHub {
        &self.active_snapshot
    }

    pub(in crate::daemon) fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    pub(in crate::daemon) fn perf_telemetry(&self) -> &PerfTelemetry {
        &self.perf_telemetry
    }

    pub(in crate::daemon) async fn attach_workspace_stream_session_pin(
        &self,
        session_id: SessionId,
    ) {
        self.sessions
            .attach_session_with_host(self.lifecycle_host.as_ref(), session_id)
            .await;
    }

    pub(in crate::daemon) async fn detach_workspace_stream_session_pin(
        &self,
        session_id: SessionId,
    ) {
        self.sessions
            .detach_session_with_host(self.lifecycle_host.as_ref(), session_id)
            .await;
    }

    pub(crate) async fn ensure_workspace_active_snapshot_hydrated(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<(), crate::daemon::workspaces::WorkspaceHydrationError> {
        (self.effects.ensure_workspace_active_snapshot_hydrated)(workspace_id).await
    }

    pub(in crate::daemon) async fn activate_workspace_merge_queue(
        &self,
        workspace_id: WorkspaceId,
    ) {
        (self.effects.activate_workspace_merge_queue)(workspace_id).await
    }
}

pub(in crate::daemon) type WorkspaceVcsStreamRefreshFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;
pub(in crate::daemon) type WorkspaceVcsStreamRefreshEffect =
    Arc<dyn Fn(Worktree, bool, bool) -> WorkspaceVcsStreamRefreshFuture + Send + Sync>;
pub(in crate::daemon) type WorkspaceVcsStreamWatcherFuture =
    Pin<Box<dyn Future<Output = ()> + Send>>;
pub(in crate::daemon) type WorkspaceVcsStreamWatcherEffect =
    Arc<dyn Fn(Worktree) -> WorkspaceVcsStreamWatcherFuture + Send + Sync>;

#[derive(Clone)]
pub struct WorkspaceVcsStreamHandle {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    runtime: crate::daemon::workspaces::stream::WorkspaceVcsStreamRuntime,
    perf_telemetry: PerfTelemetry,
    ensure_worktree_vcs_watcher: WorkspaceVcsStreamWatcherEffect,
    refresh_worktree_vcs: WorkspaceVcsStreamRefreshEffect,
}

impl WorkspaceVcsStreamHandle {
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        runtime: crate::daemon::workspaces::stream::WorkspaceVcsStreamRuntime,
        perf_telemetry: PerfTelemetry,
        ensure_worktree_vcs_watcher: WorkspaceVcsStreamWatcherEffect,
        refresh_worktree_vcs: WorkspaceVcsStreamRefreshEffect,
    ) -> Self {
        Self {
            global_store,
            workspace_stores,
            runtime,
            perf_telemetry,
            ensure_worktree_vcs_watcher,
            refresh_worktree_vcs,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) async fn store_for_worktree(
        &self,
        worktree_id: WorktreeId,
    ) -> anyhow::Result<Store> {
        self.workspace_stores.store_for_worktree(worktree_id).await
    }

    pub(in crate::daemon) fn runtime(
        &self,
    ) -> &crate::daemon::workspaces::stream::WorkspaceVcsStreamRuntime {
        &self.runtime
    }

    pub(in crate::daemon) fn perf_telemetry(&self) -> &PerfTelemetry {
        &self.perf_telemetry
    }

    pub(in crate::daemon) async fn ensure_loaded_worktree_vcs_watcher(&self, worktree: Worktree) {
        (self.ensure_worktree_vcs_watcher)(worktree).await
    }

    pub(in crate::daemon) async fn refresh_loaded_worktree_vcs(
        &self,
        worktree: Worktree,
        summary: bool,
        touched_files: bool,
    ) -> anyhow::Result<()> {
        (self.refresh_worktree_vcs)(worktree, summary, touched_files).await
    }
}

pub(in crate::daemon) struct WorkspaceStreamSessionLifecycleHost {
    global_store: Store,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    providers: Arc<ProviderRuntime>,
}

impl WorkspaceStreamSessionLifecycleHost {
    pub(in crate::daemon) fn new(
        global_store: Store,
        active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
        providers: Arc<ProviderRuntime>,
    ) -> Self {
        Self {
            global_store,
            active_snapshot,
            providers,
        }
    }
}

#[async_trait::async_trait]
impl SessionLifecycleHost for WorkspaceStreamSessionLifecycleHost {
    async fn set_provider_session_pinned(&self, session_id: SessionId, pinned: bool) {
        self.providers
            .set_provider_session_pinned(session_id.0.to_string(), pinned)
            .await;
    }

    async fn remove_workspace_active_session(&self, session_id: SessionId) {
        let workspace_id = self
            .global_store
            .get_workspace_id_for_session(session_id)
            .await
            .ok()
            .flatten();
        if let Some(workspace_id) = workspace_id {
            self.active_snapshot
                .remove_session_with_workspace_hint(workspace_id, session_id)
                .await;
        } else {
            self.active_snapshot.remove_session(session_id).await;
        }
    }
}
