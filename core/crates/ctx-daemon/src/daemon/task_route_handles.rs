use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ctx_core::ids::{SessionId, TaskId, WorkspaceId};
use ctx_core::models::{ExecutionEnvironment, Session, Task, TaskDeltaKind, Workspace};
use ctx_observability::ops_events::OpsEvents;
use ctx_observability::perf_telemetry::PerfTelemetry;
use ctx_observability::telemetry::Telemetry;
use ctx_provider_runtime::ProviderRuntime;
use ctx_session_runtime::runtime::SessionRuntime;
use ctx_session_tools::model_resolution::ModelCatalog;
use ctx_store::Store;
use tokio::sync::mpsc;

use super::{
    provider_route_handles::ProviderStatusHandle,
    state::{ProtectedWorkspaceStoreLookup, TaskStoreLookup},
    workspaces::TaskWorktreeHost,
};

pub(in crate::daemon) type TaskAdmissionFuture<T> =
    Pin<Box<dyn Future<Output = T> + Send + 'static>>;
pub(in crate::daemon) type TaskLifecycleFuture<T> =
    Pin<Box<dyn Future<Output = T> + Send + 'static>>;
pub(in crate::daemon) type TaskMetadataFuture<T> =
    Pin<Box<dyn Future<Output = T> + Send + 'static>>;
pub(in crate::daemon) type TaskArchivedRevLoader =
    Arc<dyn Fn(WorkspaceId) -> TaskMetadataFuture<i64> + Send + Sync>;
pub(in crate::daemon) type TaskCloseWebSessionsForTask = Arc<
    dyn Fn(HashSet<String>, HashSet<String>) -> TaskMetadataFuture<anyhow::Result<usize>>
        + Send
        + Sync,
>;
pub(in crate::daemon) type TaskAdmissionModelCatalogLoader = Arc<
    dyn Fn(
            Workspace,
            String,
            ExecutionEnvironment,
        ) -> TaskAdmissionFuture<Result<Option<ModelCatalog>, String>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct TaskListingHandle {
    workspace_stores: ProtectedWorkspaceStoreLookup,
    archived_rev_loader: TaskArchivedRevLoader,
}

impl TaskListingHandle {
    pub(in crate::daemon) fn new(
        workspace_stores: ProtectedWorkspaceStoreLookup,
        archived_rev_loader: TaskArchivedRevLoader,
    ) -> Self {
        Self {
            workspace_stores,
            archived_rev_loader,
        }
    }

    pub(in crate::daemon) async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, crate::daemon::WorkspaceStoreAccessError> {
        self.workspace_stores
            .existing_workspace_store(workspace_id)
            .await
    }

    pub(in crate::daemon) async fn load_archived_rev(&self, workspace_id: WorkspaceId) -> i64 {
        (self.archived_rev_loader)(workspace_id).await
    }
}

#[derive(Clone)]
pub struct TaskSessionListingHandle {
    lookup: TaskStoreLookup,
}

impl TaskSessionListingHandle {
    pub(in crate::daemon) fn new(lookup: TaskStoreLookup) -> Self {
        Self { lookup }
    }

    pub(in crate::daemon) async fn task_store_or_none(
        &self,
        task_id: TaskId,
    ) -> anyhow::Result<Option<Store>> {
        self.lookup.task_store_or_none(task_id).await
    }
}

#[derive(Clone)]
pub struct TaskReadStateHandle {
    lookup: TaskStoreLookup,
    effects: Arc<TaskMetadataEffects>,
}

impl TaskReadStateHandle {
    pub(in crate::daemon) fn new(
        lookup: TaskStoreLookup,
        effects: Arc<TaskMetadataEffects>,
    ) -> Self {
        Self { lookup, effects }
    }

    pub(in crate::daemon) async fn task_store_or_none(
        &self,
        task_id: TaskId,
    ) -> anyhow::Result<Option<Store>> {
        self.lookup.task_store_or_none(task_id).await
    }

    pub(in crate::daemon) fn effects(&self) -> &TaskMetadataEffects {
        self.effects.as_ref()
    }

    #[cfg(test)]
    pub(in crate::daemon) fn with_effects_for_test(
        &self,
        effects: Arc<TaskMetadataEffects>,
    ) -> Self {
        Self {
            lookup: self.lookup.clone(),
            effects,
        }
    }
}

#[derive(Clone)]
pub struct TaskTitleHandle {
    lookup: TaskStoreLookup,
    effects: Arc<TaskMetadataEffects>,
    close_web_sessions_for_task: TaskCloseWebSessionsForTask,
}

