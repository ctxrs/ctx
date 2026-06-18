use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use ctx_core::ids::{SessionId, WorkspaceId};
use ctx_core::models::{Session, SessionEvent, SessionEventType, Workspace, Worktree};
use ctx_observability::perf_telemetry::{PerfMetric, PerfMetricKind, PerfTelemetry};
use ctx_observability::provider_unknown_events::{
    provider_unknown_event_hook, ProviderUnknownEventContext, ProviderUnknownEvents,
};
use ctx_provider_install::install_state::InstallTarget;
use ctx_providers::adapters::{
    ProviderAdapter, ProviderRunHooks, ProviderSessionRefClaimHook, ProviderUnknownEventHook,
};
use ctx_providers::ask_user_question::{AskUserQuestionAnswer, AskUserQuestionBroker};
use ctx_session_runtime::runtime::SessionRuntime;
use ctx_session_tools::interrupt_telemetry::{metric_labels, InterruptTelemetryContext};
use ctx_session_tools::order_seq::OrderSeqState;
use ctx_store::Store;
use tokio::sync::{mpsc, Mutex};

use crate::daemon::scheduler::SchedulerCommand;
use crate::daemon::session_route_handles::SessionMessageSchedulerSpawner;
use crate::daemon::sessions::ask_user::{SubmitAskUserAnswer, SubmitAskUserAnswerError};
use crate::daemon::sessions::auth::SessionAuthError;
use crate::daemon::sessions::command_dispatch::SessionSchedulerCommandError;
use crate::daemon::task_session_effects::SessionPublicationEffects;
use crate::daemon::ProviderWorkspaceLaunchRuntime;
use crate::daemon::{SessionStoreAccessError, SessionStoreLookup, StoreLookup};

pub(in crate::daemon) struct SessionControlHandleParts {
    pub(in crate::daemon) session_stores: SessionStoreLookup,
    pub(in crate::daemon) session_runtime: Arc<SessionRuntime<SchedulerCommand>>,
    pub(in crate::daemon) scheduler_spawner: SessionMessageSchedulerSpawner,
    pub(in crate::daemon) perf_telemetry: PerfTelemetry,
    pub(in crate::daemon) provider_launch: Arc<ProviderWorkspaceLaunchRuntime>,
    pub(in crate::daemon) session_publication: SessionPublicationEffects,
    pub(in crate::daemon) ask_user_question: Arc<AskUserQuestionBroker>,
    pub(in crate::daemon) provider_unknown_events: ProviderUnknownEvents,
}

#[derive(Clone)]
pub struct SessionControlHandle {
    command: SessionControlCommandHost,
    auth: SessionAuthHost,
    ask_user: SessionAskUserHost,
}

impl SessionControlHandle {
    pub(in crate::daemon) fn new(parts: SessionControlHandleParts) -> Self {
        let session_stores = parts.session_stores.clone();
        let auth_events = SessionAuthEventHost::new(
            Arc::clone(&parts.session_runtime),
            parts.session_publication.clone(),
            parts.perf_telemetry.clone(),
        );
        Self {
            command: SessionControlCommandHost::new(
                parts.session_stores.clone(),
                Arc::clone(&parts.session_runtime),
                parts.scheduler_spawner,
                parts.perf_telemetry,
            ),
            auth: SessionAuthHost::new(
                parts.session_stores.clone(),
                SessionAuthRuntimeHost::new(parts.provider_launch),
                auth_events,
                parts.provider_unknown_events,
            ),
            ask_user: SessionAskUserHost::new(
                parts.ask_user_question,
                parts.session_publication,
                session_stores,
            ),
        }
    }

    pub(in crate::daemon) async fn cancel_session(
        &self,
        session_id: SessionId,
    ) -> Result<(), SessionSchedulerCommandError> {
        self.command.cancel_session(session_id).await
    }

    pub(in crate::daemon) async fn interrupt_session(
        &self,
        session_id: SessionId,
        request_started: Instant,
    ) -> Result<(), SessionSchedulerCommandError> {
        self.command
            .interrupt_session(session_id, request_started)
            .await
    }

