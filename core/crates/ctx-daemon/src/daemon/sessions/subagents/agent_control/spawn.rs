use std::path::{Path, PathBuf};
use std::sync::Arc;

use ctx_core::ids::{SessionId, TaskId, TurnId, WorkspaceId, WorktreeId};
use ctx_core::models::{Session, SubagentInvocationChild, Workspace};
use ctx_observability::perf_telemetry::PerfTelemetry;
use ctx_session_runtime::runtime::SessionRuntime;
use ctx_store::Store;
use tokio::sync::Mutex;

use super::super::errors::ApiResult;
use super::super::{
    api_error, build_spawned_agent_detail, init_subagents, run_subagent_child, AgentInitItem,
    AgentInitReq, SessionSubagentMcpControlPublicationHost,
    SessionSubagentMcpControlSchedulerSpawner, SpawnAgentReq, SpawnAgentResp, SubagentChildRunHost,
    SubagentErrorKind,
};
use crate::daemon::workspaces::TaskWorktreeHost;
use crate::daemon::{
    scheduler::SchedulerCommand, session_store_access_anyhow, ProviderWorkspaceLaunchRuntime,
    SessionStoreAccessError, SessionStoreLookup, SessionVcsHandle,
};

mod prompt;
mod providers;
mod worktrees;

#[derive(Clone)]
pub(in crate::daemon) struct SubagentSpawnHost {
    session_stores: SessionStoreLookup,
    session_runtime: Arc<SessionRuntime<SchedulerCommand>>,
    scheduler_spawner: SessionSubagentMcpControlSchedulerSpawner,
    publish_host: SessionSubagentMcpControlPublicationHost,
    child_run_host: SubagentChildRunHost,
    session_vcs: SessionVcsHandle,
    worktrees: Arc<TaskWorktreeHost>,
    provider_launch: Arc<ProviderWorkspaceLaunchRuntime>,
    global_store: Store,
    perf_telemetry: PerfTelemetry,
    data_root: PathBuf,
}

pub(in crate::daemon) struct SubagentSpawnHostParts {
    pub(in crate::daemon) session_stores: SessionStoreLookup,
    pub(in crate::daemon) session_runtime: Arc<SessionRuntime<SchedulerCommand>>,
    pub(in crate::daemon) scheduler_spawner: SessionSubagentMcpControlSchedulerSpawner,
    pub(in crate::daemon) publish_host: SessionSubagentMcpControlPublicationHost,
    pub(in crate::daemon) child_run_host: SubagentChildRunHost,
    pub(in crate::daemon) session_vcs: SessionVcsHandle,
    pub(in crate::daemon) worktrees: Arc<TaskWorktreeHost>,
    pub(in crate::daemon) provider_launch: Arc<ProviderWorkspaceLaunchRuntime>,
    pub(in crate::daemon) global_store: Store,
    pub(in crate::daemon) perf_telemetry: PerfTelemetry,
    pub(in crate::daemon) data_root: PathBuf,
}

impl SubagentSpawnHost {
    pub(in crate::daemon) fn new(parts: SubagentSpawnHostParts) -> Self {
        Self {
            session_stores: parts.session_stores,
            session_runtime: parts.session_runtime,
            scheduler_spawner: parts.scheduler_spawner,
            publish_host: parts.publish_host,
            child_run_host: parts.child_run_host,
            session_vcs: parts.session_vcs,
            worktrees: parts.worktrees,
            provider_launch: parts.provider_launch,
            global_store: parts.global_store,
            perf_telemetry: parts.perf_telemetry,
            data_root: parts.data_root,
        }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) async fn max_subagents_per_call(&self) -> ApiResult<usize> {
        let settings = ctx_settings_service::load_settings(&self.global_store)
            .await
            .map_err(super::super::internal_api_error)?;
        Ok(ctx_subagent_service::resolve_max_subagents_per_call(
            settings.subagents.as_ref().and_then(|s| s.max_per_call),
        ))
    }

