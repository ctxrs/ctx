use ctx_core::ids::WorkspaceId;
use ctx_core::models::Workspace;

use crate::daemon::providers::auth_check::ProviderAuthCheckError;
use crate::daemon::ProviderWorkspaceLaunchRuntime;

pub(super) async fn load_workspace(
    launch: &ProviderWorkspaceLaunchRuntime,
    workspace_id: WorkspaceId,
) -> Result<Workspace, ProviderAuthCheckError> {
    launch
        .load_workspace(workspace_id)
        .await
        .map_err(|_| ProviderAuthCheckError::WorkspaceLoad)?
        .ok_or(ProviderAuthCheckError::WorkspaceNotFound)
}
