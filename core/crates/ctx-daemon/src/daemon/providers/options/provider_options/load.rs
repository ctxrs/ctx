use ctx_core::ids::WorkspaceId;
use ctx_core::models::Workspace;

use crate::daemon::ProviderWorkspaceLaunchRuntime;

use super::ProviderOptionsResponseError;

pub(super) struct ProviderOptionsWorkspaceInputs {
    pub(super) workspace: Workspace,
    pub(super) preferred_model_id: Option<String>,
}

pub(super) async fn load_provider_options_workspace_inputs(
    launch: &ProviderWorkspaceLaunchRuntime,
    workspace_id: WorkspaceId,
    provider_id: &str,
) -> Result<ProviderOptionsWorkspaceInputs, ProviderOptionsResponseError> {
    let workspace = load_workspace(launch, workspace_id).await?;
    let preferred_model_id =
        load_workspace_preferred_model_id(launch, workspace_id, provider_id).await?;
    Ok(ProviderOptionsWorkspaceInputs {
        workspace,
        preferred_model_id,
    })
}

async fn load_workspace(
    launch: &ProviderWorkspaceLaunchRuntime,
    ws_id: WorkspaceId,
) -> Result<Workspace, ProviderOptionsResponseError> {
    launch
        .load_workspace(ws_id)
        .await
        .map_err(|_| ProviderOptionsResponseError::WorkspaceLoad)?
        .ok_or(ProviderOptionsResponseError::WorkspaceNotFound)
}

async fn load_workspace_preferred_model_id(
    launch: &ProviderWorkspaceLaunchRuntime,
    workspace_id: WorkspaceId,
    provider_id: &str,
) -> Result<Option<String>, ProviderOptionsResponseError> {
    let store = launch
        .store_for_workspace(workspace_id)
        .await
        .map_err(ProviderOptionsResponseError::WorkspaceStoreLoad)?;
    ctx_workspace_config::load_preferred_new_session_model_id(&store, provider_id)
        .await
        .map_err(ProviderOptionsResponseError::WorkspacePreferenceLoad)
}
