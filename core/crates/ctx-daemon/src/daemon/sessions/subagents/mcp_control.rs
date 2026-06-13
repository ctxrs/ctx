use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use ctx_core::ids::{MessageId, RunId, SessionId, TaskId, TurnId, WorkspaceId};
use ctx_core::models::{
    Message, MessageDelivery, MessageRole, Session, SessionEvent, SessionEventType,
    SessionHeadDelta, SessionSummaryDelta, SessionTurn, SessionTurnStatus, SessionTurnToolSummary,
    TaskDeltaKind,
};
use ctx_provider_runtime::ProviderRuntime;
use ctx_session_runtime::runtime::{
    SessionEventPublicationHost, SessionLifecycleHost, SessionReplayCursor, SessionRuntime,
    SessionTaskDeltaRefreshHost,
};
use ctx_store::Store;
use ctx_workspace_active_snapshot::WorkspaceActiveSnapshotHub;
use tokio::sync::mpsc;

use crate::daemon::scheduler::SessionSchedulerWorkerHost;
use crate::daemon::{
    session_store_access_anyhow, ProtectedWorkspaceStoreLookup, SessionStoreLookup,
};

pub(in crate::daemon) type SessionSubagentMcpControlFuture<T> =
    Pin<Box<dyn Future<Output = T> + Send + 'static>>;
pub(in crate::daemon) type SessionSubagentMcpControlProviderTimeout =
    Arc<dyn Fn() -> SessionSubagentMcpControlFuture<Duration> + Send + Sync>;
pub(in crate::daemon) type SessionSubagentMcpControlLegacyContextWindowRejectCounter =
    Arc<dyn Fn(String) -> SessionSubagentMcpControlFuture<()> + Send + Sync>;
#[derive(Clone)]
pub struct SessionSubagentMcpControlHandle {
    session_stores: SessionStoreLookup,
    session_runtime: Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
    scheduler_spawner: SessionSubagentMcpControlSchedulerSpawner,
    publish_host: SessionSubagentMcpControlPublicationHost,
    lifecycle_host: SessionSubagentMcpControlLifecycleHost,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    spawn_host: Arc<crate::daemon::sessions::subagents::SubagentSpawnHost>,
    archive_worktree_cleanup:
        Arc<crate::daemon::sessions::subagents::SubagentArchiveWorktreeCleanupHost>,
    provider_inactivity_timeout: SessionSubagentMcpControlProviderTimeout,
    emit_legacy_context_window_key_reject:
        SessionSubagentMcpControlLegacyContextWindowRejectCounter,
}

pub(in crate::daemon) struct SessionSubagentMcpControlHandleParts {
    pub(in crate::daemon) session_stores: SessionStoreLookup,
    pub(in crate::daemon) session_runtime:
        Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
    pub(in crate::daemon) scheduler_spawner: SessionSubagentMcpControlSchedulerSpawner,
    pub(in crate::daemon) publish_host: SessionSubagentMcpControlPublicationHost,
    pub(in crate::daemon) lifecycle_host: SessionSubagentMcpControlLifecycleHost,
    pub(in crate::daemon) active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    pub(in crate::daemon) spawn_host: Arc<crate::daemon::sessions::subagents::SubagentSpawnHost>,
    pub(in crate::daemon) archive_worktree_cleanup:
        Arc<crate::daemon::sessions::subagents::SubagentArchiveWorktreeCleanupHost>,
    pub(in crate::daemon) provider_inactivity_timeout: SessionSubagentMcpControlProviderTimeout,
    pub(in crate::daemon) emit_legacy_context_window_key_reject:
        SessionSubagentMcpControlLegacyContextWindowRejectCounter,
}

impl SessionSubagentMcpControlHandle {
    pub(in crate::daemon) fn new(parts: SessionSubagentMcpControlHandleParts) -> Self {
        Self {
            session_stores: parts.session_stores,
            session_runtime: parts.session_runtime,
            scheduler_spawner: parts.scheduler_spawner,
            publish_host: parts.publish_host,
            lifecycle_host: parts.lifecycle_host,
            active_snapshot: parts.active_snapshot,
            spawn_host: parts.spawn_host,
            archive_worktree_cleanup: parts.archive_worktree_cleanup,
            provider_inactivity_timeout: parts.provider_inactivity_timeout,
            emit_legacy_context_window_key_reject: parts.emit_legacy_context_window_key_reject,
        }
    }

