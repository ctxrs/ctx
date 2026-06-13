use ctx_route_contracts::workspaces::{
    UpdateWorkspaceExecutionConfigRequest, WorkspaceExecutionConfigRouteSnapshot,
    WorkspaceRouteParams,
};

use super::route_config::{WorkspaceConfigUpdateResult, WorkspaceRouteError};
use crate::daemon::WorkspaceExecutionConfigHandle;

impl WorkspaceExecutionConfigHandle {
    pub async fn workspace_execution_config_for_route_params(
        &self,
        params: WorkspaceRouteParams,
    ) -> Result<WorkspaceExecutionConfigRouteSnapshot, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.workspace_execution_config_snapshot(workspace_id).await
    }

    pub async fn update_workspace_execution_config_for_route_params(
        &self,
        params: WorkspaceRouteParams,
        request: UpdateWorkspaceExecutionConfigRequest,
    ) -> Result<WorkspaceConfigUpdateResult, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.update_workspace_execution_config(workspace_id, request)
            .await
    }

    pub async fn workspace_execution_config_update_target_for_route_params(
        &self,
        params: &WorkspaceRouteParams,
    ) -> Result<(), WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.require_workspace_execution_config_update_target(workspace_id)
            .await
    }
}

#[cfg(test)]
mod tests {
    use ctx_core::ids::WorkspaceId;
    use ctx_core::models::{VcsKind, Workspace};
    use ctx_route_contracts::workspaces::{
        UpdateWorkspaceExecutionConfigRequest, WorkspaceRouteErrorKind, WorkspaceRouteParams,
    };

    use crate::test_support::TestDaemon;

    async fn test_daemon() -> (tempfile::TempDir, TestDaemon) {
        let temp = tempfile::tempdir().expect("tempdir");
        let daemon =
            TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
                .await
                .expect("test daemon");
        (temp, daemon)
    }

    async fn create_workspace(daemon: &TestDaemon, name: &str) -> Workspace {
        daemon
            .global_store()
            .create_workspace(
                name.to_string(),
                daemon.data_root().join(name).to_string_lossy().to_string(),
                VcsKind::Git,
            )
            .await
            .expect("create workspace")
    }

    #[tokio::test]
    async fn execution_config_route_rejects_invalid_workspace_id() {
        let (_temp, daemon) = test_daemon().await;
        let handle = daemon.workspace_execution_config_handle_for_test();

        let get_error = handle
            .workspace_execution_config_for_route_params(WorkspaceRouteParams::new(
                "not-a-workspace",
            ))
            .await
            .unwrap_err();
        assert_eq!(get_error.kind(), WorkspaceRouteErrorKind::BadRequest);
        assert_eq!(get_error.message(), "invalid workspace id");

        let post_error = handle
            .update_workspace_execution_config_for_route_params(
                WorkspaceRouteParams::new("not-a-workspace"),
                UpdateWorkspaceExecutionConfigRequest {
                    environment: "host".to_string(),
                    network_mode: None,
                    allowlist: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(post_error.kind(), WorkspaceRouteErrorKind::BadRequest);
        assert_eq!(post_error.message(), "invalid workspace id");
    }

    #[tokio::test]
    async fn execution_config_route_maps_missing_workspace_to_not_found() {
        let (_temp, daemon) = test_daemon().await;
        let missing_workspace_id = WorkspaceId::new();
        let handle = daemon.workspace_execution_config_handle_for_test();

        let get_error = handle
            .workspace_execution_config_for_route_params(WorkspaceRouteParams::new(
                missing_workspace_id.0.to_string(),
            ))
            .await
            .unwrap_err();
        assert_eq!(get_error.kind(), WorkspaceRouteErrorKind::NotFound);
        assert_eq!(get_error.message(), "workspace not found");

        let post_error = handle
            .update_workspace_execution_config_for_route_params(
                WorkspaceRouteParams::new(missing_workspace_id.0.to_string()),
                UpdateWorkspaceExecutionConfigRequest {
                    environment: "container".to_string(),
                    network_mode: Some("invalid-network".to_string()),
                    allowlist: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(post_error.kind(), WorkspaceRouteErrorKind::NotFound);
        assert_eq!(post_error.message(), "workspace not found");
    }

    #[tokio::test]
    async fn execution_config_route_treats_deleting_workspace_as_not_found() {
        let (_temp, daemon) = test_daemon().await;
        let workspace = create_workspace(&daemon, "deleting-execution-config").await;
        daemon.stores().begin_workspace_delete(workspace.id).await;
        let handle = daemon.workspace_execution_config_handle_for_test();

        let get_error = handle
            .workspace_execution_config_for_route_params(WorkspaceRouteParams::new(
                workspace.id.0.to_string(),
            ))
            .await
            .unwrap_err();
        assert_eq!(get_error.kind(), WorkspaceRouteErrorKind::NotFound);
        assert_eq!(get_error.message(), "workspace not found");

        let post_error = handle
            .update_workspace_execution_config_for_route_params(
                WorkspaceRouteParams::new(workspace.id.0.to_string()),
                UpdateWorkspaceExecutionConfigRequest {
                    environment: "host".to_string(),
                    network_mode: None,
                    allowlist: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(post_error.kind(), WorkspaceRouteErrorKind::NotFound);
        assert_eq!(post_error.message(), "workspace not found");
        daemon.stores().finish_workspace_delete(workspace.id).await;
    }

    #[tokio::test]
    async fn execution_config_route_maps_unavailable_workspace_store_to_internal() {
        let (_temp, daemon) = test_daemon().await;
        let workspace = create_workspace(&daemon, "unavailable-execution-config").await;
        daemon
            .cache_rehydration_make_workspace_store_unopenable_for_test(workspace.id)
            .await
            .expect("block workspace store");
        let handle = daemon.workspace_execution_config_handle_for_test();

        let get_error = handle
            .workspace_execution_config_for_route_params(WorkspaceRouteParams::new(
                workspace.id.0.to_string(),
            ))
            .await
            .unwrap_err();
        assert_eq!(get_error.kind(), WorkspaceRouteErrorKind::Internal);

        let post_error = handle
            .update_workspace_execution_config_for_route_params(
                WorkspaceRouteParams::new(workspace.id.0.to_string()),
                UpdateWorkspaceExecutionConfigRequest {
                    environment: "host".to_string(),
                    network_mode: None,
                    allowlist: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(post_error.kind(), WorkspaceRouteErrorKind::Internal);
    }

    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn execution_config_sandbox_runtime_is_available_on_non_macos() {
        let (_temp, daemon) = test_daemon().await;
        assert!(daemon
            .workspace_execution_config_handle_for_test()
            .sandbox_runtime_available_for_execution_config());
    }
}
