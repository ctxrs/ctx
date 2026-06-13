use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use ctx_core::provider_ids::CODEX_PROVIDER_ID;
use ctx_harness_sources::HarnessRuntimeSourceMode;
use ctx_provider_accounts as provider_accounts;
use ctx_provider_install::install_state::InstallTarget;

use crate::daemon::scheduler::host::ProviderTurnLaunchHost;
use ctx_managed_installs as installer;

#[path = "credentials/codex.rs"]
mod codex;
#[path = "credentials/mode.rs"]
mod mode;
#[path = "credentials/subscription.rs"]
mod subscription;

use codex::{prepare_codex_runtime_credentials, CodexRuntimeCredentialRequest};
use mode::{provider_runtime_credential_mode, ProviderRuntimeCredentialMode};
use subscription::{
    prepare_subscription_runtime_credentials, SubscriptionRuntimeCredentialRequest,
};

pub(in crate::daemon::scheduler::runtime) struct ProviderRuntimeEnvironmentRequest<'a> {
    pub(in crate::daemon::scheduler::runtime) provider_launch: &'a ProviderTurnLaunchHost,
    pub(in crate::daemon::scheduler::runtime) provider_env: &'a mut HashMap<String, String>,
    pub(in crate::daemon::scheduler::runtime) runtime_provider_id: &'a str,
    pub(in crate::daemon::scheduler::runtime) runtime_plan:
        &'a ctx_harness_runtime::HarnessExecutionPlan,
    pub(in crate::daemon::scheduler::runtime) is_linux_sandbox: bool,
    pub(in crate::daemon::scheduler::runtime) runtime_source_mode: HarnessRuntimeSourceMode,
    pub(in crate::daemon::scheduler::runtime) adapter_cfg: &'a installer::AgentServerConfigFile,
    pub(in crate::daemon::scheduler::runtime) install_target: InstallTarget,
}

pub(in crate::daemon::scheduler::runtime) async fn prepare_provider_runtime_environment(
    request: ProviderRuntimeEnvironmentRequest<'_>,
) -> Result<()> {
    let ProviderRuntimeEnvironmentRequest {
        provider_launch,
        provider_env,
        runtime_provider_id,
        runtime_plan,
        is_linux_sandbox,
        runtime_source_mode,
        adapter_cfg,
        install_target,
    } = request;
    let credential_mode = provider_runtime_credential_mode(runtime_source_mode);
    if runtime_provider_id == CODEX_PROVIDER_ID {
        prepare_codex_runtime_credentials(CodexRuntimeCredentialRequest {
            provider_launch,
            provider_env,
            runtime_provider_id,
            runtime_plan,
            is_linux_sandbox,
            credential_mode,
        })
        .await?;
    } else if credential_mode == ProviderRuntimeCredentialMode::Subscription {
        prepare_subscription_runtime_credentials(SubscriptionRuntimeCredentialRequest {
            provider_launch,
            provider_env,
            runtime_provider_id,
            runtime_plan,
            is_linux_sandbox,
        })
        .await?;
    }

    if is_linux_sandbox {
        if let Some(root) = runtime_plan.env_overrides.get("CTX_DATA_ROOT") {
            provider_accounts::ensure_provider_runtime_home_env(
                Path::new(root),
                runtime_provider_id,
                provider_env,
            )
            .await?;
        }
    }

    installer::prepend_runtime_bin_dirs_to_provider_path_for_target(
        provider_env,
        adapter_cfg,
        runtime_provider_id,
        provider_launch.data_root(),
        Some(install_target),
    );
    installer::ensure_codex_cli_command_env_for_target(
        provider_env,
        adapter_cfg,
        runtime_provider_id,
        Some(install_target),
    )?;

    Ok(())
}
