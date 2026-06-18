use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use ctx_core::ids::{SessionId, TaskId, WorkspaceId};
use ctx_core::models::{
    ExecutionEnvironment, Session, SessionEvent, SessionHeadDelta, SessionSummaryDelta,
    SessionTurn, SessionTurnToolSummary, Task, TaskDeltaKind, Workspace,
};
use ctx_observability::ops_events::OpsEvents;
use ctx_provider_runtime::{
    provider_install_tracker::ProviderInstallOpsEvent, ProviderRuntime, ProviderRuntimeHost,
};
use ctx_session_runtime::runtime::{
    SessionEventPublicationHost, SessionReplayCursor, SessionRuntime, SessionTaskDeltaRefreshHost,
};
use ctx_store::Store;
use ctx_workspace_active_snapshot::WorkspaceActiveSnapshotHub;
use ctx_workspace_runtime::HarnessRuntimeManager;

use crate::daemon::plugins::PluginInventoryRuntime;
use crate::daemon::state::{ProtectedWorkspaceStoreLookup, SessionStoreLookup};

pub(in crate::daemon) struct SessionTitleModelModeHandleParts {
    pub(in crate::daemon) global_store: Store,
    pub(in crate::daemon) session_stores: SessionStoreLookup,
    pub(in crate::daemon) workspace_stores: ProtectedWorkspaceStoreLookup,
    pub(in crate::daemon) session_runtime:
        Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
    pub(in crate::daemon) active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    pub(in crate::daemon) provider_runtime: Arc<ProviderRuntime>,
    pub(in crate::daemon) plugins: Arc<PluginInventoryRuntime>,
    pub(in crate::daemon) ops_events: OpsEvents,
    pub(in crate::daemon) data_root: PathBuf,
    pub(in crate::daemon) daemon_url: String,
    pub(in crate::daemon) auth_token: Option<String>,
    pub(in crate::daemon) harness: Arc<HarnessRuntimeManager>,
}

#[derive(Clone)]
pub struct SessionTitleModelModeHandle {
    global_store: Store,
    session_stores: SessionStoreLookup,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    session_runtime: Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    provider_runtime: Arc<ProviderRuntime>,
    plugins: Arc<PluginInventoryRuntime>,
    ops_events: OpsEvents,
    data_root: PathBuf,
    daemon_url: String,
    auth_token: Option<String>,
    harness: Arc<HarnessRuntimeManager>,
}

