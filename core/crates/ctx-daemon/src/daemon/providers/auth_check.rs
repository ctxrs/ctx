use ctx_core::ids::WorkspaceId;
pub use ctx_provider_runtime::provider_auth_check::ProviderAuthCheckSnapshot;
use ctx_provider_runtime::provider_auth_check::{
    ProviderAuthCheckServiceError, ProviderWorkspaceAuthenticationError,
};
pub use ctx_provider_runtime::provider_launch::config_snapshot::ProviderLaunchConfigError;
use ctx_provider_runtime::{
    AuthenticateProviderForWorkspaceRouteRequest, ProviderAuthCheckRouteError,
    ProviderAuthCheckRouteResponse, VerifyProviderForWorkspaceRouteRequest,
};

use crate::daemon::{ProviderWorkspaceAuthHandle, ProviderWorkspaceLaunchRuntime};

mod workspace;

use workspace::load_workspace;

#[derive(Debug)]
pub enum ProviderAuthCheckError {
    WorkspaceLoad,
    WorkspaceNotFound,
    ExecutionSettings(anyhow::Error),
    ProviderLaunchConfig(ProviderLaunchConfigError),
    Verify(String),
}

pub(in crate::daemon) async fn authenticate_provider_for_workspace(
    launch: &ProviderWorkspaceLaunchRuntime,
    workspace_id: WorkspaceId,
    provider_id: &str,
    method_id: Option<String>,
) -> Result<ProviderAuthCheckSnapshot, ProviderAuthCheckError> {
    let workspace = load_workspace(launch, workspace_id).await?;
    let install_target = launch
        .install_target_for_workspace(workspace.id)
        .await
        .map_err(ProviderAuthCheckError::ExecutionSettings)?;
    ctx_provider_runtime::provider_auth_check::authenticate_provider_for_workspace_runtime(
        launch,
        &workspace,
        workspace_id,
        provider_id,
        install_target,
        method_id,
    )
    .await
    .map_err(|error| match error {
        ProviderWorkspaceAuthenticationError::Verify(error) => {
            ProviderAuthCheckError::Verify(error)
        }
    })
}

impl ProviderWorkspaceAuthHandle {
    pub async fn authenticate_provider_for_workspace_for_route(
        &self,
        request: AuthenticateProviderForWorkspaceRouteRequest,
    ) -> Result<ProviderAuthCheckRouteResponse, ProviderAuthCheckRouteError> {
        let (workspace_id_raw, provider_id, method_id) = request.into_parts();
        let workspace_id = parse_workspace_id_for_auth_route(&workspace_id_raw)?;
        authenticate_provider_for_workspace(self.launch(), workspace_id, &provider_id, method_id)
            .await
            .map(ProviderAuthCheckRouteResponse::from)
            .map_err(provider_auth_check_route_error)
    }

    pub async fn verify_provider_for_workspace_for_route(
        &self,
        request: VerifyProviderForWorkspaceRouteRequest,
    ) -> Result<ProviderAuthCheckRouteResponse, ProviderAuthCheckRouteError> {
        let (workspace_id_raw, provider_id) = request.into_parts();
        let workspace_id = parse_workspace_id_for_auth_route(&workspace_id_raw)?;
        verify_provider_for_workspace(self.launch(), workspace_id, &provider_id)
            .await
            .map(ProviderAuthCheckRouteResponse::from)
            .map_err(provider_auth_check_route_error)
    }
}

fn parse_workspace_id_for_auth_route(
    raw: &str,
) -> Result<WorkspaceId, ProviderAuthCheckRouteError> {
    uuid::Uuid::parse_str(raw)
        .map(WorkspaceId)
        .map_err(|_| ProviderAuthCheckRouteError::bad_request("invalid workspace id"))
}

fn provider_auth_check_route_error(error: ProviderAuthCheckError) -> ProviderAuthCheckRouteError {
    match error {
        ProviderAuthCheckError::WorkspaceLoad => {
            ProviderAuthCheckRouteError::internal_server_error("failed to load workspace")
        }
        ProviderAuthCheckError::WorkspaceNotFound => {
            ProviderAuthCheckRouteError::not_found("workspace not found")
        }
        ProviderAuthCheckError::ExecutionSettings(error) => {
            ProviderAuthCheckRouteError::internal_server_error(format!(
                "failed to load workspace execution settings: {error:#}"
            ))
        }
        ProviderAuthCheckError::ProviderLaunchConfig(error) => {
            provider_launch_config_route_error(error)
        }
        ProviderAuthCheckError::Verify(error) => ProviderAuthCheckRouteError::bad_request(error),
    }
}

