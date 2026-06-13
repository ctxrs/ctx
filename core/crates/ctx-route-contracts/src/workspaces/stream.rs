use ctx_core::ids::WorkspaceId;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WorkspaceStreamRouteParams {
    workspace_id: String,
}

impl WorkspaceStreamRouteParams {
    pub fn new(workspace_id: impl Into<String>) -> Self {
        Self {
            workspace_id: workspace_id.into(),
        }
    }

    pub fn parse_workspace_id(&self) -> Result<WorkspaceId, WorkspaceStreamRouteError> {
        uuid::Uuid::parse_str(&self.workspace_id)
            .map(WorkspaceId)
            .map_err(|_| WorkspaceStreamRouteError::bad_request("invalid workspace id"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceStreamRouteErrorKind {
    BadRequest,
    NotFound,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceStreamRouteError {
    kind: WorkspaceStreamRouteErrorKind,
    message: String,
}

impl WorkspaceStreamRouteError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            kind: WorkspaceStreamRouteErrorKind::BadRequest,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: WorkspaceStreamRouteErrorKind::NotFound,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: WorkspaceStreamRouteErrorKind::Internal,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> WorkspaceStreamRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}
