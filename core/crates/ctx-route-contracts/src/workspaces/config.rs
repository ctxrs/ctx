use ctx_core::ids::WorkspaceId;
use serde::{Deserialize, Serialize};

use super::WorkspaceRouteError;

#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
pub struct UpdateWorkspaceExecutionConfigRequest {
    pub environment: String,
    #[serde(default)]
    pub network_mode: Option<String>,
    #[serde(default)]
    pub allowlist: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Eq, PartialEq)]
pub struct WorkspaceExecutionConfigRouteSnapshot {
    pub source: String,
    pub environment: String,
    pub network_mode: Option<String>,
    pub allowlist: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
pub struct UpdateWorkspaceMergeQueueConfigRequest {
    pub enabled: bool,
    #[serde(default)]
    pub target_branch: Option<String>,
    #[serde(default)]
    pub verify_command: Option<String>,
    #[serde(default)]
    pub push_on_success: Option<bool>,
    #[serde(default)]
    pub push_remote: Option<String>,
    #[serde(default)]
    pub push_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Eq, PartialEq)]
pub struct WorkspaceMergeQueueConfigRouteResponse {
    pub enabled: bool,
    pub target_branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_command: Option<String>,
    pub push_on_success: bool,
    pub push_remote: String,
    pub push_branch: String,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
pub struct UpdateWorktreeBootstrapConfigRequest {
    #[serde(default)]
    pub setup_command: Option<String>,
    #[serde(default)]
    pub timeout_sec: Option<u64>,
    #[serde(default)]
    pub wait_for_completion: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Eq, PartialEq)]
pub struct WorkspaceWorktreeBootstrapConfigRouteResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_sec: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_for_completion: Option<bool>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WorkspaceProviderModelPreferenceRouteParams {
    workspace_id: String,
    provider_id: String,
}

impl WorkspaceProviderModelPreferenceRouteParams {
    pub fn new(workspace_id: impl Into<String>, provider_id: impl Into<String>) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            provider_id: provider_id.into(),
        }
    }

    pub fn parse_workspace_id(&self) -> Result<WorkspaceId, WorkspaceRouteError> {
        uuid::Uuid::parse_str(&self.workspace_id)
            .map(WorkspaceId)
            .map_err(|_| WorkspaceRouteError::bad_request("invalid workspace id"))
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WorkspacePromptConfigRouteParams {
    workspace_id: String,
}

impl WorkspacePromptConfigRouteParams {
    pub fn new(workspace_id: impl Into<String>) -> Self {
        Self {
            workspace_id: workspace_id.into(),
        }
    }

    pub fn parse_workspace_id(&self) -> Result<WorkspaceId, WorkspaceRouteError> {
        uuid::Uuid::parse_str(&self.workspace_id)
            .map(WorkspaceId)
            .map_err(|_| WorkspaceRouteError::bad_request("invalid workspace id"))
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
pub struct UpdateWorkspaceProviderModelPreferenceRouteRequest {
    #[serde(default)]
    pub preferred_model_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Eq, PartialEq)]
pub struct WorkspaceProviderModelPreferenceRouteResponse {
    provider_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    preferred_model_id: Option<String>,
}

impl WorkspaceProviderModelPreferenceRouteResponse {
    pub fn new(provider_id: impl Into<String>, preferred_model_id: Option<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            preferred_model_id,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
pub struct UpdateAgentSystemPromptConfigRouteRequest {
    #[serde(default)]
    pub system_prompt_append: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
pub struct UpdateSubagentSystemPromptConfigRouteRequest {
    #[serde(default)]
    pub system_prompt_append: Option<String>,
}

#[derive(Debug, Clone, Serialize, Eq, PartialEq)]
pub struct AgentSystemPromptConfigRouteResponse {
    default_append: String,
    configured_append: Option<String>,
    effective_append: Option<String>,
    source: String,
}

impl AgentSystemPromptConfigRouteResponse {
    pub fn new(
        default_append: impl Into<String>,
        configured_append: Option<String>,
        effective_append: Option<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            default_append: default_append.into(),
            configured_append,
            effective_append,
            source: source.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Eq, PartialEq)]
pub struct SubagentSystemPromptConfigRouteResponse {
    default_append: String,
    configured_append: Option<String>,
    effective_append: Option<String>,
    source: String,
}

impl SubagentSystemPromptConfigRouteResponse {
    pub fn new(
        default_append: impl Into<String>,
        configured_append: Option<String>,
        effective_append: Option<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            default_append: default_append.into(),
            configured_append,
            effective_append,
            source: source.into(),
        }
    }
}