    pub(in crate::daemon) async fn authenticate_session(
        &self,
        session_id: SessionId,
        method_id: Option<String>,
    ) -> Result<(), SessionAuthError> {
        self.auth.authenticate_session(session_id, method_id).await
    }

    pub(in crate::daemon) async fn submit_ask_user_answer(
        &self,
        session_id: SessionId,
        submission: SubmitAskUserAnswer,
    ) -> Result<(), SubmitAskUserAnswerError> {
        self.ask_user
            .submit_ask_user_answer(session_id, submission)
            .await
    }
}

#[derive(Clone)]
pub(in crate::daemon) struct SessionControlCommandHost {
    session_stores: SessionStoreLookup,
    session_runtime: Arc<SessionRuntime<SchedulerCommand>>,
    scheduler_spawner: SessionMessageSchedulerSpawner,
    perf_telemetry: PerfTelemetry,
}

impl SessionControlCommandHost {
    fn new(
        session_stores: SessionStoreLookup,
        session_runtime: Arc<SessionRuntime<SchedulerCommand>>,
        scheduler_spawner: SessionMessageSchedulerSpawner,
        perf_telemetry: PerfTelemetry,
    ) -> Self {
        Self {
            session_stores,
            session_runtime,
            scheduler_spawner,
            perf_telemetry,
        }
    }

    pub(in crate::daemon) async fn cancel_session(
        &self,
        session_id: SessionId,
    ) -> Result<(), SessionSchedulerCommandError> {
        let (_store, session) = self.load_session_for_command(session_id).await?;
        let tx = self.ensure_scheduler(session).await;
        let _ = tx.send(SchedulerCommand::Cancel).await;
        Ok(())
    }

    pub(in crate::daemon) async fn interrupt_session(
        &self,
        session_id: SessionId,
        request_started: Instant,
    ) -> Result<(), SessionSchedulerCommandError> {
        let (store, session) = self.load_session_for_command(session_id).await?;
        let session_root_kind = match store.get_worktree(session.worktree_id).await {
            Ok(Some(worktree)) if worktree.vcs_ref.is_some() || worktree.git_branch.is_some() => {
                "worktree"
            }
            Ok(Some(_)) => "workspace_root",
            _ => "unknown",
        };
        let provider_id = session.provider_id.clone();
        let model_id = session.model_id.clone();
        let execution_environment = session.execution_environment;
        let tx = self.ensure_scheduler(session).await;
        let interrupt = InterruptTelemetryContext::new(uuid::Uuid::new_v4().to_string());
        let _ = tx
            .send(SchedulerCommand::Interrupt(interrupt.clone()))
            .await;
        let dispatch_ms = request_started.elapsed().as_millis() as u64;
        let metric = PerfMetric {
            name: "scheduler.interrupt_http_ms".to_string(),
            kind: PerfMetricKind::Histogram,
            unit: "ms".to_string(),
            value: dispatch_ms as f64,
            labels: metric_labels(
                &provider_id,
                &model_id,
                execution_environment.as_str(),
                session_root_kind,
                "http_dispatch",
            ),
        };
        self.perf_telemetry
            .record_metric(metric, None, None, None)
            .await;
        tracing::info!(
            session_id = %session_id.0,
            interrupt_id = %interrupt.interrupt_id(),
            provider_id = %provider_id,
            model_id = %model_id,
            dispatch_ms,
            "session interrupt dispatched"
        );
        Ok(())
    }

    async fn load_session_for_command(
        &self,
        session_id: SessionId,
    ) -> Result<(Store, Session), SessionSchedulerCommandError> {
        let store = self
            .session_stores
            .existing_session_store_for_write(session_id)
            .await
            .map_err(command_session_store_error)?;
        let session = store
            .get_session(session_id)
            .await
            .map_err(|_| SessionSchedulerCommandError::StoreUnavailable)?
            .ok_or(SessionSchedulerCommandError::NotFound)?;
        Ok((store, session))
    }

