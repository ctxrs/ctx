use ctx_observability::logs;
use ctx_route_contracts::workspaces::{
    AgentSystemPromptConfigRouteResponse, SubagentSystemPromptConfigRouteResponse,
    WorkspaceProviderModelPreferenceRouteResponse, WorkspaceRouteError,
};
use ctx_workspace_config as workspace_config;

use crate::daemon::workspaces::{
    WorkspaceProviderModelPreference, WorkspaceProviderModelPreferenceError,
};
use crate::daemon::WorkspaceStoreAccessError;

pub(in crate::daemon::workspaces) fn provider_model_preference_error(
    error: WorkspaceProviderModelPreferenceError,
) -> WorkspaceRouteError {
    match error {
        WorkspaceProviderModelPreferenceError::ProviderIdRequired => {
            WorkspaceRouteError::bad_request("provider_id is required")
        }
        WorkspaceProviderModelPreferenceError::ProviderNotFound { provider_id } => {
            WorkspaceRouteError::not_found(format!("provider not found: {provider_id}"))
        }
        WorkspaceProviderModelPreferenceError::WorkspaceNotFound => {
            WorkspaceRouteError::not_found("workspace not found")
        }
        WorkspaceProviderModelPreferenceError::StoreUnavailable(error) => {
            WorkspaceRouteError::internal(logs::redact_sensitive(&error.to_string()))
        }
        WorkspaceProviderModelPreferenceError::ExecutionSettings(error) => {
            WorkspaceRouteError::internal(format!(
                "failed to load workspace execution settings: {error:#}"
            ))
        }
    }
}

pub(in crate::daemon::workspaces) fn provider_model_preference_route_response(
    value: WorkspaceProviderModelPreference,
) -> WorkspaceProviderModelPreferenceRouteResponse {
    WorkspaceProviderModelPreferenceRouteResponse::new(value.provider_id, value.preferred_model_id)
}

pub(in crate::daemon::workspaces) fn workspace_store_error(
    error: WorkspaceStoreAccessError,
) -> WorkspaceRouteError {
    match error {
        WorkspaceStoreAccessError::NotFound => {
            WorkspaceRouteError::not_found("workspace not found")
        }
        WorkspaceStoreAccessError::Unavailable(error) => {
            WorkspaceRouteError::internal(logs::redact_sensitive(&error.to_string()))
        }
    }
}

fn source_label(source: workspace_config::AgentSystemPromptAppendSource) -> String {
    match source {
        workspace_config::AgentSystemPromptAppendSource::Default => "default".to_string(),
        workspace_config::AgentSystemPromptAppendSource::Config => "config".to_string(),
        workspace_config::AgentSystemPromptAppendSource::Disabled => "disabled".to_string(),
    }
}

fn configured_append(value: &Option<String>) -> Option<String> {
    value.as_ref().map(|value| value.trim().to_string())
}

pub(in crate::daemon::workspaces) fn agent_system_prompt_config_route_response(
    cfg: workspace_config::AgentSystemPromptAppendConfig,
) -> AgentSystemPromptConfigRouteResponse {
    AgentSystemPromptConfigRouteResponse::new(
        cfg.default_append.clone(),
        configured_append(&cfg.configured_append),
        cfg.effective_append(),
        source_label(cfg.source()),
    )
}

pub(in crate::daemon::workspaces) fn subagent_system_prompt_config_route_response(
    cfg: workspace_config::SubagentSystemPromptAppendConfig,
) -> SubagentSystemPromptConfigRouteResponse {
    SubagentSystemPromptConfigRouteResponse::new(
        cfg.default_append.clone(),
        configured_append(&cfg.configured_append),
        cfg.effective_append(),
        source_label(cfg.source()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_route_contracts::workspaces::{
        UpdateWorkspaceProviderModelPreferenceRouteRequest,
        WorkspaceProviderModelPreferenceRouteParams,
    };
    use serde_json::json;

    #[test]
    fn provider_preference_response_omits_absent_model() {
        let response = provider_model_preference_route_response(WorkspaceProviderModelPreference {
            provider_id: "codex".to_string(),
            preferred_model_id: None,
        });

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({
                "provider_id": "codex"
            })
        );
    }

    #[test]
    fn provider_preference_error_mapping_preserves_wire_messages() {
        use ctx_route_contracts::workspaces::WorkspaceRouteErrorKind;

        let required = provider_model_preference_error(
            WorkspaceProviderModelPreferenceError::ProviderIdRequired,
        );
        assert_eq!(required.kind(), WorkspaceRouteErrorKind::BadRequest);
        assert_eq!(required.message(), "provider_id is required");

        let missing = provider_model_preference_error(
            WorkspaceProviderModelPreferenceError::ProviderNotFound {
                provider_id: "missing".to_string(),
            },
        );
        assert_eq!(missing.kind(), WorkspaceRouteErrorKind::NotFound);
        assert_eq!(missing.message(), "provider not found: missing");

        let workspace = provider_model_preference_error(
            WorkspaceProviderModelPreferenceError::WorkspaceNotFound,
        );
        assert_eq!(workspace.kind(), WorkspaceRouteErrorKind::NotFound);
        assert_eq!(workspace.message(), "workspace not found");

        let execution = provider_model_preference_error(
            WorkspaceProviderModelPreferenceError::ExecutionSettings(anyhow::anyhow!(
                "bad settings"
            )),
        );
        assert_eq!(execution.kind(), WorkspaceRouteErrorKind::Internal);
        assert!(execution
            .message()
            .starts_with("failed to load workspace execution settings:"));
    }

    #[test]
    fn route_params_reject_invalid_ids_with_wire_message() {
        use ctx_route_contracts::workspaces::WorkspaceRouteErrorKind;

        let error = WorkspaceProviderModelPreferenceRouteParams::new("not-a-workspace", "codex")
            .parse_workspace_id()
            .unwrap_err();
        assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid workspace id");
    }

    #[test]
    fn prompt_response_projection_preserves_source_and_trimming() {
        let response = agent_system_prompt_config_route_response(
            workspace_config::AgentSystemPromptAppendConfig {
                default_append: "Default".to_string(),
                configured_append: Some("  Configured  ".to_string()),
            },
        );

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({
                "default_append": "Default",
                "configured_append": "Configured",
                "effective_append": "Configured",
                "source": "config"
            })
        );

        let response = subagent_system_prompt_config_route_response(
            workspace_config::SubagentSystemPromptAppendConfig {
                default_append: "Subagent default".to_string(),
                configured_append: None,
            },
        );

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({
                "default_append": "Subagent default",
                "configured_append": null,
                "effective_append": "Subagent default",
                "source": "default"
            })
        );
    }

    #[test]
    fn provider_preference_update_requests_keep_raw_normalization_inputs() {
        let trimmed: UpdateWorkspaceProviderModelPreferenceRouteRequest =
            serde_json::from_value(json!({
                "preferred_model_id": " gpt-5.4/xhigh "
            }))
            .unwrap();
        assert_eq!(
            trimmed.preferred_model_id.as_deref(),
            Some(" gpt-5.4/xhigh ")
        );

        let blank: UpdateWorkspaceProviderModelPreferenceRouteRequest =
            serde_json::from_value(json!({
                "preferred_model_id": "   "
            }))
            .unwrap();
        assert_eq!(blank.preferred_model_id.as_deref(), Some("   "));
    }
}
