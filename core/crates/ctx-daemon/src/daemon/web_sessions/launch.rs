use std::path::PathBuf;
use std::sync::Arc;

mod context;

use context::resolve_web_session_launch_context;
use ctx_core::ids::{SessionId, WorktreeId};
use ctx_core::models::{ExecutionEnvironment, Worktree};
use ctx_settings_model::{ExecutionMode, ExecutionSettings};
use ctx_store::Store;
use ctx_transport_runtime::web_sessions::{
    validate_web_session_host_worktree, validate_web_session_url, WebSessionCreateRequest,
    WebSessionInfo, WebSessionLaunchPolicyError, WebSessionLaunchPolicyErrorKind,
    WebSessionManager, WebSessionViewport,
};

use crate::daemon::web_sessions::{prepare_web_session_worker, WebSessionWorkerRuntimeHost};
use crate::daemon::ProtectedWorkspaceStoreLookup;

pub struct WebSessionLaunchRequest {
    pub session_id: Option<SessionId>,
    pub worktree_id: Option<WorktreeId>,
    pub url: String,
    pub viewport: Option<WebSessionViewport>,
    pub fps: Option<u32>,
}

#[derive(Clone)]
pub(in crate::daemon) struct WebSessionLaunchHost {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    data_root: PathBuf,
    worker_runtime: WebSessionWorkerRuntimeHost,
    web_sessions: Arc<WebSessionManager>,
}

impl WebSessionLaunchHost {
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        data_root: PathBuf,
        worker_runtime: WebSessionWorkerRuntimeHost,
        web_sessions: Arc<WebSessionManager>,
    ) -> Self {
        Self {
            global_store,
            workspace_stores,
            data_root,
            worker_runtime,
            web_sessions,
        }
    }

    async fn effective_execution_settings(
        &self,
        workspace_id: ctx_core::ids::WorkspaceId,
    ) -> anyhow::Result<ExecutionSettings> {
        let store = self
            .workspace_stores
            .store_for_workspace(workspace_id)
            .await?;
        ctx_settings_service::effective_execution_settings(&self.global_store, &store).await
    }

    async fn load_session_launch_target(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<(ExecutionEnvironment, Worktree)> {
        let session_workspace_id = self
            .global_store
            .get_workspace_id_for_session(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("workspace missing for session {}", session_id.0))?;
        let store = self
            .workspace_stores
            .store_for_workspace(session_workspace_id)
            .await?;
        let session = store
            .get_session(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session not found"))?;
        let worktree = store
            .get_worktree(session.worktree_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("worktree not found"))?;
        Ok((session.execution_environment, worktree))
    }

    async fn load_worktree_launch_target(
        &self,
        worktree_id: WorktreeId,
    ) -> anyhow::Result<Worktree> {
        let store = self
            .workspace_stores
            .store_for_worktree(worktree_id)
            .await?;
        store
            .get_worktree(worktree_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("worktree not found"))
    }

    async fn validate_worktree_host_launch(&self, worktree: &Worktree) -> anyhow::Result<()> {
        let store = self
            .workspace_stores
            .store_for_workspace(worktree.workspace_id)
            .await?;
        let has_sandbox_binding = store.get_sandbox_binding(worktree.id).await?.is_some();
        let effective = self
            .effective_execution_settings(worktree.workspace_id)
            .await?;
        validate_web_session_host_worktree(
            has_sandbox_binding,
            matches!(effective.mode, ExecutionMode::Sandbox),
        )?;
        Ok(())
    }

    async fn prepare_worker(
        &self,
    ) -> anyhow::Result<crate::daemon::web_sessions::PreparedWebSessionWorker> {
        debug_assert_eq!(self.data_root.as_path(), self.worker_runtime.data_root());
        prepare_web_session_worker(&self.worker_runtime).await
    }

    async fn create_web_session(
        &self,
        request: WebSessionCreateRequest,
    ) -> Result<WebSessionInfo, WebSessionLaunchError> {
        let handle = self
            .web_sessions
            .create(request)
            .await
            .map_err(|e| internal_error(format!("failed to create web session: {e}")))?;
        Ok(handle.snapshot().await)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebSessionLaunchErrorKind {
    BadRequest,
    Forbidden,
    Internal,
}

#[derive(Debug, Eq, PartialEq)]
pub struct WebSessionLaunchError {
    kind: WebSessionLaunchErrorKind,
    message: String,
}

impl WebSessionLaunchError {
    pub(in crate::daemon) fn bad_request(error: impl Into<String>) -> Self {
        launch_error(WebSessionLaunchErrorKind::BadRequest, error)
    }

    #[cfg(test)]
    pub(in crate::daemon) fn forbidden(error: impl Into<String>) -> Self {
        launch_error(WebSessionLaunchErrorKind::Forbidden, error)
    }

    pub(in crate::daemon) fn internal(error: impl Into<String>) -> Self {
        launch_error(WebSessionLaunchErrorKind::Internal, error)
    }

    pub fn kind(&self) -> WebSessionLaunchErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

pub(in crate::daemon) async fn create_web_session(
    host: &WebSessionLaunchHost,
    request: WebSessionLaunchRequest,
) -> Result<WebSessionInfo, WebSessionLaunchError> {
    validate_web_session_url(&request.url).map_err(|e| bad_request(e.to_string()))?;

    let launch_context =
        resolve_web_session_launch_context(host, request.session_id, request.worktree_id)
            .await
            .map_err(request_or_policy_error)?;

    let worker = host
        .prepare_worker()
        .await
        .map_err(|error| internal_error(format!("{error:#}")))?;

    host.create_web_session(WebSessionCreateRequest {
        url: request.url,
        viewport: request.viewport,
        fps: request.fps,
        work_dir: launch_context.work_dir,
        session_id: request.session_id.map(|id| id.0.to_string()),
        worktree_id: request.worktree_id.map(|id| id.0.to_string()),
        node_bin: worker.node_runtime.node_bin,
        worker_path: worker.bundle.worker_path,
        node_modules_path: worker.bundle.node_modules_path,
    })
    .await
}

fn launch_error(
    kind: WebSessionLaunchErrorKind,
    message: impl Into<String>,
) -> WebSessionLaunchError {
    WebSessionLaunchError {
        kind,
        message: message.into(),
    }
}

fn bad_request(error: impl Into<String>) -> WebSessionLaunchError {
    WebSessionLaunchError::bad_request(error)
}

fn request_or_policy_error(error: anyhow::Error) -> WebSessionLaunchError {
    let kind = if let Some(policy_error) = error.downcast_ref::<WebSessionLaunchPolicyError>() {
        match policy_error.kind() {
            WebSessionLaunchPolicyErrorKind::BadRequest => WebSessionLaunchErrorKind::BadRequest,
            WebSessionLaunchPolicyErrorKind::Forbidden => WebSessionLaunchErrorKind::Forbidden,
        }
    } else if ctx_settings_service::is_execution_policy_denial(&error) {
        WebSessionLaunchErrorKind::Forbidden
    } else {
        WebSessionLaunchErrorKind::BadRequest
    };
    launch_error(kind, format!("{error:#}"))
}

fn internal_error(error: impl Into<String>) -> WebSessionLaunchError {
    WebSessionLaunchError::internal(error)
}

#[cfg(test)]
#[path = "launch/tests.rs"]
mod tests;
