use ctx_core::ids::WorkspaceId;
use ctx_observability::logs;
use ctx_provider_runtime::provider_options::service::{
    finish_provider_options_response, prepare_provider_options_response, ProviderOptionsPreflight,
    ProviderOptionsPreflightRequest, ProviderOptionsServiceError, ProviderOptionsWorkspaceInput,
};
use ctx_provider_runtime::{ProviderOptionsRouteError, ProviderOptionsRouteRequest};
use serde_json::Value;

use crate::daemon::providers::ProviderLaunchConfigError;
use crate::daemon::{ProviderOptionsHandle, ProviderWorkspaceLaunchRuntime};

mod load;

use load::load_provider_options_workspace_inputs;

#[derive(Debug)]
pub enum ProviderOptionsResponseError {
    ExecutionSettings(anyhow::Error),
    ProviderLaunchConfig(ProviderLaunchConfigError),
    WorkspaceLoad,
    WorkspaceNotFound,
    WorkspaceStoreLoad(anyhow::Error),
    WorkspacePreferenceLoad(anyhow::Error),
    SelectedEndpointMissing,
}

pub(in crate::daemon) async fn get_provider_options_response(
    launch: &ProviderWorkspaceLaunchRuntime,
    workspace_id: WorkspaceId,
    provider_id: &str,
) -> Result<Value, ProviderOptionsResponseError> {
    launch.sync_plugin_provider_adapters().await;
    let install_target = launch
        .install_target_for_workspace(workspace_id)
        .await
        .map_err(ProviderOptionsResponseError::ExecutionSettings)?;
    let preflight = prepare_provider_options_response(
        launch,
        ProviderOptionsPreflightRequest {
            workspace_id,
            provider_id,
            install_target,
        },
    )
    .await
    .map_err(provider_options_service_error)?;
    let prepared = match preflight {
        ProviderOptionsPreflight::Cached(out) => return Ok(out),
        ProviderOptionsPreflight::NeedsWorkspace(prepared) => prepared,
    };
    let workspace_inputs =
        load_provider_options_workspace_inputs(launch, workspace_id, provider_id).await?;
    finish_provider_options_response(
        launch,
        ProviderOptionsWorkspaceInput {
            prepared,
            workspace: &workspace_inputs.workspace,
            preferred_model_id: workspace_inputs.preferred_model_id,
        },
    )
    .await
    .map_err(provider_options_service_error)
}

fn provider_options_service_error(
    error: ProviderOptionsServiceError,
) -> ProviderOptionsResponseError {
    match error {
        ProviderOptionsServiceError::ProviderLaunchConfig(error) => {
            ProviderOptionsResponseError::ProviderLaunchConfig(error)
        }
        ProviderOptionsServiceError::SelectedEndpointMissing => {
            ProviderOptionsResponseError::SelectedEndpointMissing
        }
    }
}

impl ProviderOptionsHandle {
    pub async fn get_provider_options_for_route(
        &self,
        request: ProviderOptionsRouteRequest,
    ) -> Result<Value, ProviderOptionsRouteError> {
        let (workspace_id_raw, provider_id) = request.into_parts();
        let workspace_id = parse_workspace_id_for_options_route(&workspace_id_raw)?;
        get_provider_options_response(self.launch(), workspace_id, &provider_id)
            .await
            .map_err(provider_options_route_error)
    }
}

fn parse_workspace_id_for_options_route(
    raw: &str,
) -> Result<WorkspaceId, ProviderOptionsRouteError> {
    uuid::Uuid::parse_str(raw)
        .map(WorkspaceId)
        .map_err(|_| ProviderOptionsRouteError::bad_request("invalid workspace id"))
}

fn provider_options_route_error(error: ProviderOptionsResponseError) -> ProviderOptionsRouteError {
    match error {
        ProviderOptionsResponseError::ExecutionSettings(error) => {
            ProviderOptionsRouteError::internal_server_error(format!(
                "failed to load workspace execution settings: {error:#}"
            ))
        }
        ProviderOptionsResponseError::ProviderLaunchConfig(error) => {
            provider_launch_config_options_route_error(error)
        }
        ProviderOptionsResponseError::WorkspaceLoad => {
            ProviderOptionsRouteError::internal_server_error("failed to load workspace")
        }
        ProviderOptionsResponseError::WorkspaceNotFound => {
            ProviderOptionsRouteError::not_found("workspace not found")
        }
        ProviderOptionsResponseError::WorkspaceStoreLoad(error) => {
            ProviderOptionsRouteError::internal_server_error(format!(
                "failed to load workspace store: {}",
                logs::redact_sensitive(&error.to_string())
            ))
        }
        ProviderOptionsResponseError::WorkspacePreferenceLoad(error) => {
            ProviderOptionsRouteError::internal_server_error(format!(
                "failed to load workspace provider model preference: {}",
                logs::redact_sensitive(&error.to_string())
            ))
        }
        ProviderOptionsResponseError::SelectedEndpointMissing => {
            ProviderOptionsRouteError::internal_server_error(
                "selected endpoint missing from provider configuration",
            )
        }
    }
}