impl TaskTitleHandle {
    pub(in crate::daemon) fn new(
        lookup: TaskStoreLookup,
        effects: Arc<TaskMetadataEffects>,
        close_web_sessions_for_task: TaskCloseWebSessionsForTask,
    ) -> Self {
        Self {
            lookup,
            effects,
            close_web_sessions_for_task,
        }
    }

    pub(in crate::daemon) async fn task_store_or_none(
        &self,
        task_id: TaskId,
    ) -> anyhow::Result<Option<Store>> {
        self.lookup.task_store_or_none(task_id).await
    }

    pub(in crate::daemon) fn effects(&self) -> &TaskMetadataEffects {
        self.effects.as_ref()
    }

    pub(in crate::daemon) async fn close_web_sessions_for_task(
        &self,
        session_ids: HashSet<String>,
        worktree_ids: HashSet<String>,
    ) -> anyhow::Result<usize> {
        (self.close_web_sessions_for_task)(session_ids, worktree_ids).await
    }

    #[cfg(test)]
    pub(in crate::daemon) fn with_effects_and_close_for_test(
        &self,
        effects: Arc<TaskMetadataEffects>,
        close_web_sessions_for_task: TaskCloseWebSessionsForTask,
    ) -> Self {
        Self {
            lookup: self.lookup.clone(),
            effects,
            close_web_sessions_for_task,
        }
    }
}

#[derive(Clone)]
pub struct TaskLifecycleHandle {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    workspace: Arc<TaskWorktreeHost>,
    effects: Arc<TaskLifecycleEffects>,
}

