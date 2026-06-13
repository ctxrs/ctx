use std::sync::Arc;

use anyhow::{Context, Result};
use ctx_core::ids::{SessionId, TaskId, TurnId, WorkspaceId};
use ctx_core::models::{
    Session, SessionEvent, SessionHeadDelta, SessionHeadSnapshot, SessionSummaryDelta, SessionTurn,
    SessionTurnToolSummary, Task, TaskDeltaKind,
};
use ctx_provider_runtime::ProviderRuntime;
use ctx_session_runtime::runtime::{
    SessionEventPublicationHost, SessionHeadRefreshHost, SessionHeadRefreshLoad,
    SessionLifecycleHost, SessionReplayCursor, SessionRuntime, SessionTaskDeltaRefreshHost,
};
use ctx_store::Store;
use ctx_workspace_active_snapshot::WorkspaceActiveSnapshotHub;

use super::merge_queue_route_handles::MergeQueueNoticeSessionEvent;
use super::session_route_handles::{SessionArtifactEffects, SessionArtifactsFuture};
use super::task_route_handles::{
    TaskAdmissionFuture, TaskAdmissionSessionEffects, TaskLifecycleEffects, TaskLifecycleFuture,
    TaskMetadataEffects, TaskMetadataFuture,
};
use super::{session_store_access_anyhow, ProtectedWorkspaceStoreLookup, SessionStoreLookup};

#[derive(Clone)]
pub(crate) struct SessionPublicationEffects {
    session_runtime: Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
    host: RouteSessionPublicationHost,
}

impl SessionPublicationEffects {
    pub(in crate::daemon) fn new(
        session_runtime: Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
        session_stores: SessionStoreLookup,
        task_publication: Arc<TaskPublicationHost>,
    ) -> Self {
        Self {
            session_runtime,
            host: RouteSessionPublicationHost::new(session_stores, task_publication),
        }
    }

    pub(crate) async fn publish_event(&self, event: SessionEvent) {
        self.session_runtime
            .publish_event_with_host(&self.host, event)
            .await;
    }

    pub(in crate::daemon) async fn publish_merge_queue_notice(
        &self,
        notice: MergeQueueNoticeSessionEvent,
    ) -> Result<()> {
        self.publish_event(notice.into_event()).await;
        Ok(())
    }

    pub(in crate::daemon) fn session_artifact_effects(&self) -> Arc<SessionArtifactEffects> {
        let publisher = self.clone();
        SessionArtifactEffects::new(Arc::new(move |event: SessionEvent| {
            let publisher = publisher.clone();
            Box::pin(async move { publisher.publish_event(event).await })
                as SessionArtifactsFuture<_>
        }))
    }
}

#[derive(Clone)]
struct RouteSessionPublicationHost {
    session_stores: SessionStoreLookup,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    task_delta_refresh_host: Arc<TaskPublicationHost>,
}

impl RouteSessionPublicationHost {
    fn new(session_stores: SessionStoreLookup, task_publication: Arc<TaskPublicationHost>) -> Self {
        Self {
            session_stores,
            active_snapshot: task_publication.active_snapshot(),
            task_delta_refresh_host: task_publication,
        }
    }

    async fn store_for_session(&self, session_id: SessionId) -> Result<Store> {
        self.session_stores
            .existing_session_store(session_id)
            .await
            .map_err(session_store_access_anyhow)
    }
}

#[async_trait::async_trait]
impl SessionEventPublicationHost for RouteSessionPublicationHost {
    type TaskDeltaRefreshHost = TaskPublicationHost;

    fn task_delta_refresh_host(&self) -> Arc<Self::TaskDeltaRefreshHost> {
        Arc::clone(&self.task_delta_refresh_host)
    }

    async fn load_session(&self, session_id: SessionId) -> Option<Session> {
        let store = self.store_for_session(session_id).await.ok()?;
        store.get_session(session_id).await.ok().flatten()
    }

