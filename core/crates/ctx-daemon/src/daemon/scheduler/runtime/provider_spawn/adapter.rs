use std::sync::Arc;

use anyhow::{anyhow, Result};
use ctx_managed_installs::AgentServerConfigFile;
use ctx_provider_install::install_state::InstallTarget;
use ctx_providers::adapters::ProviderAdapter;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
