use ctx_core::ids::{ContributionId, SessionId, TaskId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    Contribution, ContributionEndpoint, ContributionRole, ExecutionEnvironment, Message,
    RecordFidelity, RecordOrigin, RecordSource, RecordTrust, Session, Task, VcsKind, Workspace,
    Worktree,
};
use ctx_observability::ops_events::OpsEvent;
use ctx_observability::perf_telemetry::{PerfMetric, PerfMetricKind};
use ctx_observability::telemetry::TelemetryEvent;
pub use ctx_session_service::session_creation::DefaultSessionSeed;
use ctx_session_service::session_creation::{
    session_matches_creation_identity, SessionCreationIdentity,
};
use ctx_session_tools::model_resolution::{compose_model_id, resolve_model_id};
use ctx_settings_model::ExecutionSettings;
use ctx_store::Store;
use std::collections::HashMap;
use std::path::Path as StdPath;

use crate::daemon::scheduler::SchedulerCommand;
use crate::daemon::task_route_handles::TaskSessionAdmissionHandle;
use crate::daemon::workspaces::{
    execution_environment_from_settings, retry_global_index_write, BranchCleanupErrorMode,
    TaskWorktreeCleanupTarget,
};

#[path = "create_session/cleanup.rs"]
mod cleanup;
#[path = "create_session/existing.rs"]
mod existing;
#[path = "create_session/initial_prompt.rs"]
mod initial_prompt;
#[path = "create_session/loaded.rs"]
mod loaded;
#[path = "create_session/persistence.rs"]
mod persistence;
#[path = "create_session/telemetry.rs"]
mod telemetry;
#[path = "create_session/worktree.rs"]
mod worktree;

use cleanup::cleanup_orphaned_provisioned_worktree;
use existing::{resolve_existing_requested_session, ExistingRequestedSession};
use initial_prompt::{seed_initial_prompt, InitialPromptSeed};
use loaded::create_session_for_loaded_task_inner;
use telemetry::emit_session_started_observability;
use worktree::resolve_session_worktree_for_task;

#[derive(Debug, Clone)]
pub struct CreateTaskSessionInput {
    pub id: Option<String>,
    pub provider_id: String,
    pub model_id: String,
    pub reasoning_effort: Option<String>,
    pub remember_model_preference: bool,
    pub parent_session_id: Option<String>,
    pub relationship: Option<String>,
    pub initial_prompt: Option<String>,
    pub initial_message_id: Option<String>,
    pub initial_turn_id: Option<String>,
    pub worktree_id: Option<String>,
    pub execution_environment: Option<ExecutionEnvironment>,
    pub run_id_header: Option<String>,
}

impl CreateTaskSessionInput {
    pub fn from_default_seed(seed: DefaultSessionSeed) -> Self {
        Self {
            id: None,
            provider_id: seed.provider_id,
            model_id: seed.model_id,
            reasoning_effort: seed.reasoning_effort,
            remember_model_preference: false,
            parent_session_id: None,
            relationship: None,
            initial_prompt: None,
            initial_message_id: None,
            initial_turn_id: None,
            worktree_id: None,
            execution_environment: Some(seed.execution_environment),
            run_id_header: None,
        }
    }
}

#[derive(Debug)]
pub enum TaskSessionCreateError {
    BadRequest,
    NotFound,
    Conflict,
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for TaskSessionCreateError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

#[derive(Clone)]
struct TaskSessionHandles {
    admission: TaskSessionAdmissionHandle,
}

impl TaskSessionHandles {
    fn new(handle: &TaskSessionAdmissionHandle) -> Self {
        Self {
            admission: handle.clone(),
        }
    }
}

fn task_session_contribution_id(task_id: TaskId, session_id: SessionId) -> ContributionId {
    ContributionId::from_id(format!(
        "con_task_session_{}_{}",
        task_id.0.simple(),
        session_id.0.simple()
    ))
}

fn session_worktree_contribution_id(
    session_id: SessionId,
    worktree_id: WorktreeId,
) -> ContributionId {
    ContributionId::from_id(format!(
        "con_session_{}_worktree_{}",
        session_id.0.simple(),
        worktree_id.0.simple()
    ))
}

fn parent_child_session_contribution_id(
    parent_session_id: SessionId,
    child_session_id: SessionId,
) -> ContributionId {
    ContributionId::from_id(format!(
        "con_parent_session_{}_child_{}",
        parent_session_id.0.simple(),
        child_session_id.0.simple()
    ))
}

impl TaskSessionAdmissionHandle {
    pub(in crate::daemon) async fn task_session_creation_lock(
        &self,
        task_id: TaskId,
    ) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        self.sessions().task_session_creation_lock(task_id).await
    }

