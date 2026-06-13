use ctx_core::ids::{TaskId, WorkspaceId};

#[derive(Debug)]
pub struct TaskRouteParams {
    task_id: String,
}

impl TaskRouteParams {
    pub fn new(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
        }
    }

    pub fn parse_task_id(&self) -> Result<TaskId, TaskRouteError> {
        parse_task_id(&self.task_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskRouteErrorKind {
    BadRequest,
    NotFound,
    Conflict,
    Forbidden,
    InsufficientStorage,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRouteError {
    kind: TaskRouteErrorKind,
    message: String,
}

impl TaskRouteError {
    fn new(kind: TaskRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(TaskRouteErrorKind::BadRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(TaskRouteErrorKind::NotFound, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(TaskRouteErrorKind::Conflict, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(TaskRouteErrorKind::Forbidden, message)
    }

    pub fn insufficient_storage(message: impl Into<String>) -> Self {
        Self::new(TaskRouteErrorKind::InsufficientStorage, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(TaskRouteErrorKind::Internal, message)
    }

    pub fn kind(&self) -> TaskRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

pub(crate) fn parse_workspace_id(value: &str) -> Result<WorkspaceId, TaskRouteError> {
    uuid::Uuid::parse_str(value)
        .map(WorkspaceId)
        .map_err(|_| TaskRouteError::bad_request("invalid workspace id"))
}

pub(crate) fn parse_task_id(value: &str) -> Result<TaskId, TaskRouteError> {
    uuid::Uuid::parse_str(value)
        .map(TaskId)
        .map_err(|_| TaskRouteError::bad_request("invalid task id"))
}