impl TaskLifecycleHandle {
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        workspace: Arc<TaskWorktreeHost>,
        effects: Arc<TaskLifecycleEffects>,
    ) -> Self {
        Self {
            global_store,
            workspace_stores,
            workspace,
            effects,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, crate::daemon::WorkspaceStoreAccessError> {
        self.workspace_stores
            .existing_workspace_store(workspace_id)
            .await
    }

    pub(in crate::daemon) fn workspace(&self) -> &Arc<TaskWorktreeHost> {
        &self.workspace
    }

    pub(in crate::daemon) fn effects(&self) -> &TaskLifecycleEffects {
        &self.effects
    }
}

#[derive(Clone)]
pub struct TaskCreationHandle {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    session_admission: TaskSessionAdmissionHandle,
    task_lifecycle: TaskLifecycleHandle,
}

impl TaskCreationHandle {
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        session_admission: TaskSessionAdmissionHandle,
        task_lifecycle: TaskLifecycleHandle,
    ) -> Self {
        Self {
            global_store,
            workspace_stores,
            session_admission,
            task_lifecycle,
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

    pub(in crate::daemon) fn session_admission(&self) -> &TaskSessionAdmissionHandle {
        &self.session_admission
    }

    pub(in crate::daemon) async fn delete_loaded_task_with_cleanup(
        &self,
        store: &Store,
        workspace: &Workspace,
        task: &Task,
    ) -> Result<(), crate::daemon::tasks::TaskLifecycleError> {
        self.task_lifecycle
            .delete_loaded_task_with_cleanup(store, workspace, task)
            .await
    }
}

#[derive(Clone)]
pub struct TaskSessionAdmissionHandle {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    sessions: Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
    providers: Arc<ProviderRuntime>,
    provider_status: ProviderStatusHandle,
    workspace: Arc<TaskWorktreeHost>,
    effects: Arc<TaskAdmissionSessionEffects>,
    model_catalog_loader: TaskAdmissionModelCatalogLoader,
    telemetry: Telemetry,
    ops_events: OpsEvents,
    perf_telemetry: PerfTelemetry,
}

impl TaskSessionAdmissionHandle {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        sessions: Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
        providers: Arc<ProviderRuntime>,
        provider_status: ProviderStatusHandle,
        workspace: Arc<TaskWorktreeHost>,
        effects: Arc<TaskAdmissionSessionEffects>,
        model_catalog_loader: TaskAdmissionModelCatalogLoader,
        telemetry: Telemetry,
        ops_events: OpsEvents,
        perf_telemetry: PerfTelemetry,
    ) -> Self {
        Self {
            global_store,
            workspace_stores,
            sessions,
            providers,
            provider_status,
            workspace,
            effects,
            model_catalog_loader,
            telemetry,
            ops_events,
            perf_telemetry,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) fn sessions(
        &self,
    ) -> &SessionRuntime<crate::daemon::scheduler::SchedulerCommand> {
        self.sessions.as_ref()
    }

    pub(in crate::daemon) fn providers(&self) -> &ProviderRuntime {
        self.providers.as_ref()
    }

    pub(in crate::daemon) fn provider_status(&self) -> &ProviderStatusHandle {
        &self.provider_status
    }

    pub(in crate::daemon) fn workspace(&self) -> &Arc<TaskWorktreeHost> {
        &self.workspace
    }

    pub(in crate::daemon) fn effects(&self) -> &TaskAdmissionSessionEffects {
        self.effects.as_ref()
    }

    pub(in crate::daemon) fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    pub(in crate::daemon) fn ops_events(&self) -> &OpsEvents {
        &self.ops_events
    }

    pub(in crate::daemon) fn perf_telemetry(&self) -> &PerfTelemetry {
        &self.perf_telemetry
    }

    pub(in crate::daemon) async fn store_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Store> {
        self.workspace_stores
            .store_for_workspace(workspace_id)
            .await
    }

    pub(in crate::daemon) async fn load_provider_model_catalog_for_execution_environment(
        &self,
        workspace: &Workspace,
        provider_id: &str,
        execution_environment: ExecutionEnvironment,
    ) -> Result<Option<ModelCatalog>, String> {
        (self.model_catalog_loader)(
            workspace.clone(),
            provider_id.to_string(),
            execution_environment,
        )
        .await
    }
}

pub(in crate::daemon) type TaskAdmissionTaskUpsertEffect =
    Arc<dyn Fn(TaskId) -> TaskAdmissionFuture<anyhow::Result<()>> + Send + Sync>;

pub(in crate::daemon) struct TaskMetadataEffects {
    emit_workspace_task_delta:
        Arc<dyn Fn(Task, TaskDeltaKind) -> TaskMetadataFuture<()> + Send + Sync>,
    emit_workspace_task_upsert:
        Arc<dyn Fn(TaskId) -> TaskMetadataFuture<anyhow::Result<()>> + Send + Sync>,
}

impl TaskMetadataEffects {
    pub(in crate::daemon) fn new(
        emit_workspace_task_delta: Arc<
            dyn Fn(Task, TaskDeltaKind) -> TaskMetadataFuture<()> + Send + Sync,
        >,
        emit_workspace_task_upsert: Arc<
            dyn Fn(TaskId) -> TaskMetadataFuture<anyhow::Result<()>> + Send + Sync,
        >,
    ) -> Arc<Self> {
        Arc::new(Self {
            emit_workspace_task_delta,
            emit_workspace_task_upsert,
        })
    }

    pub(in crate::daemon) async fn publish_task_updated(&self, task_id: TaskId, task: Task) {
        (self.emit_workspace_task_delta)(task, TaskDeltaKind::Updated).await;
        if let Err(error) = (self.emit_workspace_task_upsert)(task_id).await {
            tracing::warn!(task_id = %task_id.0, "workspace active snapshot refresh failed: {error:?}");
        }
    }
}

pub(in crate::daemon) struct TaskLifecycleEffects {
    cleanup_session: Arc<dyn Fn(SessionId) -> TaskLifecycleFuture<()> + Send + Sync>,
    emit_workspace_task_delta:
        Arc<dyn Fn(Task, TaskDeltaKind) -> TaskLifecycleFuture<()> + Send + Sync>,
    emit_workspace_task_upsert:
        Arc<dyn Fn(TaskId) -> TaskLifecycleFuture<anyhow::Result<()>> + Send + Sync>,
    remove_active_snapshot_session: Arc<dyn Fn(SessionId) -> TaskLifecycleFuture<()> + Send + Sync>,
    refresh_session_head_cache: Arc<dyn Fn(SessionId) -> TaskLifecycleFuture<()> + Send + Sync>,
    emit_workspace_archived_task_delete:
        Arc<dyn Fn(WorkspaceId, TaskId) -> TaskLifecycleFuture<()> + Send + Sync>,
    emit_workspace_task_delete:
        Arc<dyn Fn(WorkspaceId, TaskId) -> TaskLifecycleFuture<()> + Send + Sync>,
}

impl TaskLifecycleEffects {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::daemon) fn new(
        cleanup_session: Arc<dyn Fn(SessionId) -> TaskLifecycleFuture<()> + Send + Sync>,
        emit_workspace_task_delta: Arc<
            dyn Fn(Task, TaskDeltaKind) -> TaskLifecycleFuture<()> + Send + Sync,
        >,
        emit_workspace_task_upsert: Arc<
            dyn Fn(TaskId) -> TaskLifecycleFuture<anyhow::Result<()>> + Send + Sync,
        >,
        remove_active_snapshot_session: Arc<
            dyn Fn(SessionId) -> TaskLifecycleFuture<()> + Send + Sync,
        >,
        refresh_session_head_cache: Arc<dyn Fn(SessionId) -> TaskLifecycleFuture<()> + Send + Sync>,
        emit_workspace_archived_task_delete: Arc<
            dyn Fn(WorkspaceId, TaskId) -> TaskLifecycleFuture<()> + Send + Sync,
        >,
        emit_workspace_task_delete: Arc<
            dyn Fn(WorkspaceId, TaskId) -> TaskLifecycleFuture<()> + Send + Sync,
        >,
    ) -> Arc<Self> {
        Arc::new(Self {
            cleanup_session,
            emit_workspace_task_delta,
            emit_workspace_task_upsert,
            remove_active_snapshot_session,
            refresh_session_head_cache,
            emit_workspace_archived_task_delete,
            emit_workspace_task_delete,
        })
    }

    pub(in crate::daemon) async fn cleanup_session(&self, session_id: SessionId) {
        (self.cleanup_session)(session_id).await;
    }

    pub(in crate::daemon) async fn emit_workspace_task_delta(
        &self,
        task: Task,
        kind: TaskDeltaKind,
    ) {
        (self.emit_workspace_task_delta)(task, kind).await;
    }

    pub(in crate::daemon) async fn emit_workspace_task_upsert(
        &self,
        task_id: TaskId,
    ) -> anyhow::Result<()> {
        (self.emit_workspace_task_upsert)(task_id).await
    }

    pub(in crate::daemon) async fn remove_active_snapshot_session(&self, session_id: SessionId) {
        (self.remove_active_snapshot_session)(session_id).await;
    }

    pub(in crate::daemon) async fn refresh_session_head_cache(&self, session_id: SessionId) {
        (self.refresh_session_head_cache)(session_id).await;
    }

    pub(in crate::daemon) async fn emit_workspace_archived_task_delete(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
    ) {
        (self.emit_workspace_archived_task_delete)(workspace_id, task_id).await;
    }

    pub(in crate::daemon) async fn emit_workspace_task_delete(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
    ) {
        (self.emit_workspace_task_delete)(workspace_id, task_id).await;
    }
}

pub(in crate::daemon) struct TaskAdmissionSessionEffects {
    publish_event:
        Arc<dyn Fn(ctx_core::models::SessionEvent) -> TaskAdmissionFuture<()> + Send + Sync>,
    ensure_scheduler: Arc<
        dyn Fn(
                Session,
            )
                -> TaskAdmissionFuture<mpsc::Sender<crate::daemon::scheduler::SchedulerCommand>>
            + Send
            + Sync,
    >,
    schedule_title_generation:
        Arc<dyn Fn(Session, String, bool) -> TaskAdmissionFuture<bool> + Send + Sync>,
    emit_workspace_task_upsert: TaskAdmissionTaskUpsertEffect,
}

impl TaskAdmissionSessionEffects {
    pub(in crate::daemon) fn new(
        publish_event: Arc<
            dyn Fn(ctx_core::models::SessionEvent) -> TaskAdmissionFuture<()> + Send + Sync,
        >,
        ensure_scheduler: Arc<
            dyn Fn(
                    Session,
                )
                    -> TaskAdmissionFuture<mpsc::Sender<crate::daemon::scheduler::SchedulerCommand>>
                + Send
                + Sync,
        >,
        schedule_title_generation: Arc<
            dyn Fn(Session, String, bool) -> TaskAdmissionFuture<bool> + Send + Sync,
        >,
        emit_workspace_task_upsert: TaskAdmissionTaskUpsertEffect,
    ) -> Arc<Self> {
        Arc::new(Self {
            publish_event,
            ensure_scheduler,
            schedule_title_generation,
            emit_workspace_task_upsert,
        })
    }

    pub(in crate::daemon) async fn publish_event(&self, event: ctx_core::models::SessionEvent) {
        (self.publish_event)(event).await;
    }

    pub(in crate::daemon) async fn ensure_scheduler(
        &self,
        session: Session,
    ) -> mpsc::Sender<crate::daemon::scheduler::SchedulerCommand> {
        (self.ensure_scheduler)(session).await
    }

    pub(in crate::daemon) async fn schedule_title_generation(
        &self,
        session: Session,
        prompt: String,
        force: bool,
    ) -> bool {
        (self.schedule_title_generation)(session, prompt, force).await
    }

    pub(in crate::daemon) async fn emit_workspace_task_upsert(
        &self,
        task_id: TaskId,
    ) -> anyhow::Result<()> {
        (self.emit_workspace_task_upsert)(task_id).await
    }
}