    async fn load_parent_session(
        &self,
        parent_id: SessionId,
    ) -> Result<(Store, Session), crate::daemon::sessions::subagents::SubagentError> {
        let store = match self.session_stores.existing_session_store(parent_id).await {
            Ok(store) => store,
            Err(crate::daemon::SessionStoreAccessError::NotFound) => {
                return Err(crate::daemon::sessions::subagents::not_found(
                    "parent session not found",
                ));
            }
            Err(error) => {
                return Err(crate::daemon::sessions::subagents::internal_api_error(
                    session_store_access_anyhow(error),
                ));
            }
        };
        let parent = store
            .get_session(parent_id)
            .await
            .map_err(crate::daemon::sessions::subagents::internal_api_error)?
            .ok_or_else(|| {
                crate::daemon::sessions::subagents::not_found("parent session not found")
            })?;
        Ok((store, parent))
    }

    async fn provider_inactivity_timeout(&self) -> Duration {
        (self.provider_inactivity_timeout)().await
    }

    pub(in crate::daemon) async fn require_scoped_mcp_session_context(
        &self,
        mcp_auth: ctx_mcp_auth::McpAuthContext,
        session_id: SessionId,
    ) -> Result<(), crate::daemon::ScopedMcpSessionAccessError> {
        self.session_stores
            .require_scoped_mcp_session_context(mcp_auth, session_id)
            .await
    }

    pub(in crate::daemon) async fn spawn_agent(
        &self,
        parent_id: SessionId,
        req: crate::daemon::sessions::subagents::SpawnAgentReq,
    ) -> Result<
        crate::daemon::sessions::subagents::SpawnAgentResp,
        crate::daemon::sessions::subagents::SubagentError,
    > {
        self.spawn_host.spawn_agent(parent_id, req).await
    }

    pub(in crate::daemon) async fn send_input(
        &self,
        parent_id: SessionId,
        req: crate::daemon::sessions::subagents::SendInputReq,
    ) -> Result<
        crate::daemon::sessions::subagents::SendInputResp,
        crate::daemon::sessions::subagents::SubagentError,
    > {
        let (store, parent) = self.load_parent_session(parent_id).await?;
        let child = crate::daemon::sessions::subagents::resolve_child_agent_session(
            &store,
            &parent,
            &req.agent_id,
        )
        .await?;
        let message = req.message.trim().to_string();
        if message.is_empty() {
            return Err(crate::daemon::sessions::subagents::api_error(
                crate::daemon::sessions::subagents::SubagentErrorKind::BadRequest,
                "message is required",
            ));
        }

        let interrupt = req.interrupt.unwrap_or(false);
        if interrupt {
            self.send_scheduler_interrupt(&child).await;
        }

        let persisted = self.enqueue_subagent_prompt(&child, message).await?;
        let detail = self
            .build_enqueued_agent_detail(&store, &parent, &child, &persisted)
            .await;
        Ok(crate::daemon::sessions::subagents::SendInputResp {
            agent: detail,
            queued_run_id: ctx_subagent_service::encode_run_ref(persisted.run_id),
            delivery: ctx_subagent_service::agent_delivery_label(&persisted.saved_message.delivery)
                .to_string(),
        })
    }

    pub(in crate::daemon) async fn archive_agent(
        &self,
        parent_id: SessionId,
        req: crate::daemon::sessions::subagents::ArchiveAgentReq,
    ) -> Result<
        crate::daemon::sessions::subagents::ArchiveAgentResp,
        crate::daemon::sessions::subagents::SubagentError,
    > {
        let (store, parent) = self.load_parent_session(parent_id).await?;
        let child = crate::daemon::sessions::subagents::resolve_child_agent_session(
            &store,
            &parent,
            &req.agent_id,
        )
        .await?;
        let latest_turn = store
            .get_latest_turn_for_session(child.id)
            .await
            .map_err(crate::daemon::sessions::subagents::internal_api_error)?;
        if latest_turn
            .as_ref()
            .is_some_and(|turn| ctx_subagent_service::is_active_turn_status(&turn.status))
        {
            return Err(crate::daemon::sessions::subagents::api_error(
                crate::daemon::sessions::subagents::SubagentErrorKind::BadRequest,
                "cannot archive agent while it has active or queued work; wait or interrupt first",
            ));
        }

        let archived = store
            .archive_subagent_session(parent.id, child.id)
            .await
            .map_err(crate::daemon::sessions::subagents::internal_api_error)?;
        if !archived {
            return Err(crate::daemon::sessions::subagents::api_error(
                crate::daemon::sessions::subagents::SubagentErrorKind::NotFound,
                "agent not found",
            ));
        }
        self.active_snapshot
            .remove_subagent_session_from_active_task(child.workspace_id, child.task_id, child.id)
            .await;
        self.session_runtime
            .cleanup_session_with_host(&self.lifecycle_host, child.id)
            .await;
        let cleanup_failed =
            crate::daemon::sessions::subagents::cleanup_archived_subagent_worktree_with_host(
                self.archive_worktree_cleanup.as_ref(),
                &store,
                &parent,
                &child,
            )
            .await;

        Ok(crate::daemon::sessions::subagents::ArchiveAgentResp {
            agent_id: ctx_subagent_service::encode_agent_ref(child.id),
            task_label: child.title.trim().to_string(),
            archived: true,
            cleanup_failed,
        })
    }

