use std::collections::HashMap;
use std::path::Path;

use ctx_core::env::DAEMON_AUTH_ENV_VARS;
use ctx_core::provider_ids::CODEX_PROVIDER_ID;
use ctx_core::provider_policy::CTX_CRP_LAUNCH_POLICY_ENV;
use ctx_harness_sources::{
    HarnessRouteBackend, HarnessRuntimeSourceMode, HarnessSourceKind, ResolvedHarnessSource,
};
use ctx_provider_accounts as provider_accounts;

use super::ProviderProbeHost;

pub(super) async fn provider_env_with_runtime_root<H>(
    state: &H,
    provider_id: &str,
    runtime_data_root: Option<&Path>,
    require_subscription_account_env: bool,
    include_daemon_auth: bool,
    disable_mcp: bool,
) -> Result<(ResolvedHarnessSource, HashMap<String, String>), String>
where
    H: ProviderProbeHost,
{
    let source = ctx_harness_sources::resolve_provider_source_for_probe_with_runtime_root(
        state.data_root(),
        provider_id,
        runtime_data_root,
    )
    .await
    .map_err(|e| state.redact_sensitive(&e.to_string()))?;
    let mut env = HashMap::new();
    env.insert("CTX_DAEMON_URL".to_string(), state.daemon_url().to_string());
    if include_daemon_auth {
        if let Some(token) = state.auth_token() {
            env.insert("CTX_AUTH_TOKEN".to_string(), token.clone());
        }
    }
    if disable_mcp {
        env.insert("CTX_MCP_DISABLED".to_string(), "1".to_string());
    }
    if source.runtime_source_mode().source_kind() == HarnessSourceKind::Subscription {
        if require_subscription_account_env && provider_id == CODEX_PROVIDER_ID {
            let has_auth = match runtime_data_root {
                Some(runtime_root) => {
                    provider_accounts::codex_has_active_auth_with_runtime_root(
                        state.data_root(),
                        runtime_root,
                    )
                    .await
                }
                None => provider_accounts::codex_has_active_auth(state.data_root()).await,
            }
            .map_err(|err| {
                state.redact_sensitive(&format!(
                    "probe subscription env preparation failed: {err:#}"
                ))
            })?;
            if !has_auth {
                return Err(format!(
                    "subscription account env is missing for provider '{provider_id}'; configure an active account or select an endpoint"
                ));
            }
        }
        let extra = match runtime_data_root {
            Some(runtime_root) => {
                provider_accounts::subscription_env_for_active_account_with_runtime_root(
                    state.data_root(),
                    runtime_root,
                    provider_id,
                )
                .await
            }
            None => {
                provider_accounts::subscription_env_for_active_account(
                    state.data_root(),
                    provider_id,
                )
                .await
            }
        }
        .map_err(|err| {
            state.redact_sensitive(&format!(
                "probe subscription env preparation failed: {err:#}"
            ))
        })?;
        if require_subscription_account_env
            && subscription_probe_requires_account_env(provider_id)
            && extra.is_empty()
        {
            return Err(format!(
                "subscription account env is missing for provider '{provider_id}'; configure an active account or select an endpoint"
            ));
        }
        for (key, value) in extra {
            env.insert(key, value);
        }
    }
    for (key, value) in source.env.iter() {
        insert_probe_source_env(&mut env, key, value);
    }
    strip_daemon_owned_probe_env(&mut env);
    Ok((source, env))
}

pub(super) fn insert_probe_source_env(env: &mut HashMap<String, String>, key: &str, value: &str) {
    if key == CTX_CRP_LAUNCH_POLICY_ENV {
        return;
    }
    env.insert(key.to_string(), value.to_string());
}

pub(super) fn strip_daemon_owned_probe_env(env: &mut HashMap<String, String>) {
    env.remove(CTX_CRP_LAUNCH_POLICY_ENV);
    for key in DAEMON_AUTH_ENV_VARS {
        env.remove(*key);
    }
}

fn subscription_probe_requires_account_env(provider_id: &str) -> bool {
    matches!(
        provider_id,
        "claude-crp" | "gemini" | "qwen" | "kimi" | "mistral" | "copilot" | "cursor" | "amp"
    )
}

pub(super) async fn finalize_workspace_probe_env<H>(
    state: &H,
    source: &ResolvedHarnessSource,
    provider_id: &str,
    env: &mut HashMap<String, String>,
) -> Result<(), String>
where
    H: ProviderProbeHost,
{
    if provider_id == CODEX_PROVIDER_ID
        && source.runtime_source_mode()
            == HarnessRuntimeSourceMode::Endpoint(HarnessRouteBackend::UserManaged)
    {
        if let Some(root) = env.get("CTX_DATA_ROOT").cloned() {
            provider_accounts::ensure_codex_endpoint_runtime_home_from_env(Path::new(&root), env)
                .await
                .map_err(|err| {
                    state.redact_sensitive(&format!(
                        "probe codex endpoint runtime-home preparation failed: {err:#}"
                    ))
                })?;
        }
    }

    if let Some(root) = env.get("CTX_DATA_ROOT").cloned() {
        provider_accounts::ensure_provider_runtime_home_env(Path::new(&root), provider_id, env)
            .await
            .map_err(|err| {
                state.redact_sensitive(&format!(
                    "probe provider runtime-home preparation failed: {err:#}"
                ))
            })?;
    }
    Ok(())
}