    async fn list_turn_tool_summaries_for_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Vec<SessionTurnToolSummary> {
        let Ok(store) = self.store_for_session(session_id).await else {
            return Vec::new();
        };
        store
            .list_turn_tool_summaries_for_turns(session_id, std::slice::from_ref(&turn_id))
            .await
            .unwrap_or_default()
    }

    async fn cached_turn_for_read(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Option<SessionTurn> {
        self.active_snapshot
            .get_cached_session_head_for_read(session_id)
            .await
            .and_then(|head| head.turns.into_iter().find(|turn| turn.turn_id == turn_id))
    }

    async fn load_turn(&self, session_id: SessionId, turn_id: TurnId) -> Option<SessionTurn> {
        let store = self.store_for_session(session_id).await.ok()?;
        store
            .get_session_turn(session_id, turn_id)
            .await
            .ok()
            .flatten()
    }

    async fn session_replay_cursor(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
    ) -> SessionReplayCursor {
        let cursor = self
            .active_snapshot
            .session_replay_cursor(workspace_id, session_id)
            .await;
        SessionReplayCursor {
            last_event_seq: cursor.last_event_seq,
            projection_rev: cursor.projection_rev,
        }
    }

    async fn load_projection_rev(&self, session_id: SessionId) -> Option<i64> {
        let store = self.store_for_session(session_id).await.ok()?;
        store.get_session_projection_rev(session_id).await.ok()
    }

    async fn publish_session_head_delta(
        &self,
        workspace_id: WorkspaceId,
        session: &Session,
        delta: SessionHeadDelta,
        durable: bool,
    ) {
        self.active_snapshot
            .publish_session_head_delta(workspace_id, session, delta, durable)
            .await;
    }

    async fn publish_session_summary_delta(
        &self,
        workspace_id: WorkspaceId,
        delta: SessionSummaryDelta,
    ) {
        self.active_snapshot
            .publish_session_summary_delta(workspace_id, delta)
            .await;
    }
}

pub(crate) struct TaskPublicationHost {
    workspace_stores: ProtectedWorkspaceStoreLookup,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
}

impl TaskPublicationHost {
    pub(in crate::daemon) fn new(
        workspace_stores: ProtectedWorkspaceStoreLookup,
        active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    ) -> Self {
        Self {
            workspace_stores,
            active_snapshot,
        }
    }

    fn active_snapshot(&self) -> Arc<WorkspaceActiveSnapshotHub> {
        Arc::clone(&self.active_snapshot)
    }

    pub(crate) async fn emit_workspace_task_delta(&self, task: Task, kind: TaskDeltaKind) {
        let _ = self
            .active_snapshot
            .publish_task_delta(task.workspace_id, task, kind)
            .await;
    }

    pub(crate) async fn emit_workspace_task_upsert(&self, task_id: TaskId) -> Result<()> {
        let mut task: Option<Task> = None;
        let store = self.workspace_stores.store_for_task(task_id).await?;
        match store.get_workspace_active_task_summary(task_id).await? {
            Some(summary) => {
                let workspace_id = summary.task.workspace_id;
                task = Some(summary.task.clone());
                self.active_snapshot
                    .publish_active_task_upsert(workspace_id, summary)
                    .await;
            }
            None => {
                if let Some(loaded) = store.get_task(task_id).await? {
                    task = Some(loaded.clone());
                    self.active_snapshot
                        .publish_active_task_delete(loaded.workspace_id, task_id)
                        .await;
                }
            }
        }

        if let Some(task) = task.as_ref().filter(|task| task.archived_at.is_some()) {
            self.emit_workspace_archived_task_upsert(task).await?;
        }
        Ok(())
    }

    async fn emit_workspace_archived_task_upsert(&self, task: &Task) -> Result<()> {
        let store = self.workspace_stores.store_for_task(task.id).await?;
        let Some(summary) = store.get_workspace_task_summary(task.id).await? else {
            return Ok(());
        };
        if summary.task.archived_at.is_none() {
            return Ok(());
        }

        let _ = store
            .bump_workspace_archived_snapshot_rev(task.workspace_id)
            .await?;
        self.active_snapshot
            .publish_archived_task_upsert(task.workspace_id, summary)
            .await;
        Ok(())
    }

