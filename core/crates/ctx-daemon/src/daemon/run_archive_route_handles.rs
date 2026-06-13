use ctx_core::ids::WorkspaceId;
use ctx_store::Store;

use super::state::{ProtectedWorkspaceStoreLookup, WorkspaceStoreAccessError};

#[derive(Clone)]
pub struct RunArchiveHandle {
    workspace_stores: ProtectedWorkspaceStoreLookup,
}

impl RunArchiveHandle {
    pub(in crate::daemon) fn new(workspace_stores: ProtectedWorkspaceStoreLookup) -> Self {
        Self { workspace_stores }
    }

    pub(in crate::daemon) async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, WorkspaceStoreAccessError> {
        self.workspace_stores
            .existing_workspace_store(workspace_id)
            .await
    }
}