fn provider_launch_config_route_error(
    error: ProviderLaunchConfigError,
) -> ProviderAuthCheckRouteError {
    match error {
        ProviderLaunchConfigError::UnsupportedProvider { provider_id } => {
            ProviderAuthCheckRouteError::bad_request(format!(
                "unsupported provider id: {provider_id}"
            ))
        }
    }
}

pub(in crate::daemon) async fn verify_provider_for_workspace(
    launch: &ProviderWorkspaceLaunchRuntime,
    workspace_id: WorkspaceId,
    provider_id: &str,
) -> Result<ProviderAuthCheckSnapshot, ProviderAuthCheckError> {
    let workspace = load_workspace(launch, workspace_id).await?;
    let install_target = launch
        .install_target_for_workspace(workspace.id)
        .await
        .map_err(ProviderAuthCheckError::ExecutionSettings)?;
    ctx_provider_runtime::provider_auth_check::verify_provider_for_workspace_runtime(
        launch,
        &workspace,
        workspace_id,
        provider_id,
        install_target,
    )
    .await
    .map_err(|error| match error {
        ProviderAuthCheckServiceError::ProviderLaunchConfig(error) => {
            ProviderAuthCheckError::ProviderLaunchConfig(error)
        }
        ProviderAuthCheckServiceError::Verify(error) => ProviderAuthCheckError::Verify(error),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_provider_runtime::ProviderAuthCheckRouteErrorStatus;

    #[test]
    fn auth_route_parse_error_preserves_json_body() {
        let error = parse_workspace_id_for_auth_route("not-a-uuid").unwrap_err();

        assert_eq!(
            error.status(),
            ProviderAuthCheckRouteErrorStatus::BadRequest
        );
        assert_eq!(error.body()["error"].as_str(), Some("invalid workspace id"));
    }

    #[test]
    fn auth_route_error_preserves_basic_status_bodies() {
        let missing_workspace =
            provider_auth_check_route_error(ProviderAuthCheckError::WorkspaceNotFound);
        assert_eq!(
            missing_workspace.status(),
            ProviderAuthCheckRouteErrorStatus::NotFound
        );
        assert_eq!(
            missing_workspace.body()["error"].as_str(),
            Some("workspace not found")
        );

        let workspace_load = provider_auth_check_route_error(ProviderAuthCheckError::WorkspaceLoad);
        assert_eq!(
            workspace_load.status(),
            ProviderAuthCheckRouteErrorStatus::InternalServerError
        );
        assert_eq!(
            workspace_load.body()["error"].as_str(),
            Some("failed to load workspace")
        );

        let unsupported =
            provider_auth_check_route_error(ProviderAuthCheckError::ProviderLaunchConfig(
                ProviderLaunchConfigError::UnsupportedProvider {
                    provider_id: "missing-provider".to_string(),
                },
            ));
        assert_eq!(
            unsupported.status(),
            ProviderAuthCheckRouteErrorStatus::BadRequest
        );
        assert_eq!(
            unsupported.body()["error"].as_str(),
            Some("unsupported provider id: missing-provider")
        );
    }

    #[test]
    fn auth_route_error_preserves_execution_settings_and_verify_messages() {
        let execution = provider_auth_check_route_error(ProviderAuthCheckError::ExecutionSettings(
            anyhow::anyhow!("settings failed"),
        ));
        assert_eq!(
            execution.status(),
            ProviderAuthCheckRouteErrorStatus::InternalServerError
        );
        assert!(execution.body()["error"]
            .as_str()
            .is_some_and(|message| message
                .starts_with("failed to load workspace execution settings: settings failed")));

        let verify = provider_auth_check_route_error(ProviderAuthCheckError::Verify(
            "login required".to_string(),
        ));
        assert_eq!(
            verify.status(),
            ProviderAuthCheckRouteErrorStatus::BadRequest
        );
        assert_eq!(verify.body()["error"].as_str(), Some("login required"));
    }
}
