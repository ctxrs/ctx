use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use ctx_core::ids::WorkspaceId;
use ctx_core::models::{Workspace, Worktree};
#[cfg(test)]
use ctx_core::provider_policy::CTX_CRP_LAUNCH_POLICY_ENV;
use ctx_harness_sources::ResolvedHarnessSource;

use crate::provider_auth::provider_has_active_auth_config_with_runtime_root;

mod env;

#[cfg(test)]
use env::insert_probe_source_env;
use env::{
    finalize_workspace_probe_env, provider_env_with_runtime_root, strip_daemon_owned_probe_env,
};

#[derive(Debug, Clone)]
pub struct PreparedWorkspaceProbeRuntime {
    pub cwd: PathBuf,
    pub runtime_data_root: Option<PathBuf>,
    pub env_overrides: HashMap<String, String>,
}

pub struct WorkspaceRuntimeProbeContext {
    pub source: ResolvedHarnessSource,
    pub env: HashMap<String, String>,
    pub cwd: PathBuf,
}

#[async_trait]
pub trait ProviderProbeHost: Send + Sync + 'static {
    fn data_root(&self) -> &Path;
    fn daemon_url(&self) -> &str;
    fn auth_token(&self) -> Option<&String>;
    fn redact_sensitive(&self, input: &str) -> String;

    async fn load_workspace(&self, workspace_id: WorkspaceId) -> Result<Option<Workspace>, String>;

    async fn prepare_workspace_probe_runtime(
        &self,
        workspace: &Workspace,
    ) -> Result<PreparedWorkspaceProbeRuntime, String>;

    async fn prepare_worktree_probe_runtime(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
    ) -> Result<PreparedWorkspaceProbeRuntime, String>;
}

pub async fn provider_probe_env<H>(
    state: &H,
    provider_id: &str,
) -> Result<(ResolvedHarnessSource, HashMap<String, String>), String>
where
    H: ProviderProbeHost,
{
    provider_env_with_runtime_root(state, provider_id, None, true, false, true).await
}

async fn provider_context_for_workspace_runtime<H>(
    state: &H,
    workspace: &Workspace,
    provider_id: &str,
    require_subscription_account_env: bool,
    disable_mcp: bool,
) -> Result<WorkspaceRuntimeProbeContext, String>
where
    H: ProviderProbeHost,
{
    let runtime = state.prepare_workspace_probe_runtime(workspace).await?;
    let (source, mut env) = provider_env_with_runtime_root(
        state,
        provider_id,
        runtime.runtime_data_root.as_deref(),
        require_subscription_account_env,
        false,
        disable_mcp,
    )
    .await?;
    for (key, value) in runtime.env_overrides {
        env.insert(key, value);
    }
    finalize_workspace_probe_env(state, &source, provider_id, &mut env).await?;
    strip_daemon_owned_probe_env(&mut env);
    Ok(WorkspaceRuntimeProbeContext {
        source,
        env,
        cwd: runtime.cwd,
    })
}

pub async fn provider_auth_context_for_worktree_runtime<H>(
    state: &H,
    worktree: &Worktree,
    provider_id: &str,
) -> Result<WorkspaceRuntimeProbeContext, String>
where
    H: ProviderProbeHost,
{
    let workspace = state
        .load_workspace(worktree.workspace_id)
        .await?
        .ok_or_else(|| "workspace not found".to_string())?;
    let runtime = state
        .prepare_worktree_probe_runtime(&workspace, worktree)
        .await?;
    let (source, mut env) = provider_env_with_runtime_root(
        state,
        provider_id,
        runtime.runtime_data_root.as_deref(),
        false,
        true,
        false,
    )
    .await?;
    for (key, value) in runtime.env_overrides {
        env.insert(key, value);
    }
    finalize_workspace_probe_env(state, &source, provider_id, &mut env).await?;
    strip_daemon_owned_probe_env(&mut env);
    Ok(WorkspaceRuntimeProbeContext {
        source,
        env,
        cwd: runtime.cwd,
    })
}

pub async fn provider_probe_context_for_workspace_runtime<H>(
    state: &H,
    workspace: &Workspace,
    provider_id: &str,
) -> Result<WorkspaceRuntimeProbeContext, String>
where
    H: ProviderProbeHost,
{
    provider_context_for_workspace_runtime(state, workspace, provider_id, true, false).await
}

pub async fn provider_auth_context_for_workspace_runtime<H>(
    state: &H,
    workspace: &Workspace,
    provider_id: &str,
) -> Result<WorkspaceRuntimeProbeContext, String>
where
    H: ProviderProbeHost,
{
    provider_context_for_workspace_runtime(state, workspace, provider_id, false, true).await
}

pub async fn provider_has_active_auth_for_workspace_runtime<H>(
    state: &H,
    workspace: &Workspace,
    provider_id: &str,
    source_config: Option<&ctx_harness_sources::HarnessProviderSourceConfig>,
) -> Result<bool, String>
where
    H: ProviderProbeHost,
{
    let runtime_root =
        provider_context_for_workspace_runtime(state, workspace, provider_id, false, false)
            .await?
            .env
            .get("CTX_DATA_ROOT")
            .map(PathBuf::from);
    provider_has_active_auth_config_with_runtime_root(
        state.data_root(),
        runtime_root.as_deref(),
        provider_id,
        source_config,
    )
    .await
}

pub async fn provider_probe_env_for_workspace_runtime<H>(
    state: &H,
    workspace: &Workspace,
    provider_id: &str,
) -> Result<(ResolvedHarnessSource, HashMap<String, String>), String>
where
    H: ProviderProbeHost,
{
    let context =
        provider_probe_context_for_workspace_runtime(state, workspace, provider_id).await?;
    Ok((context.source, context.env))
}

#[cfg(test)]
mod tests;
