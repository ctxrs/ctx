use ctx_route_contracts::workspaces::{
    UpdateWorkspaceProviderModelPreferenceRouteRequest,
    WorkspaceProviderModelPreferenceRouteParams, WorkspaceProviderModelPreferenceRouteResponse,
    WorkspaceRouteError,
};

use super::route_config::{
    provider_model_preference_error, provider_model_preference_route_response,
};
use crate::daemon::WorkspaceProviderModelPreferenceHandle;

impl WorkspaceProviderModelPreferenceHandle {
    pub async fn workspace_provider_model_preference_for_route(
        &self,
        params: WorkspaceProviderModelPreferenceRouteParams,
    ) -> Result<WorkspaceProviderModelPreferenceRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.load_workspace_provider_model_preference(workspace_id, params.provider_id())
            .await
            .map(provider_model_preference_route_response)
            .map_err(provider_model_preference_error)
    }

    pub async fn update_workspace_provider_model_preference_for_route(
        &self,
        params: WorkspaceProviderModelPreferenceRouteParams,
        req: UpdateWorkspaceProviderModelPreferenceRouteRequest,
    ) -> Result<WorkspaceProviderModelPreferenceRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.update_workspace_provider_model_preference(
            workspace_id,
            params.provider_id(),
            req.preferred_model_id,
        )
        .await
        .map(provider_model_preference_route_response)
        .map_err(provider_model_preference_error)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use ctx_core::ids::WorkspaceId;
    use ctx_core::models::{VcsKind, Workspace};
    use ctx_providers::adapters::ProviderAdapter;
    use ctx_providers::fake::FakeProviderAdapter;
    use ctx_route_contracts::workspaces::{
        UpdateWorkspaceProviderModelPreferenceRouteRequest,
        WorkspaceProviderModelPreferenceRouteParams, WorkspaceRouteErrorKind,
    };

    use crate::test_support::TestDaemon;

    async fn test_daemon() -> (tempfile::TempDir, TestDaemon) {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("codex".to_string(), Arc::new(FakeProviderAdapter::new()));
        let daemon = TestDaemon::new_with_providers_for_test(
            temp.path().to_path_buf(),
            providers,
            "http://127.0.0.1:0".to_string(),
            None,
        )
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
    async fn provider_model_preference_route_rejects_invalid_workspace_id() {
        let (_temp, daemon) = test_daemon().await;
        let error = daemon
            .workspace_provider_model_preferences_handle_for_test()
            .workspace_provider_model_preference_for_route(
                WorkspaceProviderModelPreferenceRouteParams::new("not-a-workspace", "codex"),
            )
            .await
            .unwrap_err();

        assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid workspace id");
    }

    #[tokio::test]
    async fn provider_model_preference_route_rejects_empty_provider_id() {
        let (_temp, daemon) = test_daemon().await;
        let workspace = create_workspace(&daemon, "empty-provider").await;
        let error = daemon
            .workspace_provider_model_preferences_handle_for_test()
            .workspace_provider_model_preference_for_route(
                WorkspaceProviderModelPreferenceRouteParams::new(workspace.id.0.to_string(), "   "),
            )
            .await
            .unwrap_err();

        assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "provider_id is required");
    }

    #[tokio::test]
    async fn provider_model_preference_route_rejects_unknown_provider_id() {
        let (_temp, daemon) = test_daemon().await;
        let workspace = create_workspace(&daemon, "unknown-provider").await;
        let error = daemon
            .workspace_provider_model_preferences_handle_for_test()
            .workspace_provider_model_preference_for_route(
                WorkspaceProviderModelPreferenceRouteParams::new(
                    workspace.id.0.to_string(),
                    "missing-provider",
                ),
            )
            .await
            .unwrap_err();

        assert_eq!(error.kind(), WorkspaceRouteErrorKind::NotFound);
        assert_eq!(error.message(), "provider not found: missing-provider");
    }

    #[tokio::test]
    async fn provider_model_preference_route_maps_missing_workspace_to_not_found() {
        let (_temp, daemon) = test_daemon().await;
        let missing_workspace_id = WorkspaceId::new();
        let error = daemon
            .workspace_provider_model_preferences_handle_for_test()
            .workspace_provider_model_preference_for_route(
                WorkspaceProviderModelPreferenceRouteParams::new(
                    missing_workspace_id.0.to_string(),
                    "codex",
                ),
            )
            .await
            .unwrap_err();

        assert_eq!(error.kind(), WorkspaceRouteErrorKind::NotFound);
        assert_eq!(error.message(), "workspace not found");
    }

    #[tokio::test]
    async fn provider_model_preference_route_preserves_deleting_workspace_internal_parity() {
        let (_temp, daemon) = test_daemon().await;
        let workspace = create_workspace(&daemon, "deleting-provider-pref").await;
        daemon.stores().begin_workspace_delete(workspace.id).await;
        let error = daemon
            .workspace_provider_model_preferences_handle_for_test()
            .workspace_provider_model_preference_for_route(
                WorkspaceProviderModelPreferenceRouteParams::new(
                    workspace.id.0.to_string(),
                    "codex",
                ),
            )
            .await
            .unwrap_err();

        assert_eq!(error.kind(), WorkspaceRouteErrorKind::Internal);
        daemon.stores().finish_workspace_delete(workspace.id).await;
    }

    #[tokio::test]
    async fn provider_model_preference_route_maps_unavailable_workspace_store_to_internal() {
        let (_temp, daemon) = test_daemon().await;
        let workspace = create_workspace(&daemon, "unavailable-provider-pref").await;
        daemon
            .cache_rehydration_make_workspace_store_unopenable_for_test(workspace.id)
            .await
            .expect("block workspace store");
        let error = daemon
            .workspace_provider_model_preferences_handle_for_test()
            .workspace_provider_model_preference_for_route(
                WorkspaceProviderModelPreferenceRouteParams::new(
                    workspace.id.0.to_string(),
                    "codex",
                ),
            )
            .await
            .unwrap_err();

        assert_eq!(error.kind(), WorkspaceRouteErrorKind::Internal);
    }

    #[tokio::test]
    async fn provider_model_preference_route_preserves_malformed_runtime_settings_status() {
        let (_temp, daemon) = test_daemon().await;
        let workspace = create_workspace(&daemon, "malformed-provider-pref").await;
        daemon
            .seed_invalid_workspace_runtime_settings_document_for_test(workspace.id, "{ not json")
            .await
            .expect("seed invalid runtime settings");
        let handle = daemon.workspace_provider_model_preferences_handle_for_test();

        let get_error = handle
            .workspace_provider_model_preference_for_route(
                WorkspaceProviderModelPreferenceRouteParams::new(
                    workspace.id.0.to_string(),
                    "codex",
                ),
            )
            .await
            .unwrap_err();
        assert_eq!(get_error.kind(), WorkspaceRouteErrorKind::Internal);

        let post_error = handle
            .update_workspace_provider_model_preference_for_route(
                WorkspaceProviderModelPreferenceRouteParams::new(
                    workspace.id.0.to_string(),
                    "codex",
                ),
                UpdateWorkspaceProviderModelPreferenceRouteRequest {
                    preferred_model_id: Some("gpt-5.4/xhigh".to_string()),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(post_error.kind(), WorkspaceRouteErrorKind::Internal);
    }
}