    pub(crate) async fn emit_workspace_archived_task_delete(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
    ) {
        let updated = match self
            .workspace_stores
            .store_for_workspace(workspace_id)
            .await
        {
            Ok(store) => match store
                .bump_workspace_archived_snapshot_rev(workspace_id)
                .await
            {
                Ok(_) => true,
                Err(err) => {
                    tracing::warn!(
                        workspace_id = %workspace_id.0,
                        task_id = %task_id.0,
                        "workspace archived delete read model update failed: {err:#}"
                    );
                    false
                }
            },
            Err(err) => {
                tracing::warn!(
                    workspace_id = %workspace_id.0,
                    task_id = %task_id.0,
                    "workspace archived delete read model store missing: {err:#}"
                );
                false
            }
        };
        if updated {
            self.active_snapshot
                .publish_archived_task_delete(workspace_id, task_id)
                .await;
        }
    }

    pub(crate) async fn emit_workspace_task_delete(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
    ) {
        if let Err(err) = self
            .workspace_stores
            .store_for_workspace(workspace_id)
            .await
        {
            tracing::warn!(
                workspace_id = %workspace_id.0,
                task_id = %task_id.0,
                "workspace task delete store missing: {err:#}"
            );
            return;
        }
        self.active_snapshot
            .publish_active_task_delete(workspace_id, task_id)
            .await;
    }
}

#[async_trait::async_trait]
impl SessionTaskDeltaRefreshHost for TaskPublicationHost {
    async fn emit_task_delta_refresh(&self, task_id: TaskId) {
        let store = match self.workspace_stores.store_for_task(task_id).await {
            Ok(store) => store,
            Err(err) => {
                tracing::warn!(
                    task_id = %task_id.0,
                    "route task delta refresh store lookup failed: {err:?}"
                );
                return;
            }
        };
        match store.get_workspace_active_task_summary(task_id).await {
            Ok(Some(summary)) => {
                self.emit_workspace_task_delta(summary.task, TaskDeltaKind::Updated)
                    .await;
            }
            Ok(None) => match store.get_task(task_id).await {
                Ok(Some(task)) => {
                    let kind = if task.archived_at.is_some() {
                        TaskDeltaKind::Archived
                    } else {
                        TaskDeltaKind::Updated
                    };
                    self.emit_workspace_task_delta(task, kind).await;
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(
                        task_id = %task_id.0,
                        "route task delta refresh task load failed: {err:?}"
                    );
                }
            },
            Err(err) => {
                tracing::warn!(
                    task_id = %task_id.0,
                    "route task delta refresh summary load failed: {err:?}"
                );
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct TaskSessionCleanupHost {
    global_store: Store,
    session_runtime: Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
    providers: Arc<ProviderRuntime>,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    workspace_stores: ProtectedWorkspaceStoreLookup,
}

impl TaskSessionCleanupHost {
    pub(in crate::daemon) fn new(
        global_store: Store,
        session_runtime: Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
        providers: Arc<ProviderRuntime>,
        active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
        workspace_stores: ProtectedWorkspaceStoreLookup,
    ) -> Self {
        Self {
            global_store,
            session_runtime,
            providers,
            active_snapshot,
            workspace_stores,
        }
    }

    pub(crate) async fn set_running(&self, session_id: SessionId, running: bool) {
        self.session_runtime
            .set_running_with_host(self, session_id, running)
            .await;
    }

    #[cfg(test)]
    pub(crate) async fn attach_session(&self, session_id: SessionId) {
        self.session_runtime
            .attach_session_with_host(self, session_id)
            .await;
    }

    #[cfg(test)]
    pub(crate) async fn detach_session(&self, session_id: SessionId) {
        self.session_runtime
            .detach_session_with_host(self, session_id)
            .await;
    }

    pub(crate) async fn cleanup_session(&self, session_id: SessionId) {
        self.session_runtime
            .cleanup_session_with_host(self, session_id)
            .await;
    }

    pub(in crate::daemon) async fn remove_active_snapshot_session(&self, session_id: SessionId) {
        self.active_snapshot.remove_session(session_id).await;
    }

    pub(crate) async fn refresh_session_head_cache(&self, session_id: SessionId) {
        self.session_runtime
            .refresh_session_head_cache_with_host(self, session_id)
            .await;
    }

    async fn store_for_session(&self, session_id: SessionId) -> Result<Store> {
        let workspace_id = self
            .global_store
            .get_workspace_id_for_session(session_id)
            .await?
            .with_context(|| format!("workspace missing for session {}", session_id.0))?;
        self.workspace_stores
            .store_for_workspace(workspace_id)
            .await
    }
}

#[async_trait::async_trait]
impl SessionLifecycleHost for TaskSessionCleanupHost {
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

#[async_trait::async_trait]
impl SessionHeadRefreshHost for TaskSessionCleanupHost {
    async fn load_active_snapshot_head(&self, session_id: SessionId) -> SessionHeadRefreshLoad {
        let store = match self.store_for_session(session_id).await {
            Ok(store) => store,
            Err(err) => {
                return SessionHeadRefreshLoad::Failed {
                    error: format!("{err:#}"),
                };
            }
        };
        match store.get_active_snapshot_head(session_id).await {
            Ok(Some(head)) => SessionHeadRefreshLoad::Found(Box::new(head)),
            Ok(None) => SessionHeadRefreshLoad::Missing,
            Err(err) => SessionHeadRefreshLoad::Failed {
                error: format!("{err:#}"),
            },
        }
    }

    async fn update_compact_session_head(&self, head: SessionHeadSnapshot) {
        self.active_snapshot.update_compact_session_head(head).await;
    }

    async fn remove_session_from_active_head_cache(&self, session_id: SessionId) {
        self.active_snapshot.remove_session(session_id).await;
    }
}

pub(in crate::daemon) fn task_metadata_effects(
    task_publication: Arc<TaskPublicationHost>,
) -> Arc<TaskMetadataEffects> {
    let emit_workspace_task_delta = Arc::new({
        let task_publication = Arc::clone(&task_publication);
        move |task: Task, kind: TaskDeltaKind| {
            let task_publication = Arc::clone(&task_publication);
            Box::pin(async move {
                task_publication.emit_workspace_task_delta(task, kind).await;
            }) as TaskMetadataFuture<_>
        }
    });
    let emit_workspace_task_upsert = Arc::new({
        let task_publication = Arc::clone(&task_publication);
        move |task_id: TaskId| {
            let task_publication = Arc::clone(&task_publication);
            Box::pin(async move { task_publication.emit_workspace_task_upsert(task_id).await })
                as TaskMetadataFuture<_>
        }
    });
    TaskMetadataEffects::new(emit_workspace_task_delta, emit_workspace_task_upsert)
}

pub(in crate::daemon) fn task_lifecycle_effects(
    task_publication: Arc<TaskPublicationHost>,
    task_cleanup: TaskSessionCleanupHost,
) -> Arc<TaskLifecycleEffects> {
    let cleanup_session = Arc::new({
        let task_cleanup = task_cleanup.clone();
        move |session_id: SessionId| {
            let task_cleanup = task_cleanup.clone();
            Box::pin(async move { task_cleanup.cleanup_session(session_id).await })
                as TaskLifecycleFuture<_>
        }
    });
    let emit_workspace_task_delta = Arc::new({
        let task_publication = Arc::clone(&task_publication);
        move |task: Task, kind: TaskDeltaKind| {
            let task_publication = Arc::clone(&task_publication);
            Box::pin(async move {
                task_publication.emit_workspace_task_delta(task, kind).await;
            }) as TaskLifecycleFuture<_>
        }
    });
    let emit_workspace_task_upsert = Arc::new({
        let task_publication = Arc::clone(&task_publication);
        move |task_id: TaskId| {
            let task_publication = Arc::clone(&task_publication);
            Box::pin(async move { task_publication.emit_workspace_task_upsert(task_id).await })
                as TaskLifecycleFuture<_>
        }
    });
    let remove_active_snapshot_session = Arc::new({
        let task_cleanup = task_cleanup.clone();
        move |session_id: SessionId| {
            let task_cleanup = task_cleanup.clone();
            Box::pin(async move {
                task_cleanup
                    .remove_active_snapshot_session(session_id)
                    .await;
            }) as TaskLifecycleFuture<_>
        }
    });
    let refresh_session_head_cache = Arc::new({
        let task_cleanup = task_cleanup.clone();
        move |session_id: SessionId| {
            let task_cleanup = task_cleanup.clone();
            Box::pin(async move { task_cleanup.refresh_session_head_cache(session_id).await })
                as TaskLifecycleFuture<_>
        }
    });
    let emit_workspace_archived_task_delete = Arc::new({
        let task_publication = Arc::clone(&task_publication);
        move |workspace_id: WorkspaceId, task_id: TaskId| {
            let task_publication = Arc::clone(&task_publication);
            Box::pin(async move {
                task_publication
                    .emit_workspace_archived_task_delete(workspace_id, task_id)
                    .await;
            }) as TaskLifecycleFuture<_>
        }
    });
    let emit_workspace_task_delete = Arc::new({
        let task_publication = Arc::clone(&task_publication);
        move |workspace_id: WorkspaceId, task_id: TaskId| {
            let task_publication = Arc::clone(&task_publication);
            Box::pin(async move {
                task_publication
                    .emit_workspace_task_delete(workspace_id, task_id)
                    .await;
            }) as TaskLifecycleFuture<_>
        }
    });
    TaskLifecycleEffects::new(
        cleanup_session,
        emit_workspace_task_delta,
        emit_workspace_task_upsert,
        remove_active_snapshot_session,
        refresh_session_head_cache,
        emit_workspace_archived_task_delete,
        emit_workspace_task_delete,
    )
}

pub(in crate::daemon) fn task_admission_session_effects(
    publisher: SessionPublicationEffects,
    session_runtime: Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
    scheduler_spawner: crate::daemon::session_route_handles::SessionMessageSchedulerSpawner,
    title_model_mode: crate::daemon::SessionTitleModelModeHandle,
    task_publication: Arc<TaskPublicationHost>,
) -> Arc<TaskAdmissionSessionEffects> {
    let publish_event = Arc::new({
        let publisher = publisher.clone();
        move |event: SessionEvent| {
            let publisher = publisher.clone();
            Box::pin(async move { publisher.publish_event(event).await }) as TaskAdmissionFuture<_>
        }
    });
    let ensure_scheduler = Arc::new(move |session: Session| {
        let session_runtime = Arc::clone(&session_runtime);
        let scheduler_spawner = scheduler_spawner.clone();
        Box::pin(async move {
            scheduler_spawner
                .ensure_scheduler(&session_runtime, session)
                .await
        }) as TaskAdmissionFuture<_>
    });
    let schedule_title_generation =
        Arc::new(move |session: Session, prompt: String, force: bool| {
            let title_model_mode = title_model_mode.clone();
            Box::pin(async move {
                title_model_mode
                    .schedule_session_title_generation(session, prompt, force)
                    .await
            }) as TaskAdmissionFuture<_>
        });
    let emit_workspace_task_upsert = Arc::new(move |task_id: TaskId| {
        let task_publication = Arc::clone(&task_publication);
        Box::pin(async move { task_publication.emit_workspace_task_upsert(task_id).await })
            as TaskAdmissionFuture<_>
    });
    TaskAdmissionSessionEffects::new(
        publish_event,
        ensure_scheduler,
        schedule_title_generation,
        emit_workspace_task_upsert,
    )
}