    pub(in crate::daemon) async fn get_workspace_id_for_task(
        &self,
        task_id: TaskId,
    ) -> anyhow::Result<Option<WorkspaceId>> {
        self.global_store().get_workspace_id_for_task(task_id).await
    }

    pub(in crate::daemon) async fn get_workspace_id_for_session(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<Option<WorkspaceId>> {
        self.global_store()
            .get_workspace_id_for_session(session_id)
            .await
    }

    pub(in crate::daemon) async fn upsert_workspace_session_index(
        &self,
        session_id: SessionId,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<()> {
        self.global_store()
            .upsert_workspace_session_index(session_id, workspace_id)
            .await
    }

    pub(in crate::daemon) async fn delete_workspace_worktree_index(
        &self,
        worktree_id: WorktreeId,
    ) -> anyhow::Result<()> {
        self.global_store()
            .delete_workspace_worktree_index(worktree_id)
            .await
    }

    pub(in crate::daemon) async fn load_task_context(
        &self,
        task_id: TaskId,
    ) -> Result<Option<(Store, Task, Workspace)>, super::TaskLifecycleError> {
        let Some(workspace_id) = self
            .global_store()
            .get_workspace_id_for_task(task_id)
            .await
            .map_err(super::TaskLifecycleError::Internal)?
        else {
            return Ok(None);
        };
        let store = self
            .store_for_workspace(workspace_id)
            .await
            .map_err(super::TaskLifecycleError::Internal)?;
        let Some(task) = store
            .get_task(task_id)
            .await
            .map_err(super::TaskLifecycleError::Internal)?
        else {
            return Ok(None);
        };
        let workspace = self
            .global_store()
            .get_workspace(task.workspace_id)
            .await
            .map_err(super::TaskLifecycleError::Internal)?;
        let Some(workspace) = workspace else {
            return Ok(None);
        };
        Ok(Some((store, task, workspace)))
    }

    pub(in crate::daemon) async fn remember_session_meta(&self, session: &Session) {
        self.sessions().remember_session_meta(session).await;
    }

    pub(in crate::daemon) async fn publish_event(&self, event: ctx_core::models::SessionEvent) {
        self.effects().publish_event(event).await;
    }

    pub(in crate::daemon) async fn session_order_seq_state(
        &self,
        store: &Store,
        session_id: SessionId,
    ) -> std::sync::Arc<tokio::sync::Mutex<ctx_session_tools::order_seq::OrderSeqState>> {
        self.sessions().get_order_seq_state(store, session_id).await
    }

    pub(in crate::daemon) async fn ensure_scheduler(
        &self,
        session: Session,
    ) -> tokio::sync::mpsc::Sender<SchedulerCommand> {
        self.effects().ensure_scheduler(session).await
    }

    pub(in crate::daemon) async fn schedule_session_title_generation(
        &self,
        session: Session,
        prompt: String,
        force: bool,
    ) -> bool {
        self.effects()
            .schedule_title_generation(session, prompt, force)
            .await
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
        let metric = PerfMetric {
            name: "compat.payload_reject_count".to_string(),
            kind: PerfMetricKind::Counter,
            unit: "count".to_string(),
            value: 1.0,
            labels,
        };
        self.perf_telemetry()
            .record_metric(metric, None, None, None)
            .await;
    }

    pub(in crate::daemon) async fn effective_execution_settings(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<ExecutionSettings> {
        let store = self.store_for_workspace(workspace_id).await?;
        ctx_settings_service::effective_execution_settings(self.global_store(), &store).await
    }

    pub(in crate::daemon) async fn install_target_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<ctx_provider_install::install_state::InstallTarget> {
        let effective = self.effective_execution_settings(workspace_id).await?;
        Ok(ctx_settings_service::install_target_for_settings(
            &effective,
        ))
    }

    pub(in crate::daemon) async fn providers_statuses_response(
        &self,
        target: ctx_provider_install::install_state::InstallTarget,
        include_matrix_providers: bool,
    ) -> Vec<ctx_providers::adapters::ProviderStatus> {
        ctx_provider_runtime::provider_status_service::providers_statuses_response(
            self.provider_status(),
            target,
            include_matrix_providers,
        )
        .await
    }

    pub(in crate::daemon) async fn refresh_provider_statuses(&self) -> anyhow::Result<()> {
        ctx_provider_runtime::provider_status_service::refresh_provider_statuses(
            self.provider_status(),
        )
        .await
    }

    pub(in crate::daemon) async fn can_create_loaded_session_for_provider(
        &self,
        provider_id: &str,
    ) -> bool {
        self.provider_status().sync_plugin_provider_adapters().await;
        self.providers()
            .can_create_loaded_session_for_provider(provider_id)
            .await
    }

    pub(in crate::daemon) async fn update_workspace_provider_preferred_model_id(
        &self,
        workspace_id: WorkspaceId,
        provider_id: &str,
        preferred_model_id: Option<String>,
    ) -> anyhow::Result<()> {
        let store = self.store_for_workspace(workspace_id).await?;
        ctx_workspace_config::update_preferred_new_session_model_id(
            &store,
            provider_id,
            preferred_model_id,
        )
        .await?;
        ctx_provider_runtime::provider_cache::invalidate_workspace_provider_options_cache(
            self.providers(),
            workspace_id,
            provider_id,
        )
        .await;
        Ok(())
    }

    pub(in crate::daemon) async fn resolve_existing_worktree_execution(
        &self,
        store: &Store,
        workspace: &Workspace,
        worktree_id: WorktreeId,
    ) -> anyhow::Result<crate::daemon::workspaces::ResolvedExistingWorktreeExecution> {
        self.workspace()
            .resolve_existing_worktree_execution(store, workspace, worktree_id)
            .await
    }

    pub(in crate::daemon) async fn provision_worktree_for_execution(
        &self,
        workspace: &Workspace,
        worktree_id: WorktreeId,
        base_commit_sha: &str,
        branch_name: &str,
        effective: &ExecutionSettings,
    ) -> anyhow::Result<(std::path::PathBuf, Option<ctx_core::models::SandboxBinding>)> {
        self.workspace()
            .provision_worktree_for_execution(
                workspace,
                worktree_id,
                base_commit_sha,
                branch_name,
                effective,
            )
            .await
    }

    pub(in crate::daemon) async fn persist_provisioned_worktree(
        &self,
        store: &Store,
        workspace: &Workspace,
        worktree: Worktree,
        sandbox_binding: Option<ctx_core::models::SandboxBinding>,
    ) -> anyhow::Result<Worktree> {
        self.workspace()
            .persist_provisioned_worktree(store, workspace, worktree, sandbox_binding)
            .await
    }

    pub(in crate::daemon) fn managed_worktree_root(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
    ) -> Option<std::path::PathBuf> {
        self.workspace().managed_worktree_root(workspace, worktree)
    }

    pub(in crate::daemon) async fn cleanup_task_worktrees(
        &self,
        workspace: &Workspace,
        task_id: TaskId,
        targets: &[TaskWorktreeCleanupTarget],
        mode: BranchCleanupErrorMode,
    ) -> Vec<anyhow::Error> {
        self.workspace()
            .cleanup_task_worktrees(workspace, task_id, targets, mode)
            .await
    }

    pub(in crate::daemon) async fn ensure_task_commit_hook(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
        task_id: TaskId,
    ) -> anyhow::Result<()> {
        self.workspace()
            .ensure_task_commit_hook(workspace, worktree, task_id)
            .await
    }

    pub(in crate::daemon) async fn emit_workspace_task_upsert(
        &self,
        task_id: TaskId,
    ) -> anyhow::Result<()> {
        self.effects().emit_workspace_task_upsert(task_id).await
    }

    pub(in crate::daemon) async fn emit_session_started_observability_for_task(
        &self,
        session: &Session,
        task: &Task,
    ) {
        let worktree = match self.store_for_workspace(session.workspace_id).await {
            Ok(store) => store.get_worktree(session.worktree_id).await.ok().flatten(),
            Err(_) => None,
        };
        let session_root_kind = match worktree
            .as_ref()
            .and_then(|worktree| worktree.git_branch.as_ref())
        {
            Some(_) => "worktree",
            None => "workspace_root",
        };
        self.telemetry()
            .emit(TelemetryEvent::session_started(
                session.provider_id.clone(),
                compose_model_id(&session.model_id, session.reasoning_effort.as_deref()),
                Some(session.execution_environment.as_str().to_string()),
                Some(session_root_kind.to_string()),
            ))
            .await;
        let mut ops_event = OpsEvent::new("info", "session_started");
        ops_event.session_id = Some(session.id.0.to_string());
        ops_event.worktree_id = Some(session.worktree_id.0.to_string());
        ops_event.provider_id = Some(session.provider_id.clone());
        ops_event.meta = Some(serde_json::json!({
            "model_id": compose_model_id(&session.model_id, session.reasoning_effort.as_deref()),
            "reasoning_effort": session.reasoning_effort.clone(),
            "execution_environment": session.execution_environment.as_str(),
            "session_root_kind": session_root_kind,
            "parent_session_id": session.parent_session_id.map(|id| id.0.to_string()),
            "relationship": session.relationship.clone(),
        }));
        self.ops_events().emit(ops_event);
        if let Err(error) = self.emit_workspace_task_upsert(task.id).await {
            tracing::warn!(task_id = %task.id.0, "workspace active snapshot refresh failed: {error:?}");
        }
    }

    pub(in crate::daemon) async fn record_session_created_agent_work(
        &self,
        store: &Store,
        task: &Task,
        session: &Session,
    ) -> anyhow::Result<()> {
        if session.workspace_id != task.workspace_id || session.task_id != task.id {
            anyhow::bail!("session does not belong to the task being linked");
        }
        let worktree = store
            .get_worktree(session.worktree_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session worktree is missing"))?;
        if worktree.workspace_id != task.workspace_id {
            anyhow::bail!("session worktree belongs to a different workspace");
        }
        let parent_session_id = if let Some(parent_session_id) = session.parent_session_id {
            match store.get_session(parent_session_id).await? {
                Some(parent_session) if parent_session.workspace_id == task.workspace_id => {
                    Some(parent_session_id)
                }
                Some(_) => {
                    tracing::warn!(
                        session_id = %session.id.0,
                        parent_session_id = %parent_session_id.0,
                        "skipping agent-work parent session link for cross-workspace parent"
                    );
                    None
                }
                None => {
                    tracing::warn!(
                        session_id = %session.id.0,
                        parent_session_id = %parent_session_id.0,
                        "skipping agent-work parent session link for unresolved parent"
                    );
                    None
                }
            }
        } else {
            None
        };

        let session_endpoint = ContributionEndpoint::Session {
            session_id: Some(session.id),
            provider: None,
            id: None,
            turn_id: None,
            run_id: None,
        };
        let worktree_endpoint = ContributionEndpoint::Worktree {
            worktree_id: Some(session.worktree_id),
            id: None,
        };
        let session_metadata = serde_json::json!({
            "event": "session_created",
            "provider_id": &session.provider_id,
            "model_id": compose_model_id(&session.model_id, session.reasoning_effort.as_deref()),
            "reasoning_effort": session.reasoning_effort.as_deref(),
            "worktree_id": session.worktree_id.0.to_string(),
            "execution_environment": session.execution_environment.as_str(),
            "parent_session_id": session.parent_session_id.map(|id| id.0.to_string()),
            "relationship": session.relationship.as_deref(),
        });
        store
            .upsert_contribution(&Contribution {
                id: task_session_contribution_id(task.id, session.id),
                workspace_id: task.workspace_id,
                change_set_id: None,
                subject: ContributionEndpoint::Task {
                    task_id: Some(task.id),
                    id: None,
                },
                target: session_endpoint.clone(),
                role: ContributionRole::Authored,
                source: RecordSource::Session,
                origin: RecordOrigin::System,
                fidelity: RecordFidelity::Exact,
                trust: RecordTrust::High,
                summary: Some("Task created agent session".to_string()),
                fingerprint: None,
                issuer: Some("ctx-daemon".to_string()),
                metadata_json: Some(session_metadata),
                source_records: Vec::new(),
                created_at: Some(session.created_at),
                updated_at: Some(session.updated_at),
                schema_version: 1,
            })
            .await?;

        store
            .upsert_contribution(&Contribution {
                id: session_worktree_contribution_id(session.id, session.worktree_id),
                workspace_id: task.workspace_id,
                change_set_id: None,
                subject: session_endpoint.clone(),
                target: worktree_endpoint,
                role: ContributionRole::Context,
                source: RecordSource::Session,
                origin: RecordOrigin::System,
                fidelity: RecordFidelity::Exact,
                trust: RecordTrust::High,
                summary: Some("Agent session used worktree".to_string()),
                fingerprint: None,
                issuer: Some("ctx-daemon".to_string()),
                metadata_json: Some(serde_json::json!({
                    "event": "session_created",
                    "execution_environment": session.execution_environment.as_str(),
                })),
                source_records: Vec::new(),
                created_at: Some(session.created_at),
                updated_at: Some(session.updated_at),
                schema_version: 1,
            })
            .await?;

        if let Some(parent_session_id) = parent_session_id {
            store
                .upsert_contribution(&Contribution {
                    id: parent_child_session_contribution_id(parent_session_id, session.id),
                    workspace_id: task.workspace_id,
                    change_set_id: None,
                    subject: ContributionEndpoint::Session {
                        session_id: Some(parent_session_id),
                        provider: None,
                        id: None,
                        turn_id: None,
                        run_id: None,
                    },
                    target: session_endpoint,
                    role: ContributionRole::Context,
                    source: RecordSource::Session,
                    origin: RecordOrigin::System,
                    fidelity: RecordFidelity::Exact,
                    trust: RecordTrust::High,
                    summary: Some("Parent session created child agent session".to_string()),
                    fingerprint: None,
                    issuer: Some("ctx-daemon".to_string()),
                    metadata_json: Some(serde_json::json!({
                        "event": "session_created",
                        "relationship": session.relationship.as_deref(),
                    })),
                    source_records: Vec::new(),
                    created_at: Some(session.created_at),
                    updated_at: Some(session.updated_at),
                    schema_version: 1,
                })
                .await?;
        }

        Ok(())
    }

    pub async fn create_session_for_task(
        &self,
        task_id: TaskId,
        input: CreateTaskSessionInput,
    ) -> Result<Session, TaskSessionCreateError> {
        let handles = TaskSessionHandles::new(self);
        let creation_lock = handles.admission.task_session_creation_lock(task_id).await;
        let _creation_guard = creation_lock.lock().await;
        let context = self
            .load_task_context(task_id)
            .await
            .map_err(|error| match error {
                super::TaskLifecycleError::NotFound => TaskSessionCreateError::NotFound,
                super::TaskLifecycleError::Internal(error) => {
                    TaskSessionCreateError::Internal(error)
                }
            })?;
        let Some((store, task, workspace)) = context else {
            return Err(TaskSessionCreateError::NotFound);
        };
        create_session_for_loaded_task_inner(&handles, store, task, workspace, input).await
    }

    pub(in crate::daemon) async fn create_session_for_loaded_task_locked(
        &self,
        store: Store,
        task: Task,
        workspace: Workspace,
        input: CreateTaskSessionInput,
    ) -> Result<Session, TaskSessionCreateError> {
        let handles = TaskSessionHandles::new(self);
        create_session_for_loaded_task_inner(&handles, store, task, workspace, input).await
    }
}