    async fn ensure_scheduler(&self, session: Session) -> mpsc::Sender<SchedulerCommand> {
        self.scheduler_spawner
            .ensure_scheduler(&self.session_runtime, session)
            .await
    }
}

#[derive(Clone)]
pub(in crate::daemon) struct SessionAuthHost {
    session_stores: SessionStoreLookup,
    auth_runtime: SessionAuthRuntimeHost,
    auth_events: SessionAuthEventHost,
    provider_unknown_events: ProviderUnknownEvents,
}

impl SessionAuthHost {
    fn new(
        session_stores: SessionStoreLookup,
        auth_runtime: SessionAuthRuntimeHost,
        auth_events: SessionAuthEventHost,
        provider_unknown_events: ProviderUnknownEvents,
    ) -> Self {
        Self {
            session_stores,
            auth_runtime,
            auth_events,
            provider_unknown_events,
        }
    }

    pub(in crate::daemon) async fn authenticate_session(
        &self,
        session_id: SessionId,
        method_id: Option<String>,
    ) -> Result<(), SessionAuthError> {
        let store = self
            .session_stores
            .existing_session_store_for_write(session_id)
            .await
            .map_err(session_store_access_auth_error)?;
        let session = store
            .get_session(session_id)
            .await
            .map_err(|_| SessionAuthError::Internal("failed to load session".to_string()))?
            .ok_or(SessionAuthError::NotFound("session"))?;
        self.authenticate_loaded_session(&store, &session, method_id)
            .await
    }

    async fn authenticate_loaded_session(
        &self,
        store: &Store,
        session: &Session,
        method_id: Option<String>,
    ) -> Result<(), SessionAuthError> {
        let prepared = crate::daemon::sessions::auth::runtime::prepare_session_auth_runtime(
            &self.auth_runtime,
            store,
            session,
        )
        .await?;
        let event_sender = crate::daemon::sessions::auth::events::spawn_session_auth_event_sink(
            self.auth_events.clone(),
            store.clone(),
            session.id,
        );

        crate::daemon::sessions::auth::events::append_auth_notice(
            &self.auth_events,
            store,
            session.id,
            serde_json::json!({
                "kind": "auth_started",
                "provider": session.provider_id,
                "method_id": method_id,
            }),
        )
        .await?;

        let provider_unknown_event = self.provider_unknown_event_hook(session);
        let result = prepared
            .adapter
            .authenticate_session(
                session.id.0.to_string(),
                prepared.workdir,
                prepared.provider_env,
                method_id,
                event_sender,
                ProviderRunHooks {
                    provider_session_ref_claim: Some(provider_session_claim_hook(
                        store.clone(),
                        session.id,
                    )),
                    provider_unknown_event: Some(provider_unknown_event),
                },
            )
            .await;

        match result {
            Ok(()) => {
                crate::daemon::sessions::auth::events::append_auth_notice(
                    &self.auth_events,
                    store,
                    session.id,
                    serde_json::json!({
                        "kind": "auth_finished",
                        "provider": session.provider_id,
                    }),
                )
                .await?;
                Ok(())
            }
            Err(error) => {
                let redacted_message =
                    ctx_observability::logs::redact_sensitive(&error.to_string());
                crate::daemon::sessions::auth::events::append_auth_notice(
                    &self.auth_events,
                    store,
                    session.id,
                    serde_json::json!({
                        "kind": "auth_failed",
                        "provider": session.provider_id,
                        "message": redacted_message,
                    }),
                )
                .await?;
                Err(SessionAuthError::AuthenticationFailed { redacted_message })
            }
        }
    }

    fn provider_unknown_event_hook(&self, session: &Session) -> ProviderUnknownEventHook {
        provider_unknown_event_hook(
            self.provider_unknown_events.clone(),
            ProviderUnknownEventContext {
                provider_id: session.provider_id.clone(),
                execution_environment: Some(session.execution_environment.as_str().to_string()),
                session_root_kind: None,
                operation: "auth".to_string(),
            },
        )
    }
}