    pub(in crate::daemon) async fn interrupt_agent(
        &self,
        parent_id: SessionId,
        req: crate::daemon::sessions::subagents::InterruptAgentReq,
    ) -> Result<
        crate::daemon::sessions::subagents::InterruptAgentResp,
        crate::daemon::sessions::subagents::SubagentError,
    > {
        let (store, parent) = self.load_parent_session(parent_id).await?;
        let inactivity_timeout = self.provider_inactivity_timeout().await;
        let child = crate::daemon::sessions::subagents::resolve_child_agent_session(
            &store,
            &parent,
            &req.agent_id,
        )
        .await?;
        self.send_scheduler_interrupt(&child).await;
        let detail = crate::daemon::sessions::subagents::build_agent_detail_for_mcp_read(
            &store,
            &parent,
            &child,
            inactivity_timeout,
            &self.emit_legacy_context_window_key_reject,
        )
        .await?;
        Ok(crate::daemon::sessions::subagents::InterruptAgentResp { agent: detail })
    }

    async fn send_scheduler_interrupt(&self, child: &Session) {
        let tx = self
            .scheduler_spawner
            .ensure_scheduler(&self.session_runtime, child.clone())
            .await;
        let interrupt = ctx_session_tools::interrupt_telemetry::InterruptTelemetryContext::new(
            uuid::Uuid::new_v4().to_string(),
        );
        let _ = tx
            .send(crate::daemon::scheduler::SchedulerCommand::Interrupt(
                interrupt,
            ))
            .await;
    }

    async fn enqueue_subagent_prompt(
        &self,
        session: &Session,
        prompt: String,
    ) -> Result<
        crate::daemon::sessions::subagents::PersistedSubagentPrompt,
        crate::daemon::sessions::subagents::SubagentError,
    > {
        let persisted = self.persist_subagent_prompt(session, prompt).await?;
        self.dispatch_subagent_prompt(session, &persisted.saved_message)
            .await;
        Ok(persisted)
    }

    async fn persist_subagent_prompt(
        &self,
        session: &Session,
        prompt: String,
    ) -> Result<
        crate::daemon::sessions::subagents::PersistedSubagentPrompt,
        crate::daemon::sessions::subagents::SubagentError,
    > {
        let store = self
            .session_stores
            .existing_session_store_for_write(session.id)
            .await
            .map_err(|error| {
                crate::daemon::sessions::subagents::internal_api_error(session_store_access_anyhow(
                    error,
                ))
            })?;
        let run_id = RunId::new();
        let turn_id = TurnId::new();
        let message_id = MessageId::new();
        let order_seq_state = self
            .session_runtime
            .get_order_seq_state(&store, session.id)
            .await;
        let order_seq = {
            let mut order_seq_state = order_seq_state.lock().await;
            order_seq_state.get_or_assign(format!("message:{}", message_id.0), None)
        };
        let has_backlog = self.session_runtime.is_running(session.id).await
            || !store
                .list_queued_messages_for_session(session.id)
                .await
                .map_err(crate::daemon::sessions::subagents::internal_api_error)?
                .is_empty()
            || store
                .get_latest_turn_for_session(session.id)
                .await
                .map_err(crate::daemon::sessions::subagents::internal_api_error)?
                .as_ref()
                .is_some_and(|turn| ctx_subagent_service::is_active_turn_status(&turn.status));
        let delivery = if has_backlog {
            MessageDelivery::Queued
        } else {
            MessageDelivery::Immediate
        };
        let msg = Message {
            id: message_id,
            session_id: session.id,
            task_id: session.task_id,
            run_id: Some(run_id),
            turn_id: Some(turn_id),
            turn_sequence: Some(0),
            order_seq: Some(order_seq),
            role: MessageRole::User,
            content: prompt,
            attachments: vec![],
            delivery,
            delivered_at: None,
            created_at: chrono::Utc::now(),
        };
        let saved = store
            .insert_message(msg)
            .await
            .map_err(crate::daemon::sessions::subagents::internal_api_error)?;
        let event = store
            .append_session_event(
                session.id,
                Some(run_id),
                Some(turn_id),
                SessionEventType::UserMessage,
                serde_json::json!({
                    "message_id": saved.id.0,
                    "content": saved.content.clone(),
                    "delivery": saved.delivery.clone(),
                    "attachments": saved.attachments,
                    "order_seq": order_seq,
                }),
            )
            .await
            .map_err(crate::daemon::sessions::subagents::internal_api_error)?;
        let start_seq = event.seq;
        let mut last_event_seq = start_seq;

        let turn = SessionTurn {
            turn_id,
            session_id: session.id,
            run_id: Some(run_id),
            user_message_id: Some(saved.id),
            status: match saved.delivery {
                MessageDelivery::Queued => SessionTurnStatus::Queued,
                MessageDelivery::Immediate => SessionTurnStatus::Starting,
            },
            start_seq: Some(start_seq),
            end_seq: None,
            started_at: saved.created_at,
            updated_at: saved.created_at,
            assistant_partial: None,
            thought_partial: None,
            metrics_json: None,
            failure: None,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
        };
        let _ = store.insert_session_turn(turn).await;
        self.publish_event(event).await;
        if matches!(saved.delivery, MessageDelivery::Queued) {
            last_event_seq = self
                .append_and_publish_queued_prompt_events(&store, session, run_id, turn_id, &saved)
                .await?;
        }

        Ok(
            crate::daemon::sessions::subagents::PersistedSubagentPrompt {
                run_id,
                saved_message: saved,
                last_event_seq,
            },
        )
    }

