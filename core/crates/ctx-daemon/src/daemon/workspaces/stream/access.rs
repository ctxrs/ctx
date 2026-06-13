use ctx_core::ids::WorkspaceId;
use ctx_route_contracts::workspaces::{WorkspaceStreamRouteError, WorkspaceStreamRouteParams};

use crate::daemon::WorkspaceStreamHandle;

#[derive(Debug)]
pub enum WorkspaceStreamAccessError {
    NotFound,
    Internal(anyhow::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceStreamRouteAdmission {
    workspace_id: WorkspaceId,
}

impl WorkspaceStreamRouteAdmission {
    pub(super) fn new(workspace_id: WorkspaceId) -> Self {
        Self { workspace_id }
    }

    pub fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }
}

pub(super) fn workspace_stream_route_error_from_access(
    error: WorkspaceStreamAccessError,
) -> WorkspaceStreamRouteError {
    match error {
        WorkspaceStreamAccessError::NotFound => {
            WorkspaceStreamRouteError::not_found("workspace not found")
        }
        WorkspaceStreamAccessError::Internal(error) => {
            WorkspaceStreamRouteError::internal(error.to_string())
        }
    }
}

pub(super) async fn require_existing_workspace_for_stream(
    handle: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
) -> Result<(), WorkspaceStreamAccessError> {
    let exists = handle
        .global_store()
        .get_workspace(workspace_id)
        .await
        .map_err(WorkspaceStreamAccessError::Internal)?
        .is_some();
    if !exists {
        return Err(WorkspaceStreamAccessError::NotFound);
    }
    Ok(())
}

impl WorkspaceStreamHandle {
    pub async fn workspace_exists(&self, workspace_id: WorkspaceId) -> anyhow::Result<bool> {
        self.global_store()
            .get_workspace(workspace_id)
            .await
            .map(|workspace| workspace.is_some())
    }

    pub async fn require_workspace_active_stream_access(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<(), WorkspaceStreamAccessError> {
        require_existing_workspace_for_stream(self, workspace_id).await
    }

    pub async fn admit_workspace_active_stream_for_route(
        &self,
        params: WorkspaceStreamRouteParams,
    ) -> Result<WorkspaceStreamRouteAdmission, WorkspaceStreamRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.require_workspace_active_stream_access(workspace_id)
            .await
            .map_err(workspace_stream_route_error_from_access)?;
        Ok(WorkspaceStreamRouteAdmission::new(workspace_id))
    }
}
