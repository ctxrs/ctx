use std::path::Path as StdPath;

use anyhow::Result;
use ctx_core::ids::{TaskId, WorkspaceId};
use ctx_core::models::{ExecutionEnvironment, Task, Workspace};
use ctx_observability::logs;
use ctx_session_service::session_creation::should_preflight_default_session;
use ctx_store::Store;
use ctx_task_service::creation::TaskRecordCreateError;

use crate::daemon::task_route_handles::{TaskCreationHandle, TaskSessionAdmissionHandle};
use crate::daemon::workspaces::execution_environment_from_settings;

#[path = "create_task/default_session_flow.rs"]
mod default_session_flow;
#[path = "create_task/default_session_plan.rs"]
mod default_session_plan;
#[path = "create_task/idempotency.rs"]
mod idempotency;
#[path = "create_task/workspace.rs"]
mod workspace;

use default_session_flow::ensure_default_session_for_task;
use default_session_plan::{preflight_default_session_creation, DefaultSessionPlan};
use idempotency::{
    load_existing_task_for_request, persist_task_for_request, reload_or_retry_task_for_request,
    upsert_workspace_task_index,
};
use workspace::load_create_task_workspace;

use super::CreateTaskSessionInput;

type CreateTaskApiError = TaskCreateError;

struct CreateTaskWorkspaceContext {
    workspace: Workspace,
    store: Store,
}

#[derive(Debug, Clone)]
pub struct CreateTaskInput {
    pub task_id: Option<TaskId>,
    pub title: String,
    pub description: Option<String>,
    pub default_session: Option<CreateTaskSessionInput>,
}

#[derive(Debug)]
pub enum TaskCreateError {
    BadRequest(String),
    NotFound(String),
    Conflict(String),
    Internal(anyhow::Error),
    DefaultSessionFailed(super::TaskSessionCreateError),
    DefaultSessionConflict,
}

impl TaskCreateError {
    fn internal(error: impl Into<anyhow::Error>) -> Self {
        Self::Internal(error.into())
    }
}

impl From<TaskRecordCreateError> for TaskCreateError {
    fn from(error: TaskRecordCreateError) -> Self {
        match error {
            TaskRecordCreateError::NotFound(message) => Self::NotFound(message),
            TaskRecordCreateError::Conflict(message) => Self::Conflict(message),
            TaskRecordCreateError::Internal(error) => Self::Internal(error),
        }
    }
}

#[derive(Clone)]
struct TaskCreationHandles {
    creation: TaskCreationHandle,
    session_admission: TaskSessionAdmissionHandle,
}

impl TaskCreationHandles {
    fn new(creation: &TaskCreationHandle) -> Self {
        Self {
            creation: creation.clone(),
            session_admission: creation.session_admission().clone(),
        }
    }
}

impl TaskCreationHandle {
    async fn upsert_workspace_task_index(
        &self,
        task_id: TaskId,
        workspace_id: WorkspaceId,
    ) -> Result<()> {
        self.global_store()
            .upsert_workspace_task_index(task_id, workspace_id)
            .await
    }

    async fn load_workspace_context(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Option<CreateTaskWorkspaceContext>> {
        let Some(workspace) = self.global_store().get_workspace(workspace_id).await? else {
            return Ok(None);
        };
        let store = self.store_for_workspace(workspace_id).await?;
        Ok(Some(CreateTaskWorkspaceContext { workspace, store }))
    }

    pub async fn create_task_for_workspace(
        &self,
        workspace_id: WorkspaceId,
        input: CreateTaskInput,
    ) -> Result<Task, TaskCreateError> {
        let handles = TaskCreationHandles::new(self);
        let (workspace, store) = load_create_task_workspace(&handles, workspace_id).await?;
        let existing_task =
            load_existing_task_for_request(&handles, &store, workspace_id, &input).await?;
        let default_session_plan = if input.should_preflight_default_session(&existing_task) {
            Some(preflight_default_session_creation(&handles, &store, &workspace).await?)
        } else {
            None
        };
        let persisted_task =
            persist_task_for_request(&store, workspace_id, existing_task, &input).await?;
        upsert_workspace_task_index(&handles, persisted_task.task.id, workspace_id).await;

        let default_session_lock = handles
            .session_admission
            .task_session_creation_lock(persisted_task.task.id)
            .await;
        let _default_session_guard = default_session_lock.lock().await;
        let persisted_task =
            reload_or_retry_task_for_request(&store, workspace_id, persisted_task, &input).await?;
        ensure_default_session_for_task(
            &handles,
            store,
            workspace,
            persisted_task.task,
            input.default_session,
            default_session_plan,
            persisted_task.created_in_this_request,
        )
        .await
    }
}

impl CreateTaskInput {
    fn should_preflight_default_session(&self, existing_task: &Option<Task>) -> bool {
        should_preflight_default_session(existing_task.is_some(), self.default_session.is_some())
    }
}