    async fn append_and_publish_queued_prompt_events(
        &self,
        store: &Store,
        session: &Session,
        run_id: RunId,
        turn_id: TurnId,
        saved: &Message,
    ) -> Result<i64, crate::daemon::sessions::subagents::SubagentError> {
        let queued = store
            .append_session_event(
                session.id,
                Some(run_id),
                Some(turn_id),
                SessionEventType::InputQueued,
                serde_json::json!({"message_id": saved.id.0}),
            )
            .await
            .map_err(crate::daemon::sessions::subagents::internal_api_error)?;
        self.publish_event(queued).await;

        let queue_position = store
            .list_queued_messages_for_session(session.id)
            .await
            .ok()
            .and_then(|messages| {
                messages
                    .iter()
                    .position(|message| message.id == saved.id)
                    .map(|idx| idx as i64)
            });

        let queue_added = store
            .append_session_event(
                session.id,
                Some(run_id),
                Some(turn_id),
                SessionEventType::MessageQueueAdded,
                serde_json::json!({
                    "message_id": saved.id.0,
                    "queue_position": queue_position,
                }),
            )
            .await
            .map_err(crate::daemon::sessions::subagents::internal_api_error)?;
        self.publish_event(queue_added).await;

        let turn_queued = store
            .append_session_event(
                session.id,
                Some(run_id),
                Some(turn_id),
                SessionEventType::TurnQueued,
                serde_json::json!({
                    "message_id": saved.id.0,
                    "queue_position": queue_position,
                }),
            )
            .await
            .map_err(crate::daemon::sessions::subagents::internal_api_error)?;
        let last_event_seq = turn_queued.seq;
        self.publish_event(turn_queued).await;
        Ok(last_event_seq)
    }

    async fn dispatch_subagent_prompt(&self, session: &Session, saved: &Message) {
        let tx = self
            .scheduler_spawner
            .ensure_scheduler(&self.session_runtime, session.clone())
            .await;
        let queued = crate::daemon::scheduler::QueuedMessage {
            message: saved.clone(),
            enqueued_at: Instant::now(),
            run_id: None,
        };
        let _ = tx
            .send(crate::daemon::scheduler::SchedulerCommand::Enqueue(queued))
            .await;
    }

    async fn publish_event(&self, event: SessionEvent) {
        self.session_runtime
            .publish_event_with_host(&self.publish_host, event)
            .await;
    }

