use crate::daemon::WorkspaceStoreAccessError;

mod management_config;
mod prompt_and_model;

pub(in crate::daemon::workspaces) use ctx_route_contracts::workspaces::{
    UpdateWorkspaceMergeQueueConfigRequest, WorkspaceMergeQueueConfigRouteResponse,
};
pub use ctx_route_contracts::workspaces::{
    UpdateWorkspacePrimaryBranchRequest, WorkspaceConfigUpdateResult,
    WorkspacePrimaryBranchSnapshot, WorkspaceRouteError,
};
pub(in crate::daemon::workspaces) use management_config::{
    merge_queue_config_route_response, merge_queue_config_update,
    workspace_execution_config_route_snapshot, worktree_bootstrap_config_route_response,
    worktree_bootstrap_config_update,
};
pub(in crate::daemon::workspaces) use prompt_and_model::{
    agent_system_prompt_config_route_response, provider_model_preference_error,
    provider_model_preference_route_response, subagent_system_prompt_config_route_response,
    workspace_store_error,
};

pub(in crate::daemon::workspaces) fn request_or_policy_route_error(
    error: anyhow::Error,
) -> WorkspaceRouteError {
    if ctx_settings_service::is_execution_policy_denial(&error) {
        WorkspaceRouteError::forbidden(error)
    } else {
        WorkspaceRouteError::bad_request(error)
    }
}

pub(in crate::daemon::workspaces) fn workspace_store_route_error(
    error: WorkspaceStoreAccessError,
) -> WorkspaceRouteError {
    match error {
        WorkspaceStoreAccessError::NotFound => {
            WorkspaceRouteError::not_found("workspace not found")
        }
        WorkspaceStoreAccessError::Unavailable(error) => WorkspaceRouteError::internal(error),
    }
}
