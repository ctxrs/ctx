use ctx_core::ids::WorkspaceId;
use ctx_workspace_config as workspace_config;

use super::route_config::{
    merge_queue_config_route_response, merge_queue_config_update, request_or_policy_route_error,
    workspace_store_route_error, UpdateWorkspaceMergeQueueConfigRequest,
    WorkspaceConfigUpdateResult, WorkspaceMergeQueueConfigRouteResponse, WorkspaceRouteError,
};
use crate::daemon::WorkspaceMergeQueueConfigHandle;
use ctx_route_contracts::workspaces::WorkspaceRouteParams;

impl WorkspaceMergeQueueConfigHandle {
    pub async fn workspace_merge_queue_config_for_route_params(
        &self,
        params: WorkspaceRouteParams,
    ) -> Result<WorkspaceMergeQueueConfigRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.workspace_merge_queue_config_for_request(workspace_id)
            .await
    }

    pub async fn workspace_merge_queue_config_for_request(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<WorkspaceMergeQueueConfigRouteResponse, WorkspaceRouteError> {
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        let cfg = workspace_config::load_merge_queue_config(&store)
            .await
            .map_err(WorkspaceRouteError::internal)?;
        Ok(merge_queue_config_route_response(cfg))
    }

    pub async fn update_workspace_merge_queue_config_for_route_params(
        &self,
        params: WorkspaceRouteParams,
        request: UpdateWorkspaceMergeQueueConfigRequest,
    ) -> Result<WorkspaceConfigUpdateResult, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.update_workspace_merge_queue_config_for_request(workspace_id, request)
            .await
    }

    pub async fn update_workspace_merge_queue_config_for_request(
        &self,
        workspace_id: WorkspaceId,
        req: UpdateWorkspaceMergeQueueConfigRequest,
    ) -> Result<WorkspaceConfigUpdateResult, WorkspaceRouteError> {
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        let transition = workspace_config::update_merge_queue_config_with_transition(
            &store,
            merge_queue_config_update(req),
        )
        .await
        .map_err(request_or_policy_route_error)?;
        if !transition.was_enabled && transition.now_enabled {
            self.schedule_store_if_enabled_and_queued(&store, workspace_id)
                .await
                .map_err(request_or_policy_route_error)?;
        } else if transition.was_enabled && !transition.now_enabled {
            self.cancel_store_queued_entries_for_disabled_workspace(&store, workspace_id)
                .await
                .map_err(request_or_policy_route_error)?;
        }
        Ok(WorkspaceConfigUpdateResult { ok: true })
    }
}