    async fn build_enqueued_agent_detail(
        &self,
        store: &Store,
        parent: &Session,
        session: &Session,
        persisted: &crate::daemon::sessions::subagents::PersistedSubagentPrompt,
    ) -> crate::daemon::sessions::subagents::AgentDetail {
        let task_label = {
            let trimmed = session.title.trim();
            if trimmed.is_empty() {
                format!("agent-{}", session.id.0)
            } else {
                trimmed.to_string()
            }
        };

        crate::daemon::sessions::subagents::AgentDetail {
            agent: crate::daemon::sessions::subagents::AgentSummary {
                agent_id: ctx_subagent_service::encode_agent_ref(session.id),
                task_label,
                state: ctx_subagent_service::agent_active_state(
                    match persisted.saved_message.delivery {
                        MessageDelivery::Queued => SessionTurnStatus::Queued,
                        MessageDelivery::Immediate => SessionTurnStatus::Starting,
                    },
                )
                .to_string(),
                health: "healthy".to_string(),
                current_run_id: Some(ctx_subagent_service::encode_run_ref(persisted.run_id)),
                latest_result_status: None,
                last_progress_at: Some(persisted.saved_message.created_at.to_rfc3339()),
                last_event_seq: persisted.last_event_seq,
            },
            latest_result: None,
            worktree_path: crate::daemon::sessions::subagents::worktree_path_for_child_in_store(
                store,
                parent.worktree_id,
                session.id,
            )
            .await,
        }
    }
}

#[derive(Clone)]
pub(in crate::daemon) struct SessionSubagentMcpControlSchedulerSpawner {
    host: Weak<SessionSchedulerWorkerHost>,
}

impl SessionSubagentMcpControlSchedulerSpawner {
    pub(in crate::daemon) fn new(host: Weak<SessionSchedulerWorkerHost>) -> Self {
        Self { host }
    }

    pub(in crate::daemon) async fn ensure_scheduler(
        &self,
        runtime: &SessionRuntime<crate::daemon::scheduler::SchedulerCommand>,
        session: Session,
    ) -> mpsc::Sender<crate::daemon::scheduler::SchedulerCommand> {
        let host = self.host.clone();
        runtime
            .ensure_scheduler(session, move |session, rx| {
                crate::daemon::scheduler::session_worker(host, session, rx)
            })
            .await
    }
}

#[derive(Clone)]
pub(in crate::daemon) struct SessionSubagentMcpControlLifecycleHost {
    global_store: Store,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    providers: Arc<ProviderRuntime>,
}

impl SessionSubagentMcpControlLifecycleHost {
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
impl SessionLifecycleHost for SessionSubagentMcpControlLifecycleHost {
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

#[derive(Clone)]
pub(in crate::daemon) struct SessionSubagentMcpControlPublicationHost {
    session_stores: SessionStoreLookup,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    task_delta_refresh_host: Arc<SessionSubagentMcpControlTaskDeltaRefreshHost>,
}

impl SessionSubagentMcpControlPublicationHost {
    pub(in crate::daemon) fn new(
        session_stores: SessionStoreLookup,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    ) -> Self {
        let task_delta_refresh_host = Arc::new(SessionSubagentMcpControlTaskDeltaRefreshHost {
            workspace_stores: workspace_stores.clone(),
            active_snapshot: Arc::clone(&active_snapshot),
        });
        Self {
            session_stores,
            active_snapshot,
            task_delta_refresh_host,
        }
    }

    async fn store_for_session(&self, session_id: SessionId) -> anyhow::Result<Store> {
        self.session_stores
            .existing_session_store(session_id)
            .await
            .map_err(session_store_access_anyhow)
    }
}

#[async_trait::async_trait]
impl SessionEventPublicationHost for SessionSubagentMcpControlPublicationHost {
    type TaskDeltaRefreshHost = SessionSubagentMcpControlTaskDeltaRefreshHost;

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

pub(in crate::daemon) struct SessionSubagentMcpControlTaskDeltaRefreshHost {
    workspace_stores: ProtectedWorkspaceStoreLookup,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
}

#[async_trait::async_trait]
impl SessionTaskDeltaRefreshHost for SessionSubagentMcpControlTaskDeltaRefreshHost {
    async fn emit_task_delta_refresh(&self, task_id: TaskId) {
        let store = match self.workspace_stores.store_for_task(task_id).await {
            Ok(store) => store,
            Err(err) => {
                tracing::warn!(
                    task_id = %task_id.0,
                    "subagent MCP control task delta refresh store lookup failed: {err:?}"
                );
                return;
            }
        };
        match store.get_workspace_active_task_summary(task_id).await {
            Ok(Some(summary)) => {
                let _ = self
                    .active_snapshot
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
                        .active_snapshot
                        .publish_task_delta(task.workspace_id, task, kind)
                        .await;
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(
                        task_id = %task_id.0,
                        "subagent MCP control task delta refresh task load failed: {err:?}"
                    );
                }
            },
            Err(err) => {
                tracing::warn!(
                    task_id = %task_id.0,
                    "subagent MCP control task delta refresh summary load failed: {err:?}"
                );
            }
        }
    }
}
