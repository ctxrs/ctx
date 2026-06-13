use ctx_core::ids::WorkspaceId;
use serde::{Deserialize, Serialize};

use crate::ResourceUtilizationSnapshot;

#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
pub struct ResourceUtilizationRouteQuery {
    workspace_id: String,
}

impl ResourceUtilizationRouteQuery {
    pub fn parse_workspace_id(&self) -> Result<WorkspaceId, ResourceUtilizationRouteError> {
        uuid::Uuid::parse_str(&self.workspace_id)
            .map(WorkspaceId)
            .map_err(|_| ResourceUtilizationRouteError::bad_request("invalid workspace id"))
    }

    #[cfg(test)]
    fn from_workspace_id_raw(workspace_id: impl Into<String>) -> Self {
        Self {
            workspace_id: workspace_id.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct ResourceUtilizationRouteResponse(ResourceUtilizationSnapshot);

impl ResourceUtilizationRouteResponse {
    pub fn new(snapshot: ResourceUtilizationSnapshot) -> Self {
        Self(snapshot)
    }

    pub fn snapshot(&self) -> &ResourceUtilizationSnapshot {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ResourceUtilizationRouteErrorKind {
    BadRequest,
    NotFound,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ResourceUtilizationRouteError {
    kind: ResourceUtilizationRouteErrorKind,
    message: String,
}

impl ResourceUtilizationRouteError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            kind: ResourceUtilizationRouteErrorKind::BadRequest,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: ResourceUtilizationRouteErrorKind::NotFound,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: ResourceUtilizationRouteErrorKind::Internal,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> ResourceUtilizationRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_query_rejects_invalid_workspace_id() {
        let query = ResourceUtilizationRouteQuery::from_workspace_id_raw("not-a-uuid");

        let error = query.parse_workspace_id().unwrap_err();
        assert_eq!(error.kind(), ResourceUtilizationRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid workspace id");
    }
}
