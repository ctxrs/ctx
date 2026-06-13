use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use ctx_managed_installs as installer;
use ctx_provider_install::install_state::InstallTarget;
use ctx_providers::adapters::{
    ProviderAdapter, ProviderHealth, ProviderStatus, RunHandle, TurnInput,
};
use ctx_providers::crp::Tier1CrpAdapter;
use ctx_providers::fake::FakeProviderAdapter;

struct StaticStatusAdapter {
    status: ProviderStatus,
}

#[async_trait]
impl ProviderAdapter for StaticStatusAdapter {
    async fn inspect(&self) -> Result<ProviderStatus> {
        Ok(self.status.clone())
    }

    async fn run(
        &self,
        _input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _event_sink: tokio::sync::mpsc::Sender<ctx_providers::events::NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<RunHandle> {
        let msg = self
            .status
            .diagnostics
            .first()
            .cloned()
            .unwrap_or_else(|| "provider is unavailable".to_string());
        anyhow::bail!("{msg}");
    }

    async fn cancel(&self, _handle: &mut RunHandle) -> Result<()> {
        Ok(())
    }
}

fn static_status_adapter(
    provider_id: &str,
    installed: bool,
    health: ProviderHealth,
    error_code: &str,
    message: String,
) -> Arc<dyn ProviderAdapter> {
    let mut details = HashMap::new();
    details.insert("error_code".to_string(), error_code.to_string());
    Arc::new(StaticStatusAdapter {
        status: ProviderStatus {
            provider_id: provider_id.to_string(),
            installed,
            detected_path: None,
            version: None,
            capabilities: None,
            health,
            diagnostics: vec![message],
            details,
            usability: ctx_providers::adapters::ProviderUsability::default(),
        },
    })
}

pub fn runtime_command_as_agent_command_for_target(
    cfg: &installer::AgentServerConfigFile,
    provider_id: &str,
    requested_target: Option<InstallTarget>,
) -> Result<Option<installer::AgentServerCommand>> {
    let Some(resolved) =
        installer::resolve_runtime_provider_command_for_target(cfg, provider_id, requested_target)?
    else {
        return Ok(None);
    };
    Ok(Some(installer::AgentServerCommand {
        command: resolved.command_abs_path,
        args: resolved.args,
        dependencies: resolved.dependencies,
        managed: None,
    }))
}

pub fn runtime_command_as_agent_command(
    cfg: &installer::AgentServerConfigFile,
    provider_id: &str,
) -> Result<Option<installer::AgentServerCommand>> {
    runtime_command_as_agent_command_for_target(cfg, provider_id, None)
}

pub fn acp_status_adapter_bridge_missing(
    provider_id: &str,
    msg: String,
) -> Arc<dyn ProviderAdapter> {
    static_status_adapter(
        provider_id,
        false,
        ProviderHealth::Error,
        "acp_bridge_missing",
        msg,
    )
}

pub fn acp_status_adapter_bridge_invalid(
    provider_id: &str,
    msg: String,
) -> Arc<dyn ProviderAdapter> {
    static_status_adapter(
        provider_id,
        false,
        ProviderHealth::Error,
        "acp_bridge_invalid",
        msg,
    )
}

pub fn acp_status_adapter_acp_command_invalid(
    provider_id: &str,
    msg: String,
) -> Arc<dyn ProviderAdapter> {
    static_status_adapter(
        provider_id,
        true,
        ProviderHealth::Error,
        "acp_command_invalid",
        msg,
    )
}

pub fn runtime_command_missing_adapter(provider_id: &str) -> Arc<dyn ProviderAdapter> {
    static_status_adapter(
        provider_id,
        false,
        ProviderHealth::Missing,
        "runtime_command_missing",
        format!("runtime command is not configured for provider '{provider_id}'"),
    )
}

pub fn runtime_command_invalid_adapter(provider_id: &str, err: String) -> Arc<dyn ProviderAdapter> {
    static_status_adapter(
        provider_id,
        false,
        ProviderHealth::Error,
        "runtime_command_invalid",
        format!("invalid runtime command for provider '{provider_id}': {err}"),
    )
}

pub fn is_acp_provider_id(provider_id: &str) -> bool {
    crate::provider_launch::resolver::is_acp_provider_id(provider_id)
}

pub fn acp_bridge_command(
    bridge_cmd: &installer::AgentServerCommand,
    acp_cmd: installer::AgentServerCommand,
) -> installer::AgentServerCommand {
    crate::provider_launch::resolver::acp_bridge_command(bridge_cmd, acp_cmd)
}

pub fn acp_bridge_adapter(
    id: &str,
    bridge_cmd: &installer::AgentServerCommand,
    acp_cmd: installer::AgentServerCommand,
) -> Arc<dyn ProviderAdapter> {
    crate::provider_launch::resolver::acp_bridge_adapter(id, bridge_cmd, acp_cmd)
}

#[cfg(test)]
pub fn runtime_probe_command_as_agent_command_for_target(
    data_root: &Path,
    cfg: &installer::AgentServerConfigFile,
    provider_id: &str,
    requested_target: Option<InstallTarget>,
) -> Result<Option<installer::AgentServerCommand>> {
    crate::provider_launch::resolver::runtime_probe_command_as_agent_command_for_target(
        data_root,
        cfg,
        provider_id,
        requested_target,
    )
}

#[cfg(test)]
pub fn runtime_probe_command_as_agent_command(
    data_root: &Path,
    cfg: &installer::AgentServerConfigFile,
    provider_id: &str,
) -> Result<Option<installer::AgentServerCommand>> {
    runtime_probe_command_as_agent_command_for_target(data_root, cfg, provider_id, None)
}

pub fn target_adapter_cache_key(provider_id: &str, target: InstallTarget) -> Option<String> {
    (!matches!(target, InstallTarget::Host)).then(|| format!("{provider_id}@{}", target.as_str()))
}

pub fn build_startup_provider_adapters(
    data_root: &Path,
    agent_cfg: &installer::AgentServerConfigFile,
) -> HashMap<String, Arc<dyn ProviderAdapter>> {
    let mut bridge_runtime_error: Option<String> = None;
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    let bridge_cmd = match runtime_command_as_agent_command(agent_cfg, "acp-crp-bridge") {
        Ok(cmd) => cmd,
        Err(err) => {
            let message = format!("invalid runtime command for acp-crp-bridge: {err}");
            tracing::warn!("{message}");
            bridge_runtime_error = Some(message);
            None
        }
    };

    for provider_id in ["codex", "claude-crp"] {
        let adapter: Arc<dyn ProviderAdapter> =
            match runtime_command_as_agent_command(agent_cfg, provider_id) {
                Ok(Some(cmd)) => Arc::new(Tier1CrpAdapter::from_provider_runtime(
                    provider_id,
                    cmd.command.clone(),
                    cmd.args.clone(),
                )),
                Ok(None) => runtime_command_missing_adapter(provider_id),
                Err(err) => runtime_command_invalid_adapter(provider_id, err.to_string()),
            };
        providers.insert(provider_id.to_string(), adapter);
    }

    for provider_id in [
        "gemini",
        "qwen",
        "cursor",
        "pi",
        "opencode",
        "mistral",
        "goose",
        "kimi",
        "auggie",
        "amp",
        "droid",
        "copilot",
        "cline",
        "openhands",
    ] {
        let bridge_missing_message = bridge_runtime_error
            .clone()
            .unwrap_or_else(|| "ACP bridge runtime is not configured".to_string());
        let adapter = match bridge_cmd.as_ref() {
            None => {
                if let Some(message) = bridge_runtime_error.clone() {
                    acp_status_adapter_bridge_invalid(provider_id, message)
                } else {
                    acp_status_adapter_bridge_missing(provider_id, bridge_missing_message)
                }
            }
            Some(bridge) => match runtime_command_as_agent_command(agent_cfg, provider_id) {
                Ok(Some(cmd)) => {
                    match crate::provider_launch::resolver::normalize_acp_provider_command(
                        data_root,
                        provider_id,
                        cmd,
                    ) {
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
            },
        };
        providers.insert(provider_id.to_string(), adapter);
    }

    if std::env::var("CTX_SHOW_FAKE_PROVIDER")
        .ok()
        .as_deref()
        .and_then(ctx_core::boolish::parse_boolish)
        .unwrap_or(false)
    {
        providers.insert("fake".into(), Arc::new(FakeProviderAdapter::new()));
    }

    providers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn startup_adapters_report_missing_runtime_commands_without_http_registry() {
        let temp = tempfile::tempdir().expect("tempdir");
        let adapters = build_startup_provider_adapters(
            temp.path(),
            &installer::AgentServerConfigFile::default(),
        );

        assert!(adapters.contains_key("codex"));
        assert!(adapters.contains_key("gemini"));

        let codex = adapters
            .get("codex")
            .expect("codex adapter")
            .inspect()
            .await
            .expect("codex status");
        assert_eq!(codex.health, ProviderHealth::Missing);
        assert_eq!(
            codex.details.get("error_code").map(String::as_str),
            Some("runtime_command_missing")
        );

        let gemini = adapters
            .get("gemini")
            .expect("gemini adapter")
            .inspect()
            .await
            .expect("gemini status");
        assert_eq!(gemini.health, ProviderHealth::Error);
        assert_eq!(
            gemini.details.get("error_code").map(String::as_str),
            Some("acp_bridge_missing")
        );
    }
}