#[derive(Clone)]
pub(in crate::daemon) struct SessionAuthRuntimeHost {
    provider_launch: Arc<ProviderWorkspaceLaunchRuntime>,
}

impl SessionAuthRuntimeHost {
    fn new(provider_launch: Arc<ProviderWorkspaceLaunchRuntime>) -> Self {
        Self { provider_launch }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        self.provider_launch.data_root()
    }

    pub(in crate::daemon) async fn load_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Option<Workspace>> {
        self.provider_launch.load_workspace(workspace_id).await
    }

    pub(in crate::daemon) async fn resolve_existing_worktree_execution(
        &self,
        store: &Store,
        worktree_id: ctx_core::ids::WorktreeId,
    ) -> Result<crate::daemon::workspaces::ResolvedExistingWorktreeExecution> {
        self.provider_launch
            .resolve_existing_worktree_execution(store, worktree_id)
            .await
    }

    pub(in crate::daemon) async fn effective_install_target_for_environment(
        &self,
        workspace_id: WorkspaceId,
        execution_environment: ctx_core::models::ExecutionEnvironment,
    ) -> Result<InstallTarget> {
        self.provider_launch
            .effective_install_target_for_environment(workspace_id, execution_environment)
            .await
    }

    pub(in crate::daemon) async fn ensure_provider_adapter_for_target_with_cfg(
        &self,
        cfg: &ctx_managed_installs::AgentServerConfigFile,
        provider_id: &str,
        target: InstallTarget,
    ) -> Arc<dyn ProviderAdapter> {
        self.provider_launch.sync_plugin_provider_adapters().await;
        ctx_provider_runtime::provider_launch::resolver::ensure_provider_adapter_for_target_with_cfg(
            self.provider_launch.as_ref(),
            cfg,
            provider_id,
            target,
        )
        .await
    }

    pub(in crate::daemon) async fn provider_auth_context_for_worktree_runtime(
        &self,
        worktree: &Worktree,
        provider_id: &str,
    ) -> Result<ctx_provider_runtime::provider_launch::probe::WorkspaceRuntimeProbeContext, String>
    {
        ctx_provider_runtime::provider_launch::probe::provider_auth_context_for_worktree_runtime(
            self.provider_launch.as_ref(),
            worktree,
            provider_id,
        )
        .await
    }
}

#[derive(Clone)]
pub(in crate::daemon) struct SessionAuthEventHost {
    session_runtime: Arc<SessionRuntime<SchedulerCommand>>,
    session_publication: SessionPublicationEffects,
    perf_telemetry: PerfTelemetry,
}

impl SessionAuthEventHost {
    fn new(
        session_runtime: Arc<SessionRuntime<SchedulerCommand>>,
        session_publication: SessionPublicationEffects,
        perf_telemetry: PerfTelemetry,
    ) -> Self {
        Self {
            session_runtime,
            session_publication,
            perf_telemetry,
        }
    }

    pub(in crate::daemon) async fn session_order_seq_state(
        &self,
        store: &Store,
        session_id: SessionId,
    ) -> Arc<Mutex<OrderSeqState>> {
        self.session_runtime
            .get_order_seq_state(store, session_id)
            .await
    }

    pub(in crate::daemon) async fn publish_event(&self, event: SessionEvent) {
        self.session_publication.publish_event(event).await;
    }

    pub(in crate::daemon) async fn emit_compat_payload_reject_counter(
        &self,
        surface: &str,
        issue: &str,
        extra_label: Option<(&str, &str)>,
    ) {
        let mut labels = HashMap::new();
        labels.insert("source".to_string(), "daemon".to_string());
        labels.insert("surface".to_string(), surface.to_string());
        labels.insert("issue".to_string(), issue.to_string());
        if let Some((key, value)) = extra_label {
            labels.insert(key.to_string(), value.to_string());
        }
        self.perf_telemetry
            .record_metric(
                PerfMetric {
                    name: "compat.payload_reject_count".to_string(),
                    kind: PerfMetricKind::Counter,
                    unit: "count".to_string(),
                    value: 1.0,
                    labels,
                },
                None,
                None,
                None,
            )
            .await;
    }
}

