use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ctx_core::models::Session;
use ctx_providers::adapters::ProviderAdapter;

use crate::daemon::session_control_effects::SessionAuthRuntimeHost;

use super::SessionAuthError;

pub(in crate::daemon) struct PreparedSessionAuth {
    pub(in crate::daemon) adapter: Arc<dyn ProviderAdapter>,
    pub(in crate::daemon) workdir: PathBuf,
    pub(in crate::daemon) provider_env: HashMap<String, String>,
}

pub(in crate::daemon) async fn prepare_session_auth_runtime(
    host: &SessionAuthRuntimeHost,
    store: &ctx_store::Store,
    session: &Session,
) -> Result<PreparedSessionAuth, SessionAuthError> {
    let worktree = store
        .get_worktree(session.worktree_id)
        .await
        .map_err(|_| SessionAuthError::Internal("failed to load worktree".to_string()))?
        .ok_or(SessionAuthError::NotFound("worktree"))?;
    let workspace = host
        .load_workspace(worktree.workspace_id)
        .await
        .map_err(|error| {
            SessionAuthError::Internal(format!("failed to load workspace: {error:#}"))
        })?
        .ok_or(SessionAuthError::NotFound("workspace"))?;
    let resolved_worktree = host
        .resolve_existing_worktree_execution(store, worktree.id)
        .await
        .map_err(|error| {
            SessionAuthError::Internal(format!(
                "failed to resolve session worktree execution: {error:#}"
            ))
        })?;
    let execution_environment = resolved_worktree.execution_environment();
    if session.execution_environment != execution_environment {
        tracing::warn!(
            session_id = %session.id.0,
            stored = session.execution_environment.as_str(),
            resolved = execution_environment.as_str(),
            "session authenticate resolved a different execution_environment than persisted metadata"
        );
    }
    let install_target = host
        .effective_install_target_for_environment(workspace.id, execution_environment)
        .await
        .map_err(|error| {
            let message = format!("failed to load workspace execution settings: {error:#}");
            if ctx_settings_service::is_execution_policy_denial(&error) {
                SessionAuthError::Forbidden(message)
            } else {
                SessionAuthError::Internal(message)
            }
        })?;
    let adapter_cfg =
        ctx_provider_runtime::provider_launch::config::load_managed_agent_server_config_or_err(
            host.data_root(),
        )
        .await
        .map_err(|err| SessionAuthError::Internal(err.to_string()))?;
    let adapter = host
        .ensure_provider_adapter_for_target_with_cfg(
            &adapter_cfg,
            &session.provider_id,
            install_target,
        )
        .await;
    let probe_context = host
        .provider_auth_context_for_worktree_runtime(
            &resolved_worktree.worktree,
            &session.provider_id,
        )
        .await
        .map_err(SessionAuthError::BadRequest)?;
    let mut provider_env = probe_context.env;
    if let Some(provider_ref) = session.provider_session_ref.clone() {
        provider_env.insert("CTX_PROVIDER_SESSION_REF".to_string(), provider_ref);
    }
    provider_env.insert("CTX_SESSION_ID".to_string(), session.id.0.to_string());
    if let Ok(value) = std::env::var("CTX_MCP_COMMAND") {
        provider_env.insert("CTX_MCP_COMMAND".to_string(), value);
    }
    if let Ok(value) = std::env::var("CTX_MCP_DISABLED") {
        provider_env.insert("CTX_MCP_DISABLED".to_string(), value);
    }
    if session.provider_id == "codex" && !provider_env.contains_key("CODEX_HOME") {
        if let Ok(extra) =
            ctx_provider_accounts::codex_env_for_active_account(host.data_root()).await
        {
            for (key, value) in extra {
                provider_env.insert(key, value);
            }
        }
    }
    ctx_managed_installs::ensure_codex_cli_command_env_for_target(
        &mut provider_env,
        &adapter_cfg,
        &session.provider_id,
        Some(install_target),
    )
    .map_err(|error| {
        SessionAuthError::Internal(format!(
            "failed to resolve codex-cli runtime path: {error:#}"
        ))
    })?;
    ctx_mcp_command::configure_runtime_mcp_command(
        &session.provider_id,
        &mut provider_env,
        host.data_root(),
    )
    .map_err(|error| {
        SessionAuthError::Internal(format!("failed to prepare sandbox MCP runtime: {error:#}"))
    })?;

    Ok(PreparedSessionAuth {
        adapter,
        workdir: probe_context.cwd,
        provider_env,
    })
}
