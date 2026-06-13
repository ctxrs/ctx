use ctx_core::ids::{WorkspaceId, WorktreeId};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WorkspaceRouteParams {
    workspace_id: String,
}

impl WorkspaceRouteParams {
    pub fn new(workspace_id: impl Into<String>) -> Self {
        Self {
            workspace_id: workspace_id.into(),
        }
    }

    pub fn parse_workspace_id(&self) -> Result<WorkspaceId, WorkspaceRouteError> {
        uuid::Uuid::parse_str(&self.workspace_id)
            .map(WorkspaceId)
            .map_err(|_| WorkspaceRouteError::bad_request("invalid workspace id"))
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WorktreeRouteParams {
    worktree_id: String,
}

impl WorktreeRouteParams {
    pub fn new(worktree_id: impl Into<String>) -> Self {
        Self {
            worktree_id: worktree_id.into(),
        }
    }

    pub fn parse_worktree_id(&self) -> Result<WorktreeId, WorkspaceRouteError> {
        uuid::Uuid::parse_str(&self.worktree_id)
            .map(WorktreeId)
            .map_err(|_| WorkspaceRouteError::bad_request("invalid worktree id"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceRouteErrorKind {
    NotFound,
    BadRequest,
    Forbidden,
    InsufficientStorage,
    Internal,
}

#[derive(Debug, Clone)]
pub struct WorkspaceRouteError {
    kind: WorkspaceRouteErrorKind,
    message: String,
}

impl WorkspaceRouteError {
    pub fn new(kind: WorkspaceRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(WorkspaceRouteErrorKind::NotFound, message)
    }

    pub fn bad_request(error: impl std::fmt::Display) -> Self {
        Self::new(WorkspaceRouteErrorKind::BadRequest, error.to_string())
    }

    pub fn forbidden(error: impl std::fmt::Display) -> Self {
        Self::new(WorkspaceRouteErrorKind::Forbidden, error.to_string())
    }

    pub fn insufficient_storage(error: impl std::fmt::Display) -> Self {
        Self::new(
            WorkspaceRouteErrorKind::InsufficientStorage,
            error.to_string(),
        )
    }

    pub fn internal(error: impl std::fmt::Display) -> Self {
        Self::new(WorkspaceRouteErrorKind::Internal, error.to_string())
    }

    pub fn kind(&self) -> WorkspaceRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}
