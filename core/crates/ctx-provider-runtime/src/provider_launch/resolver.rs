use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fs, path::Path as StdPath};

use anyhow::{Context, Result};
use ctx_providers::adapters::{
    ProviderAdapter, ProviderHealth, ProviderProcessInfo, ProviderRestartMode,
    ProviderSessionSweepConfig, ProviderSessionSweepStats, ProviderStatus, RunHandle, TurnInput,
};
use ctx_providers::crp::Tier1CrpAdapter;
use ctx_providers::fake::FakeProviderAdapter;

use crate::ProviderRuntimeHost;
use ctx_managed_installs as installer;
use ctx_provider_install::install_state::InstallTarget;

#[path = "resolver/bridge.rs"]
mod bridge;
#[path = "resolver/normalize.rs"]
mod normalize;
#[cfg(test)]
mod tests;

pub use bridge::{
    acp_bridge_adapter, acp_bridge_command, is_acp_provider_id,
    runtime_command_as_agent_command_for_target,
};
use bridge::{
    acp_status_adapter_acp_command_invalid, acp_status_adapter_bridge_invalid,
    acp_status_adapter_bridge_missing, runtime_command_invalid_adapter,
    runtime_command_missing_adapter,
};
#[cfg(test)]
use bridge::{openhands_runtime_contract_for_command, OpenHandsRuntimeContract};
pub use normalize::{
    normalize_acp_provider_command, resolve_explicit_gemini_cli_paths, ExplicitGeminiCliPaths,
};

pub fn runtime_probe_command_as_agent_command_for_target(
    data_root: &Path,
    cfg: &installer::AgentServerConfigFile,
    provider_id: &str,
    requested_target: Option<InstallTarget>,
) -> Result<Option<installer::AgentServerCommand>> {
    let Some(runtime_cmd) =
        runtime_command_as_agent_command_for_target(cfg, provider_id, requested_target)?
    else {
        return Ok(None);
    };
    if !is_acp_provider_id(provider_id) {
        return Ok(Some(runtime_cmd));
    }

    let bridge_cmd =
        runtime_command_as_agent_command_for_target(cfg, "acp-crp-bridge", requested_target)?
            .ok_or_else(|| {
                anyhow::anyhow!("runtime command is not configured for provider 'acp-crp-bridge'")
            })?;
    let normalized = normalize_acp_provider_command(data_root, provider_id, runtime_cmd)?;
    let mut bridged = acp_bridge_command(&bridge_cmd, normalized.clone());
    let mut dependencies = bridge_cmd.dependencies.clone();
    for dependency in &normalized.dependencies {
        if !dependencies.contains(dependency) {
            dependencies.push(dependency.clone());
        }
    }
    bridged.dependencies = dependencies;
    bridged.managed = bridge_cmd.managed.clone();
    Ok(Some(bridged))
}

#[cfg(test)]
pub(crate) fn runtime_probe_command_as_agent_command(
    data_root: &Path,
    cfg: &installer::AgentServerConfigFile,
    provider_id: &str,
) -> Result<Option<installer::AgentServerCommand>> {
    runtime_probe_command_as_agent_command_for_target(data_root, cfg, provider_id, None)
}

pub fn target_adapter_cache_key(provider_id: &str, target: InstallTarget) -> Option<String> {
    (!matches!(target, InstallTarget::Host)).then(|| format!("{provider_id}@{}", target.as_str()))
}

fn build_provider_adapter_for_target(
    data_root: &Path,
    cfg: &installer::AgentServerConfigFile,
    provider_id: &str,
    target: InstallTarget,
) -> Arc<dyn ProviderAdapter> {
    if matches!(provider_id, "codex" | "claude-crp") {
        return match runtime_command_as_agent_command_for_target(cfg, provider_id, Some(target)) {
            Ok(Some(cmd)) => Arc::new(Tier1CrpAdapter::from_provider_runtime(
                provider_id,
                cmd.command.clone(),
                cmd.args.clone(),
            )),
            Ok(None) => runtime_command_missing_adapter(provider_id),
            Err(err) => runtime_command_invalid_adapter(provider_id, err.to_string()),
        };
    }

    if is_acp_provider_id(provider_id) {
        let bridge_cmd = match runtime_command_as_agent_command_for_target(
            cfg,
            "acp-crp-bridge",
            Some(target),
        ) {
            Ok(cmd) => cmd,
            Err(err) => {
                return acp_status_adapter_bridge_invalid(
                    provider_id,
                    format!("invalid runtime command for acp-crp-bridge: {err}"),
                );
            }
        };
        let bridge_missing_message = "ACP bridge runtime is not configured".to_string();
        return match bridge_cmd.as_ref() {
            None => acp_status_adapter_bridge_missing(provider_id, bridge_missing_message),
            Some(bridge) => {
                match runtime_command_as_agent_command_for_target(cfg, provider_id, Some(target)) {
                    Ok(Some(cmd)) => {
                        match normalize_acp_provider_command(data_root, provider_id, cmd) {
                            Ok(cmd) => acp_bridge_adapter(provider_id, bridge, cmd),
                            Err(err) => acp_status_adapter_acp_command_invalid(
                                provider_id,
                                format!("invalid ACP command for provider '{provider_id}': {err}"),
                            ),
                        }
                    }
                    Ok(None) => acp_status_adapter_acp_command_invalid(
                        provider_id,
                        format!("ACP command is not configured for provider '{provider_id}'"),
                    ),
                    Err(err) => acp_status_adapter_acp_command_invalid(
                        provider_id,
                        format!("invalid ACP command for provider '{provider_id}': {err}"),
                    ),
                }
            }
        };
    }

    if provider_id == "fake" {
        return Arc::new(FakeProviderAdapter::new());
    }

    runtime_command_missing_adapter(provider_id)
}

pub async fn ensure_provider_adapter_for_target_with_cfg(
    state: &impl ProviderRuntimeHost,
    cfg: &installer::AgentServerConfigFile,
    provider_id: &str,
    target: InstallTarget,
) -> Arc<dyn ProviderAdapter> {
    if let Some(cache_key) = target_adapter_cache_key(provider_id, target) {
        if let Some(adapter) = state
            .provider_runtime()
            .target_provider_adapter(&cache_key)
            .await
        {
            return adapter;
        }
        let adapter =
            build_provider_adapter_for_target(state.data_root(), cfg, provider_id, target);
        state
            .provider_runtime()
            .upsert_target_provider_adapter(cache_key, adapter.clone())
            .await;
        return adapter;
    }

    if let Some(adapter) = state.provider_runtime().provider_adapter(provider_id).await {
        return adapter;
    }
    let adapter = build_provider_adapter_for_target(state.data_root(), cfg, provider_id, target);
    state
        .provider_runtime()
        .upsert_provider_adapter(provider_id.to_string(), adapter.clone())
        .await;
    adapter
}

pub async fn ensure_provider_adapter_for_target(
    state: &impl ProviderRuntimeHost,
    provider_id: &str,
    target: InstallTarget,
) -> Result<Arc<dyn ProviderAdapter>> {
    let cfg = super::config::load_managed_agent_server_config_or_err(state.data_root())
        .await
        .context("loading agent server config for provider adapter resolution")?;
    Ok(ensure_provider_adapter_for_target_with_cfg(state, &cfg, provider_id, target).await)
}