    pub(in crate::daemon) async fn load_parent_session(
        &self,
        parent_id: SessionId,
    ) -> ApiResult<(Store, Session)> {
        let store = match self.session_stores.existing_session_store(parent_id).await {
            Ok(store) => store,
            Err(SessionStoreAccessError::NotFound) => {
                return Err(super::super::not_found("parent session not found"));
            }
            Err(error) => {
                return Err(super::super::internal_api_error(
                    session_store_access_anyhow(error),
                ));
            }
        };
        let parent = store
            .get_session(parent_id)
            .await
            .map_err(super::super::internal_api_error)?
            .ok_or_else(|| super::super::not_found("parent session not found"))?;
        Ok((store, parent))
    }

    pub(in crate::daemon) async fn task_session_creation_lock(
        &self,
        task_id: TaskId,
    ) -> Arc<Mutex<()>> {
        self.session_runtime
            .task_session_creation_lock(task_id)
            .await
    }

    pub(in crate::daemon) async fn store_for_session(
        &self,
        session_id: SessionId,
    ) -> ApiResult<Store> {
        self.session_stores
            .existing_session_store(session_id)
            .await
            .map_err(|error| match error {
                SessionStoreAccessError::NotFound => super::super::not_found("session not found"),
                error => super::super::internal_api_error(session_store_access_anyhow(error)),
            })
    }

    pub(in crate::daemon) async fn load_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> ApiResult<Workspace> {
        self.global_store
            .get_workspace(workspace_id)
            .await
            .map_err(super::super::internal_api_error)?
            .ok_or_else(|| api_error(SubagentErrorKind::NotFound, "workspace not found"))
    }

    pub(in crate::daemon) async fn upsert_workspace_session_index(
        &self,
        session_id: SessionId,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<()> {
        self.global_store
            .upsert_workspace_session_index(session_id, workspace_id)
            .await
    }

    pub(in crate::daemon) fn spawn_subagent_completion_task(
        &self,
        child: SubagentInvocationChild,
        invocation_id: String,
        tool_call_id: String,
        parent_id: SessionId,
        parent_turn_id: Option<TurnId>,
        parent_worktree_id: WorktreeId,
    ) {
        let child_run_host = self.child_run_host.clone();
        tokio::spawn(async move {
            if let Err(error) = run_subagent_child(&child_run_host, child, parent_worktree_id).await
            {
                tracing::warn!(error = %error, "subagent execution failed");
            }
            if let Err(error) = super::super::finalize_subagent_invocation(
                &child_run_host,
                &invocation_id,
                &tool_call_id,
                parent_id,
                parent_turn_id,
            )
            .await
            {
                tracing::warn!(error = %error, "failed to finalize subagent invocation");
            }
        });
    }

    pub(in crate::daemon) async fn spawn_agent(
        self: &Arc<Self>,
        parent_id: SessionId,
        req: SpawnAgentReq,
    ) -> ApiResult<SpawnAgentResp> {
        spawn_agent_with_host(Arc::clone(self), parent_id, req).await
    }
}

async fn spawn_agent_with_host(
    host: Arc<SubagentSpawnHost>,
    parent_id: SessionId,
    req: SpawnAgentReq,
) -> ApiResult<SpawnAgentResp> {
    let task_label = req.task_label.trim().to_string();
    if task_label.is_empty() {
        return Err(api_error(
            SubagentErrorKind::BadRequest,
            "task_label is required",
        ));
    }
    let prompt = req.prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(api_error(
            SubagentErrorKind::BadRequest,
            "prompt is required",
        ));
    }

    let spawned_children = init_subagents(
        host,
        parent_id,
        AgentInitReq {
            tool_call_id: req.tool_call_id,
            response_mode: None,
            worktree: req.worktree,
            agents: vec![AgentInitItem {
                prompt,
                label: Some(task_label.clone()),
                harness: req.harness,
                model: req.model,
                reasoning_effort: req.reasoning_effort,
            }],
        },
    )
    .await?;
    let spawned = spawned_children
        .into_iter()
        .next()
        .ok_or_else(|| api_error(SubagentErrorKind::NotFound, "spawned agent not found"))?;
    Ok(SpawnAgentResp {
        agent: build_spawned_agent_detail(&spawned),
    })
}
