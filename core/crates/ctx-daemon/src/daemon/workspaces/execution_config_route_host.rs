use ctx_core::ids::WorkspaceId;
use ctx_route_contracts::workspaces::{
    UpdateWorkspaceExecutionConfigRequest, WorkspaceExecutionConfigRouteSnapshot,
};
use ctx_workspace_config as workspace_config;

use super::route_config::{
    request_or_policy_route_error, workspace_execution_config_route_snapshot,
    workspace_store_route_error, WorkspaceConfigUpdateResult, WorkspaceRouteError,
};
use crate::daemon::WorkspaceExecutionConfigHandle;

impl WorkspaceExecutionConfigHandle {
    pub(in crate::daemon) async fn workspace_execution_config_snapshot(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<WorkspaceExecutionConfigRouteSnapshot, WorkspaceRouteError> {
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        let settings = ctx_settings_service::load_settings(self.global_store())
            .await
            .map_err(WorkspaceRouteError::internal)?;
        match ctx_settings_service::workspace_execution_config_snapshot_for_loaded_settings(
            &settings, &store,
        )
        .await
        {
            Ok(snapshot) => Ok(workspace_execution_config_route_snapshot(snapshot)),
            Err(
                ctx_settings_service::WorkspaceExecutionConfigSnapshotError::InvalidWorkspaceConfig(
                    error,
                ),
            ) => Err(WorkspaceRouteError::bad_request(error)),
            Err(ctx_settings_service::WorkspaceExecutionConfigSnapshotError::RequestOrPolicy(
                error,
            )) => Err(request_or_policy_route_error(error)),
            Err(ctx_settings_service::WorkspaceExecutionConfigSnapshotError::Internal(error)) => {
                Err(WorkspaceRouteError::internal(error))
            }
        }
    }

    pub(in crate::daemon) async fn update_workspace_execution_config(
        &self,
        workspace_id: WorkspaceId,
        req: UpdateWorkspaceExecutionConfigRequest,
    ) -> Result<WorkspaceConfigUpdateResult, WorkspaceRouteError> {
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        let update = workspace_config::parse_execution_config_update_input(
            req.environment.trim(),
            req.network_mode.as_deref(),
            req.allowlist,
            self.sandbox_runtime_available_for_execution_config(),
        )
        .map_err(WorkspaceRouteError::bad_request)?;
        let settings = ctx_settings_service::load_settings(self.global_store())
            .await
            .map_err(WorkspaceRouteError::internal)?;
        ctx_settings_service::update_workspace_execution_config_for_loaded_settings(
            &settings, &store, update,
        )
        .await
        .map_err(|error| match error {
            ctx_settings_service::WorkspaceExecutionConfigUpdateError::RequestOrPolicy(error) => {
                request_or_policy_route_error(error)
            }
            ctx_settings_service::WorkspaceExecutionConfigUpdateError::Persistence(error) => {
                WorkspaceRouteError::bad_request(error)
            }
        })?;
        Ok(WorkspaceConfigUpdateResult { ok: true })
    }

    pub(in crate::daemon) async fn require_workspace_execution_config_update_target(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<(), WorkspaceRouteError> {
        self.existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        Ok(())
    }

    pub(in crate::daemon) fn sandbox_runtime_available_for_execution_config(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            ctx_harness_runtime::local_runtime_available(
                self.data_root(),
                &ctx_settings_model::ContainerRuntimeKind::SharedVmContainer,
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = self.data_root();
            true
        }
    }
}