#[derive(Clone)]
pub(in crate::daemon) struct SessionAskUserHost {
    ask_user_question: Arc<AskUserQuestionBroker>,
    session_publication: SessionPublicationEffects,
    session_stores: SessionStoreLookup,
}

impl SessionAskUserHost {
    fn new(
        ask_user_question: Arc<AskUserQuestionBroker>,
        session_publication: SessionPublicationEffects,
        session_stores: SessionStoreLookup,
    ) -> Self {
        Self {
            ask_user_question,
            session_publication,
            session_stores,
        }
    }

    pub(in crate::daemon) async fn submit_ask_user_answer(
        &self,
        session_id: SessionId,
        submission: SubmitAskUserAnswer,
    ) -> Result<(), SubmitAskUserAnswerError> {
        let store = self.store_for_ask_user_session(session_id).await?;
        if store
            .get_session(session_id)
            .await
            .map_err(|_| SubmitAskUserAnswerError::LoadSession)?
            .is_none()
        {
            return Err(SubmitAskUserAnswerError::SessionNotFound);
        }

        let tool_call_id = submission.tool_call_id.trim().to_string();
        if tool_call_id.is_empty() {
            return Err(SubmitAskUserAnswerError::MissingToolCallId);
        }

        let answers_for_event = submission.answers.clone();
        let ok = self
            .ask_user_question
            .submit(
                &session_id.0.to_string(),
                &tool_call_id,
                AskUserQuestionAnswer {
                    outcome: submission.outcome,
                    answers: submission.answers,
                },
            )
            .await;

        if !ok {
            return Err(SubmitAskUserAnswerError::NoPendingQuestion);
        }

        if let Ok(event) = store
            .append_session_event(
                session_id,
                None,
                None,
                SessionEventType::Notice,
                serde_json::json!({
                    "kind": "ask_user_question_answered",
                    "tool_call_id": tool_call_id,
                    "outcome": submission.outcome.as_str(),
                    "answers": answers_for_event,
                }),
            )
            .await
        {
            self.session_publication.publish_event(event).await;
        }

        Ok(())
    }

    async fn store_for_ask_user_session(
        &self,
        session_id: SessionId,
    ) -> Result<Store, SubmitAskUserAnswerError> {
        let store = match self.session_stores.lookup_session_store(session_id).await {
            StoreLookup::Found(store) => store,
            StoreLookup::Missing | StoreLookup::Deleting => {
                return Err(SubmitAskUserAnswerError::SessionNotFound);
            }
            StoreLookup::Unavailable(err) => {
                return Err(SubmitAskUserAnswerError::StoreUnavailable(err));
            }
        };
        if store
            .is_archived_subagent_session(session_id)
            .await
            .map_err(SubmitAskUserAnswerError::StoreUnavailable)?
        {
            return Err(SubmitAskUserAnswerError::SessionNotFound);
        }
        Ok(store)
    }
}

fn provider_session_claim_hook(store: Store, session_id: SessionId) -> ProviderSessionRefClaimHook {
    Arc::new(move |claim| {
        let store = store.clone();
        Box::pin(async move {
            if let Some(returned_ref) = claim.returned_provider_session_ref {
                store
                    .claim_session_provider_session_ref(
                        session_id,
                        returned_ref,
                        "provider.session_opened.auth",
                    )
                    .await?;
            }
            Ok(())
        })
    })
}

fn session_store_access_auth_error(error: SessionStoreAccessError) -> SessionAuthError {
    match error {
        SessionStoreAccessError::NotFound => SessionAuthError::NotFound("session"),
        SessionStoreAccessError::LookupUnavailable(error) => {
            SessionAuthError::Internal(error.to_string())
        }
        SessionStoreAccessError::StoreUnavailable => {
            SessionAuthError::Internal("workspace store unavailable".to_string())
        }
    }
}