fn provider_launch_config_options_route_error(
    error: ProviderLaunchConfigError,
) -> ProviderOptionsRouteError {
    match error {
        ProviderLaunchConfigError::UnsupportedProvider { provider_id } => {
            ProviderOptionsRouteError::bad_request(format!(
                "unsupported provider id: {provider_id}"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_provider_runtime::ProviderOptionsRouteErrorStatus;

    #[test]
    fn provider_options_route_error_preserves_basic_status_bodies() {
        let invalid_workspace = parse_workspace_id_for_options_route("not-a-uuid").unwrap_err();
        assert_eq!(
            invalid_workspace.status(),
            ProviderOptionsRouteErrorStatus::BadRequest
        );
        assert_eq!(
            invalid_workspace.body()["error"].as_str(),
            Some("invalid workspace id")
        );

        let unsupported =
            provider_options_route_error(ProviderOptionsResponseError::ProviderLaunchConfig(
                ProviderLaunchConfigError::UnsupportedProvider {
                    provider_id: "missing-provider".to_string(),
                },
            ));
        assert_eq!(
            unsupported.status(),
            ProviderOptionsRouteErrorStatus::BadRequest
        );
        assert_eq!(
            unsupported.body()["error"].as_str(),
            Some("unsupported provider id: missing-provider")
        );

        let missing_workspace =
            provider_options_route_error(ProviderOptionsResponseError::WorkspaceNotFound);
        assert_eq!(
            missing_workspace.status(),
            ProviderOptionsRouteErrorStatus::NotFound
        );
        assert_eq!(
            missing_workspace.body()["error"].as_str(),
            Some("workspace not found")
        );

        let workspace_load =
            provider_options_route_error(ProviderOptionsResponseError::WorkspaceLoad);
        assert_eq!(
            workspace_load.status(),
            ProviderOptionsRouteErrorStatus::InternalServerError
        );
        assert_eq!(
            workspace_load.body()["error"].as_str(),
            Some("failed to load workspace")
        );

        let selected_endpoint =
            provider_options_route_error(ProviderOptionsResponseError::SelectedEndpointMissing);
        assert_eq!(
            selected_endpoint.status(),
            ProviderOptionsRouteErrorStatus::InternalServerError
        );
        assert_eq!(
            selected_endpoint.body()["error"].as_str(),
            Some("selected endpoint missing from provider configuration")
        );
    }

    #[test]
    fn provider_options_route_error_preserves_execution_settings_prefix() {
        let error = provider_options_route_error(ProviderOptionsResponseError::ExecutionSettings(
            anyhow::anyhow!("settings failed"),
        ));

        assert_eq!(
            error.status(),
            ProviderOptionsRouteErrorStatus::InternalServerError
        );
        assert!(error.body()["error"].as_str().is_some_and(|message| message
            .starts_with("failed to load workspace execution settings: settings failed")));
    }

    #[test]
    fn provider_options_route_error_redacts_store_and_preference_failures() {
        let store = provider_options_route_error(ProviderOptionsResponseError::WorkspaceStoreLoad(
            anyhow::anyhow!("store failed with Authorization: Bearer store-secret"),
        ));
        assert_eq!(
            store.status(),
            ProviderOptionsRouteErrorStatus::InternalServerError
        );
        let store_message = store.body()["error"].as_str().unwrap();
        assert!(store_message.starts_with("failed to load workspace store: "));
        assert!(
            !store_message.contains("store-secret"),
            "store secret leaked in {store_message}"
        );

        let preference =
            provider_options_route_error(ProviderOptionsResponseError::WorkspacePreferenceLoad(
                anyhow::anyhow!("preference failed with OPENAI_API_KEY=preference-secret"),
            ));
        assert_eq!(
            preference.status(),
            ProviderOptionsRouteErrorStatus::InternalServerError
        );
        let preference_message = preference.body()["error"].as_str().unwrap();
        assert!(
            preference_message.starts_with("failed to load workspace provider model preference: ")
        );
        assert!(
            !preference_message.contains("preference-secret"),
            "preference secret leaked in {preference_message}"
        );
    }
}
