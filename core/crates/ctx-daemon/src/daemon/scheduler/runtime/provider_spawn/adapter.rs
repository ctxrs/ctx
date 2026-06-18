use std::sync::Arc;

use anyhow::{anyhow, Result};
use ctx_managed_installs::AgentServerConfigFile;
use ctx_provider_install::install_state::InstallTarget;
use ctx_provider_runtime::ProviderRuntimeHost;
use ctx_providers::adapters::{ProviderAdapter, ProviderStatus};

use crate::daemon::scheduler::host::ProviderTurnLaunchHost;

pub(in crate::daemon::scheduler::runtime) struct PreparedProviderAdapter {
    pub(in crate::daemon::scheduler::runtime) adapter: Arc<dyn ProviderAdapter>,
    pub(in crate::daemon::scheduler::runtime) adapter_cfg: AgentServerConfigFile,
    pub(in crate::daemon::scheduler::runtime) install_target: InstallTarget,
}

pub(in crate::daemon::scheduler::runtime) async fn prepare_provider_adapter_for_turn(
    provider_launch: &ProviderTurnLaunchHost,
    runtime_provider_id: &str,
    is_linux_sandbox: bool,
) -> Result<PreparedProviderAdapter> {
    let install_target = provider_install_target_for_runtime(is_linux_sandbox);
    provider_launch.sync_plugin_provider_adapters().await;
    let adapter_cfg =
        ctx_provider_runtime::provider_launch::config::load_managed_agent_server_config_or_err(
            provider_launch.data_root(),
        )
        .await
        .map_err(|err| anyhow!(err.to_string()))?;
    let adapter = ctx_provider_runtime::provider_launch::resolver::ensure_provider_adapter_for_target_with_cfg(
        provider_launch,
        &adapter_cfg,
        runtime_provider_id,
        install_target,
    )
    .await;
    let install_target = effective_provider_install_target_for_status(
        install_target,
        provider_launch
            .provider_runtime()
            .provider_status(runtime_provider_id)
            .await
            .as_ref(),
    );

    Ok(PreparedProviderAdapter {
        adapter,
        adapter_cfg,
        install_target,
    })
}

fn provider_install_target_for_runtime(is_linux_sandbox: bool) -> InstallTarget {
    if is_linux_sandbox {
        InstallTarget::Container
    } else {
        InstallTarget::Host
    }
}

fn effective_provider_install_target_for_status(
    requested: InstallTarget,
    status: Option<&ProviderStatus>,
) -> InstallTarget {
    if requested != InstallTarget::Container {
        return requested;
    }
    let Some(status) = status else {
        return requested;
    };
    if status.details.get("plugin_provider").map(String::as_str) == Some("true")
        && status
            .details
            .get("plugin_provider_target")
            .map(String::as_str)
            == Some("host")
    {
        return InstallTarget::Host;
    }
    requested
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_providers::adapters::{ProviderHealth, ProviderUsability};

    #[test]
    fn linux_sandbox_runtime_uses_container_install_target() {
        assert_eq!(
            provider_install_target_for_runtime(true),
            InstallTarget::Container
        );
    }

    #[test]
    fn host_runtime_uses_host_install_target() {
        assert_eq!(
            provider_install_target_for_runtime(false),
            InstallTarget::Host
        );
    }

    #[test]
    fn host_only_plugin_provider_uses_host_install_target_even_in_linux_sandbox() {
        let mut status = provider_status("plugin.py");
        status
            .details
            .insert("plugin_provider".to_string(), "true".to_string());
        status
            .details
            .insert("plugin_provider_target".to_string(), "host".to_string());

        assert_eq!(
            effective_provider_install_target_for_status(InstallTarget::Container, Some(&status)),
            InstallTarget::Host
        );
    }

    #[test]
    fn non_plugin_provider_keeps_requested_container_install_target() {
        let status = provider_status("codex");

        assert_eq!(
            effective_provider_install_target_for_status(InstallTarget::Container, Some(&status)),
            InstallTarget::Container
        );
    }

    fn provider_status(provider_id: &str) -> ProviderStatus {
        ProviderStatus {
            provider_id: provider_id.to_string(),
            installed: true,
            detected_path: None,
            version: None,
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: Default::default(),
            usability: ProviderUsability::default(),
        }
    }
}