fn command_session_store_error(error: SessionStoreAccessError) -> SessionSchedulerCommandError {
    match error {
        SessionStoreAccessError::NotFound => SessionSchedulerCommandError::NotFound,
        SessionStoreAccessError::LookupUnavailable(_)
        | SessionStoreAccessError::StoreUnavailable => {
            SessionSchedulerCommandError::StoreUnavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use ctx_core::models::{ExecutionEnvironment, VcsKind};
    use ctx_providers::adapters::ProviderSessionRefClaim;
    use ctx_providers::events::NormalizedEvent;
    use ctx_store::StoreManager;

    use crate::daemon::task_session_effects::TaskPublicationHost;
    use crate::daemon::{route_handles_from_state, DaemonState, ProtectedWorkspaceStoreLookup};

    struct SessionFixture {
        store: Store,
        session: Session,
    }

    async fn test_state(root: &std::path::Path) -> Arc<DaemonState> {
        Arc::new(DaemonState::new(
            root.to_path_buf(),
            StoreManager::open(root).await.unwrap(),
            HashMap::new(),
            "http://127.0.0.1:4399".to_string(),
            Some("daemon-secret".to_string()),
        ))
    }

    async fn create_session(
        state: Arc<DaemonState>,
        root: &std::path::Path,
        name: &str,
        git_branch: Option<String>,
    ) -> SessionFixture {
        let workspace = state
            .global_store()
            .create_workspace(
                name.to_string(),
                root.join(format!("workspace-{name}"))
                    .to_string_lossy()
                    .to_string(),
                VcsKind::Git,
            )
            .await
            .unwrap();
        let store = state.store_for_workspace(workspace.id).await.unwrap();
        let worktree = store
            .create_worktree(
                workspace.id,
                root.join(format!("worktree-{name}"))
                    .to_string_lossy()
                    .to_string(),
                "deadbeef".to_string(),
                git_branch,
            )
            .await
            .unwrap();
        state
            .global_store()
            .upsert_workspace_worktree_index(worktree.id, workspace.id)
            .await
            .unwrap();
        let task = store
            .create_task(workspace.id, format!("task-{name}"), None)
            .await
            .unwrap();
        let session = store
            .create_session(
                task.id,
                workspace.id,
                worktree.id,
                ExecutionEnvironment::Host,
                "fake".to_string(),
                "model".to_string(),
                "implementer".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        state
            .global_store()
            .upsert_workspace_session_index(session.id, workspace.id)
            .await
            .unwrap();
        SessionFixture { store, session }
    }

    fn auth_event_host(state: &Arc<DaemonState>) -> SessionAuthEventHost {
        let workspace_stores = ProtectedWorkspaceStoreLookup::new(
            state.core.stores.clone(),
            Arc::clone(&state.sessions),
            Arc::clone(&state.transport.merge_queue),
        );
        let session_stores =
            SessionStoreLookup::new(state.global_store().clone(), workspace_stores.clone());
        let task_publication = Arc::new(TaskPublicationHost::new(
            workspace_stores,
            Arc::clone(&state.workspaces.workspace_active_snapshot),
        ));
        let publication = SessionPublicationEffects::new(
            Arc::clone(&state.sessions),
            session_stores,
            task_publication,
        );
        SessionAuthEventHost::new(
            Arc::clone(&state.sessions),
            publication,
            state.telemetry.perf_telemetry.clone(),
        )
    }

    async fn wait_for_event_kind(
        store: &Store,
        session_id: SessionId,
        kind: &str,
    ) -> serde_json::Value {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let events = store.list_session_events(session_id).await.unwrap();
                if let Some(event) = events.iter().find(|event| {
                    event
                        .payload_json
                        .get("kind")
                        .and_then(|value| value.as_str())
                        == Some(kind)
                }) {
                    return event.payload_json.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn provider_session_claim_hook_claims_returned_ref() {
        let root = tempfile::tempdir().unwrap();
        let state = test_state(root.path()).await;
        let fixture = create_session(state, root.path(), "claim-hook", None).await;

        let hook = provider_session_claim_hook(fixture.store.clone(), fixture.session.id);
        hook(ProviderSessionRefClaim {
            requested_provider_session_ref: None,
            returned_provider_session_ref: Some("provider-thread-1".to_string()),
        })
        .await
        .unwrap();

        let session = fixture
            .store
            .get_session(fixture.session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            session.provider_session_ref.as_deref(),
            Some("provider-thread-1")
        );
    }

    #[tokio::test]
    async fn auth_event_sink_claims_provider_session_id_from_init_event() {
        let root = tempfile::tempdir().unwrap();
        let state = test_state(root.path()).await;
        let fixture = create_session(Arc::clone(&state), root.path(), "event-claim", None).await;
        let sender = crate::daemon::sessions::auth::events::spawn_session_auth_event_sink(
            auth_event_host(&state),
            fixture.store.clone(),
            fixture.session.id,
        );

        sender
            .send(NormalizedEvent {
                event_type: SessionEventType::Init,
                payload_json: serde_json::json!({
                    "provider_session_id": "provider-thread-2",
                }),
            })
            .await
            .unwrap();

        let session = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let session = fixture
                    .store
                    .get_session(fixture.session.id)
                    .await
                    .unwrap()
                    .unwrap();
                if session.provider_session_ref.as_deref() == Some("provider-thread-2") {
                    return session;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            session.provider_session_ref.as_deref(),
            Some("provider-thread-2")
        );
    }

    #[tokio::test]
    async fn auth_event_sink_rewrites_provider_ref_claim_failure_to_notice() {
        let root = tempfile::tempdir().unwrap();
        let state = test_state(root.path()).await;
        let owner = create_session(Arc::clone(&state), root.path(), "owner", None).await;
        owner
            .store
            .claim_session_provider_session_ref(
                owner.session.id,
                "shared-thread".to_string(),
                "test",
            )
            .await
            .unwrap();
        let peer_task = owner
            .store
            .create_task(owner.session.workspace_id, "peer-task".to_string(), None)
            .await
            .unwrap();
        let peer_session = owner
            .store
            .create_session(
                peer_task.id,
                owner.session.workspace_id,
                owner.session.worktree_id,
                ExecutionEnvironment::Host,
                "fake".to_string(),
                "model".to_string(),
                "implementer".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        state
            .global_store()
            .upsert_workspace_session_index(peer_session.id, peer_session.workspace_id)
            .await
            .unwrap();
        let sender = crate::daemon::sessions::auth::events::spawn_session_auth_event_sink(
            auth_event_host(&state),
            owner.store.clone(),
            peer_session.id,
        );

        sender
            .send(NormalizedEvent {
                event_type: SessionEventType::Init,
                payload_json: serde_json::json!({
                    "provider_session_id": "shared-thread",
                }),
            })
            .await
            .unwrap();

        let notice = wait_for_event_kind(
            &owner.store,
            peer_session.id,
            "provider_session_ref_claim_failed",
        )
        .await;
        assert_eq!(
            notice.get("reason").and_then(|value| value.as_str()),
            Some("provider_session_ref_claim_failed")
        );
    }

    #[tokio::test]
    async fn auth_event_sink_attaches_order_seq_selectively_and_counts_legacy_init_key() {
        let root = tempfile::tempdir().unwrap();
        let state = test_state(root.path()).await;
        let fixture = create_session(Arc::clone(&state), root.path(), "order-seq", None).await;
        let sender = crate::daemon::sessions::auth::events::spawn_session_auth_event_sink(
            auth_event_host(&state),
            fixture.store.clone(),
            fixture.session.id,
        );

        sender
            .send(NormalizedEvent {
                event_type: SessionEventType::ToolCall,
                payload_json: serde_json::json!({
                    "tool_call_id": "tool-1",
                }),
            })
            .await
            .unwrap();
        sender
            .send(NormalizedEvent {
                event_type: SessionEventType::Init,
                payload_json: serde_json::json!({
                    "crp_session_id": "legacy-thread",
                }),
            })
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let events = fixture
                    .store
                    .list_session_events(fixture.session.id)
                    .await
                    .unwrap();
                let tool = events
                    .iter()
                    .find(|event| matches!(event.event_type, SessionEventType::ToolCall));
                let init = events.iter().find(|event| {
                    matches!(event.event_type, SessionEventType::Init)
                        && event
                            .payload_json
                            .get("crp_session_id")
                            .and_then(|value| value.as_str())
                            == Some("legacy-thread")
                });
                if let (Some(tool), Some(init)) = (tool, init) {
                    assert!(tool.payload_json.get("order_seq").is_some());
                    assert!(init.payload_json.get("order_seq").is_none());
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let summary = state.telemetry.perf_telemetry.summary(
            Some("compat.payload_reject_count"),
            None,
            None,
            None,
        );
        assert!(summary.metrics.iter().any(|metric| {
            metric.metric.labels.get("surface").map(String::as_str)
                == Some("sessions.auth_event_init")
                && metric.metric.labels.get("issue").map(String::as_str) == Some("crp_session_id")
        }));
    }

    #[tokio::test]
    async fn session_control_interrupt_records_root_kind_telemetry() {
        let root = tempfile::tempdir().unwrap();
        let state = test_state(root.path()).await;
        let fixture = create_session(
            Arc::clone(&state),
            root.path(),
            "interrupt",
            Some("feature/test".to_string()),
        )
        .await;
        let handle = route_handles_from_state(&state).session_control;

        handle
            .interrupt_session(
                fixture.session.id,
                Instant::now()
                    .checked_sub(Duration::from_millis(5))
                    .unwrap_or_else(Instant::now),
            )
            .await
            .unwrap();

        let summary = state.telemetry.perf_telemetry.summary(
            Some("scheduler.interrupt_http_ms"),
            None,
            None,
            None,
        );
        assert!(summary.metrics.iter().any(|metric| {
            metric.metric.labels.get("provider_id").map(String::as_str) == Some("fake")
                && metric.metric.labels.get("model_id").map(String::as_str) == Some("model")
                && metric
                    .metric
                    .labels
                    .get("execution_environment")
                    .map(String::as_str)
                    == Some("host")
                && metric
                    .metric
                    .labels
                    .get("session_root_kind")
                    .map(String::as_str)
                    == Some("worktree")
                && metric.metric.labels.get("event").map(String::as_str) == Some("http_dispatch")
        }));
    }

    #[tokio::test]
    async fn session_control_cancel_and_interrupt_dispatch_scheduler_commands() {
        let root = tempfile::tempdir().unwrap();
        let state = test_state(root.path()).await;
        let fixture = create_session(Arc::clone(&state), root.path(), "dispatch", None).await;
        let handle = route_handles_from_state(&state);
        let (observed_tx, mut observed_rx) = tokio::sync::mpsc::unbounded_channel();
        let _scheduler_tx = handle
            .session_message_command
            .ensure_scheduler_for_test(
                fixture.session.clone(),
                move |_session, mut rx| async move {
                    for _ in 0..2 {
                        let Some(command) = rx.recv().await else {
                            return;
                        };
                        let _ = observed_tx.send(command);
                    }
                },
            )
            .await;
        let session_control = handle.session_control;

        session_control
            .cancel_session(fixture.session.id)
            .await
            .unwrap();
        session_control
            .interrupt_session(fixture.session.id, Instant::now())
            .await
            .unwrap();

        let cancel = tokio::time::timeout(Duration::from_secs(2), observed_rx.recv())
            .await
            .unwrap()
            .unwrap();
        let interrupt = tokio::time::timeout(Duration::from_secs(2), observed_rx.recv())
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(cancel, SchedulerCommand::Cancel));
        assert!(matches!(interrupt, SchedulerCommand::Interrupt(_)));
    }
}
