use std::collections::HashMap;

use anyhow::Result;
use ctx_core::models::Session;
use ctx_settings_model::ProviderControlMode;

use crate::daemon::scheduler::host::ProviderTurnLaunchHost;

use super::super::provider_env::{build_base_provider_env, BaseProviderEnvRequest};

pub(super) struct ProviderSetupBaseEnv {
    pub(super) provider_env: HashMap<String, String>,
    pub(super) provider_control_mode: ProviderControlMode,
}

pub(super) async fn load_provider_setup_base_env(
    provider_launch: &ProviderTurnLaunchHost,
    session: &Session,
    full_model_id: &str,
) -> Result<ProviderSetupBaseEnv> {
    let settings = ctx_settings_service::load_settings(provider_launch.global_store()).await?;
    let provider_control_mode = settings
        .sandboxing
        .as_ref()
        .map(|s| s.provider_control_mode.clone())
        .unwrap_or_default();
    let provider_env = build_base_provider_env(BaseProviderEnvRequest {
        daemon_url: provider_launch.daemon_url(),
        data_root: provider_launch.data_root(),
        session,
        full_model_id,
        provider_control_mode: &provider_control_mode,
    });
    Ok(ProviderSetupBaseEnv {
        provider_env,
        provider_control_mode,
    })
}
