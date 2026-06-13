use ctx_core::ids::WorkspaceId;
use ctx_route_contracts::workspaces::{
    AgentSystemPromptConfigRouteResponse, SubagentSystemPromptConfigRouteResponse,
    UpdateAgentSystemPromptConfigRouteRequest, UpdateSubagentSystemPromptConfigRouteRequest,
    UpdateWorktreeBootstrapConfigRequest, WorkspaceConfigUpdateResult,
    WorkspacePromptConfigRouteParams, WorkspaceRouteError, WorkspaceRouteParams,
    WorkspaceWorktreeBootstrapConfigRouteResponse,
};
use ctx_workspace_config as workspace_config;

use super::route_config::{
    agent_system_prompt_config_route_response, subagent_system_prompt_config_route_response,
    workspace_store_error, workspace_store_route_error, worktree_bootstrap_config_route_response,
    worktree_bootstrap_config_update,
};
use crate::daemon::{WorkspacePromptBootstrapConfigHandle, WorkspaceStoreAccessError};

impl WorkspacePromptBootstrapConfigHandle {
    pub async fn worktree_bootstrap_config_for_route(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<WorkspaceWorktreeBootstrapConfigRouteResponse, WorkspaceRouteError> {
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        let cfg = workspace_config::load_worktree_bootstrap_config(&store)
            .await
            .map_err(WorkspaceRouteError::internal)?;
        Ok(worktree_bootstrap_config_route_response(cfg))
    }

    pub async fn update_worktree_bootstrap_config_for_route(
        &self,
        workspace_id: WorkspaceId,
        req: UpdateWorktreeBootstrapConfigRequest,
    ) -> Result<WorkspaceConfigUpdateResult, WorkspaceRouteError> {
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        workspace_config::update_worktree_bootstrap_config(
            &store,
            worktree_bootstrap_config_update(req),
        )
        .await
        .map_err(WorkspaceRouteError::bad_request)?;
        Ok(WorkspaceConfigUpdateResult { ok: true })
    }

    pub async fn load_agent_system_prompt_append(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<workspace_config::AgentSystemPromptAppendConfig, WorkspaceStoreAccessError> {
        let store = self.existing_workspace_store(workspace_id).await?;
        workspace_config::load_agent_system_prompt_append(&store)
            .await
            .map_err(WorkspaceStoreAccessError::Unavailable)
    }

    pub async fn update_agent_system_prompt_append(
        &self,
        workspace_id: WorkspaceId,
        system_prompt_append: Option<String>,
    ) -> Result<workspace_config::AgentSystemPromptAppendConfig, WorkspaceStoreAccessError> {
        let store = self.existing_workspace_store(workspace_id).await?;
        workspace_config::update_and_load_agent_system_prompt_append(&store, system_prompt_append)
            .await
            .map_err(WorkspaceStoreAccessError::Unavailable)
    }

    pub async fn load_subagent_system_prompt_append(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<workspace_config::SubagentSystemPromptAppendConfig, WorkspaceStoreAccessError> {
        let store = self.existing_workspace_store(workspace_id).await?;
        workspace_config::load_subagent_system_prompt_append(&store)
            .await
            .map_err(WorkspaceStoreAccessError::Unavailable)
    }

    pub async fn update_subagent_system_prompt_append(
        &self,
        workspace_id: WorkspaceId,
        system_prompt_append: Option<String>,
    ) -> Result<workspace_config::SubagentSystemPromptAppendConfig, WorkspaceStoreAccessError> {
        let store = self.existing_workspace_store(workspace_id).await?;
        workspace_config::update_and_load_subagent_system_prompt_append(
            &store,
            system_prompt_append,
        )
        .await
        .map_err(WorkspaceStoreAccessError::Unavailable)
    }

    pub async fn agent_system_prompt_config_for_route(
        &self,
        params: WorkspacePromptConfigRouteParams,
    ) -> Result<AgentSystemPromptConfigRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.load_agent_system_prompt_append(workspace_id)
            .await
            .map(agent_system_prompt_config_route_response)
            .map_err(workspace_store_error)
    }

    pub async fn update_agent_system_prompt_config_for_route(
        &self,
        params: WorkspacePromptConfigRouteParams,
        req: UpdateAgentSystemPromptConfigRouteRequest,
    ) -> Result<AgentSystemPromptConfigRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.update_agent_system_prompt_append(workspace_id, req.system_prompt_append)
            .await
            .map(agent_system_prompt_config_route_response)
            .map_err(workspace_store_error)
    }

    pub async fn subagent_system_prompt_config_for_route(
        &self,
        params: WorkspacePromptConfigRouteParams,
    ) -> Result<SubagentSystemPromptConfigRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.load_subagent_system_prompt_append(workspace_id)
            .await
            .map(subagent_system_prompt_config_route_response)
            .map_err(workspace_store_error)
    }

    pub async fn update_subagent_system_prompt_config_for_route(
        &self,
        params: WorkspacePromptConfigRouteParams,
        req: UpdateSubagentSystemPromptConfigRouteRequest,
    ) -> Result<SubagentSystemPromptConfigRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.update_subagent_system_prompt_append(workspace_id, req.system_prompt_append)
            .await
            .map(subagent_system_prompt_config_route_response)
            .map_err(workspace_store_error)
    }

    pub async fn worktree_bootstrap_config_for_route_params(
        &self,
        params: WorkspaceRouteParams,
    ) -> Result<WorkspaceWorktreeBootstrapConfigRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.worktree_bootstrap_config_for_route(workspace_id).await
    }

    pub async fn update_worktree_bootstrap_config_for_route_params(
        &self,
        params: WorkspaceRouteParams,
        request: UpdateWorktreeBootstrapConfigRequest,
    ) -> Result<WorkspaceConfigUpdateResult, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.update_worktree_bootstrap_config_for_route(workspace_id, request)
            .await
    }
}