impl SessionTitleModelModeHandle {
    pub(in crate::daemon) fn new(parts: SessionTitleModelModeHandleParts) -> Self {
        Self {
            global_store: parts.global_store,
            session_stores: parts.session_stores,
            workspace_stores: parts.workspace_stores,
            session_runtime: parts.session_runtime,
            active_snapshot: parts.active_snapshot,
            provider_runtime: parts.provider_runtime,
            plugins: parts.plugins,
            ops_events: parts.ops_events,
            data_root: parts.data_root,
            daemon_url: parts.daemon_url,
            auth_token: parts.auth_token,
            harness: parts.harness,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) fn active_snapshot(&self) -> &WorkspaceActiveSnapshotHub {
        self.active_snapshot.as_ref()
    }

    pub(in crate::daemon) fn providers(&self) -> &ProviderRuntime {
        self.provider_runtime.as_ref()
    }

    pub(in crate::daemon) async fn sync_plugin_provider_adapters(&self) {
        self.plugins.sync_provider_adapters(self.providers()).await;
    }

    pub(in crate::daemon) fn ops_events(&self) -> &OpsEvents {
        &self.ops_events
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) fn daemon_url(&self) -> &str {
        &self.daemon_url
    }

    pub(in crate::daemon) fn auth_token(&self) -> Option<&String> {
        self.auth_token.as_ref()
    }

    pub(in crate::daemon) fn harness(&self) -> &HarnessRuntimeManager {
        self.harness.as_ref()
    }

    pub(in crate::daemon) async fn get_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Option<Workspace>> {
        self.global_store().get_workspace(workspace_id).await
    }

    pub(in crate::daemon) async fn existing_session_store_for_write(
        &self,
        session_id: SessionId,
    ) -> Result<Store, crate::daemon::SessionStoreAccessError> {
        self.session_stores
            .existing_session_store_for_write(session_id)
            .await
    }

    pub(in crate::daemon) async fn session_store_or_none(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<Option<Store>> {
        match self.session_stores.existing_session_store(session_id).await {
            Ok(store) => Ok(Some(store)),
            Err(crate::daemon::SessionStoreAccessError::NotFound) => Ok(None),
            Err(crate::daemon::SessionStoreAccessError::LookupUnavailable(error)) => Err(error),
            Err(crate::daemon::SessionStoreAccessError::StoreUnavailable) => {
                anyhow::bail!("workspace store unavailable")
            }
        }
    }

    pub(in crate::daemon) async fn session_store_for_write_or_none(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<Option<Store>> {
        match self.existing_session_store_for_write(session_id).await {
            Ok(store) => Ok(Some(store)),
            Err(crate::daemon::SessionStoreAccessError::NotFound) => Ok(None),
            Err(crate::daemon::SessionStoreAccessError::LookupUnavailable(error)) => Err(error),
            Err(crate::daemon::SessionStoreAccessError::StoreUnavailable) => {
                anyhow::bail!("workspace store unavailable")
            }
        }
    }

    pub(in crate::daemon) async fn store_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Store> {
        self.workspace_stores
            .store_for_workspace(workspace_id)
            .await
    }

    pub(in crate::daemon) async fn store_for_task(&self, task_id: TaskId) -> anyhow::Result<Store> {
        let workspace_id = self
            .global_store()
            .get_workspace_id_for_task(task_id)
            .await?
            .with_context(|| format!("workspace missing for task {}", task_id.0))?;
        self.store_for_workspace(workspace_id).await
    }

    pub(in crate::daemon) async fn store_for_session(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<Store> {
        self.session_store_or_none(session_id)
            .await?
            .with_context(|| format!("workspace missing for session {}", session_id.0))
    }

    pub(in crate::daemon) async fn remember_session_meta(&self, session: &Session) {
        self.session_runtime.remember_session_meta(session).await;
    }

    pub(in crate::daemon) async fn publish_event(&self, event: SessionEvent) {
        let host = SessionTitleModelModePublicationHost::new(self.clone());
        self.session_runtime
            .publish_event_with_host(&host, event)
            .await;
    }

    pub(in crate::daemon) async fn emit_workspace_task_upsert(
        &self,
        task_id: TaskId,
    ) -> anyhow::Result<()> {
        let mut task: Option<Task> = None;
        let store = self.store_for_task(task_id).await?;
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

    async fn emit_workspace_archived_task_upsert(&self, task: &Task) -> anyhow::Result<()> {
        let store = self.store_for_task(task.id).await?;
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

    pub(in crate::daemon) async fn load_provider_model_catalog_for_execution_environment(
        &self,
        workspace: &Workspace,
        provider_id: &str,
        execution_environment: ExecutionEnvironment,
    ) -> Result<Option<ctx_session_tools::model_resolution::ModelCatalog>, String> {
        crate::daemon::sessions::model_catalog::load_provider_model_catalog_for_execution_environment(
            self,
            workspace,
            provider_id,
            execution_environment,
        )
        .await
    }
}

impl ProviderRuntimeHost for SessionTitleModelModeHandle {
    fn data_root(&self) -> &Path {
        self.data_root()
    }

    fn current_ctx_version(&self) -> Option<String> {
        crate::daemon::provider_capability_hosts::current_ctx_version_for_provider_runtime()
    }

    fn provider_runtime(&self) -> &ProviderRuntime {
        self.providers()
    }

    fn publish_provider_install_ops_events(&self, events: Vec<ProviderInstallOpsEvent>) {
        crate::daemon::provider_capability_hosts::emit_provider_install_ops_events(
            self.ops_events(),
            events,
        );
    }
}

struct SessionTitleModelModePublicationHost {
    handle: SessionTitleModelModeHandle,
    task_delta_refresh_host: Arc<SessionTitleModelModeTaskDeltaRefreshHost>,
}

impl SessionTitleModelModePublicationHost {
    fn new(handle: SessionTitleModelModeHandle) -> Self {
        Self {
            task_delta_refresh_host: Arc::new(SessionTitleModelModeTaskDeltaRefreshHost {
                handle: handle.clone(),
            }),
            handle,
        }
    }
}

#[async_trait::async_trait]
impl SessionEventPublicationHost for SessionTitleModelModePublicationHost {
    type TaskDeltaRefreshHost = SessionTitleModelModeTaskDeltaRefreshHost;

    fn task_delta_refresh_host(&self) -> Arc<Self::TaskDeltaRefreshHost> {
        Arc::clone(&self.task_delta_refresh_host)
    }

    async fn load_session(&self, session_id: SessionId) -> Option<Session> {
        let store = self.handle.store_for_session(session_id).await.ok()?;
        store.get_session(session_id).await.ok().flatten()
    }

    async fn list_turn_tool_summaries_for_turn(
        &self,
        session_id: SessionId,
        turn_id: ctx_core::ids::TurnId,
    ) -> Vec<SessionTurnToolSummary> {
        let Ok(store) = self.handle.store_for_session(session_id).await else {
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
        turn_id: ctx_core::ids::TurnId,
    ) -> Option<SessionTurn> {
        self.handle
            .active_snapshot()
            .get_cached_session_head_for_read(session_id)
            .await
            .and_then(|head| head.turns.into_iter().find(|turn| turn.turn_id == turn_id))
    }

    async fn load_turn(
        &self,
        session_id: SessionId,
        turn_id: ctx_core::ids::TurnId,
    ) -> Option<SessionTurn> {
        let store = self.handle.store_for_session(session_id).await.ok()?;
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
            .handle
            .active_snapshot()
            .session_replay_cursor(workspace_id, session_id)
            .await;
        SessionReplayCursor {
            last_event_seq: cursor.last_event_seq,
            projection_rev: cursor.projection_rev,
        }
    }

    async fn load_projection_rev(&self, session_id: SessionId) -> Option<i64> {
        let store = self.handle.store_for_session(session_id).await.ok()?;
        store.get_session_projection_rev(session_id).await.ok()
    }

    async fn publish_session_head_delta(
        &self,
        workspace_id: WorkspaceId,
        session: &Session,
        delta: SessionHeadDelta,
        durable: bool,
    ) {
        self.handle
            .active_snapshot()
            .publish_session_head_delta(workspace_id, session, delta, durable)
            .await;
    }

    async fn publish_session_summary_delta(
        &self,
        workspace_id: WorkspaceId,
        delta: SessionSummaryDelta,
    ) {
        self.handle
            .active_snapshot()
            .publish_session_summary_delta(workspace_id, delta)
            .await;
    }
}

pub(in crate::daemon) struct SessionTitleModelModeTaskDeltaRefreshHost {
    handle: SessionTitleModelModeHandle,
}

#[async_trait::async_trait]
impl SessionTaskDeltaRefreshHost for SessionTitleModelModeTaskDeltaRefreshHost {
    async fn emit_task_delta_refresh(&self, task_id: TaskId) {
        let store = match self.handle.store_for_task(task_id).await {
            Ok(store) => store,
            Err(err) => {
                tracing::warn!(
                    task_id = %task_id.0,
                    "workspace task delta refresh store lookup failed: {err:?}"
                );
                return;
            }
        };
        match store.get_workspace_active_task_summary(task_id).await {
            Ok(Some(summary)) => {
                let _ = self
                    .handle
                    .active_snapshot()
                    .publish_task_delta(
                        summary.task.workspace_id,
                        summary.task,
                        TaskDeltaKind::Updated,
                    )
                    .await;
            }
            Ok(None) => match store.get_task(task_id).await {
                Ok(Some(task)) => {
                    let kind = if task.archived_at.is_some() {
                        TaskDeltaKind::Archived
                    } else {
                        TaskDeltaKind::Updated
                    };
                    let _ = self
                        .handle
                        .active_snapshot()
                        .publish_task_delta(task.workspace_id, task, kind)
                        .await;
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(
                        task_id = %task_id.0,
                        "workspace task delta refresh read failed: {err:?}"
                    );
                }
            },
            Err(err) => {
                tracing::warn!(
                    task_id = %task_id.0,
                    "workspace task delta refresh summary read failed: {err:?}"
                );
            }
        }
    }
}
