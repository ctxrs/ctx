use ctx_core::models::Workspace;
use ctx_core::redaction;
use ctx_provider_install::install_state::InstallTarget;
use ctx_providers::crp::{probe_crp_models, CrpModelsProbe};

use crate::provider_launch::probe::{self, ProviderProbeHost};
use crate::provider_launch::probe_error::classify_probe_error;
use crate::provider_launch::runtime_probe::{
    prepare_provider_runtime_probe_launch, PreparedProviderRuntimeProbe,
};

pub enum PreparedProviderRuntimeProbeError {
    Verify(String),
}

pub struct ProviderRuntimeProbeStatus {
    pub probe_ok: bool,
    pub auth_required: bool,
    pub probe_error: Option<String>,
}

pub struct ProviderAuthVerificationRuntimeProbe {
    pub selected_endpoint_id: Option<String>,
    pub probe_error: Option<String>,
}

pub async fn prepare_provider_runtime_probe<H>(
    state: &H,
    workspace: &Workspace,
    provider_id: &str,
    install_target: InstallTarget,
    selected_endpoint_id: Option<String>,
) -> Result<PreparedProviderRuntimeProbe, PreparedProviderRuntimeProbeError>
where
    H: ProviderProbeHost,
{
    let (cfg, config_error) =
        crate::provider_launch::config::load_managed_agent_server_config_with_error(
            state.data_root(),
        )
        .await;
    if let Some(config_error) = config_error {
        return Err(PreparedProviderRuntimeProbeError::Verify(config_error));
    }
    let probe_context =
        probe::provider_probe_context_for_workspace_runtime(state, workspace, provider_id)
            .await
            .map_err(PreparedProviderRuntimeProbeError::Verify)?;
    prepare_provider_runtime_probe_launch(
        state.data_root(),
        &cfg,
        provider_id,
        install_target,
        probe_context,
        selected_endpoint_id,
    )
    .map_err(|error| PreparedProviderRuntimeProbeError::Verify(error.into_message()))
}

pub async fn probe_provider_auth_verification_runtime<H>(
    state: &H,
    workspace: &Workspace,
    provider_id: &str,
    install_target: InstallTarget,
    selected_endpoint_id: Option<String>,
) -> Result<ProviderAuthVerificationRuntimeProbe, PreparedProviderRuntimeProbeError>
where
    H: ProviderProbeHost,
{
    let requested_endpoint_id = selected_endpoint_id.clone();
    match prepare_provider_runtime_probe(
        state,
        workspace,
        provider_id,
        install_target,
        selected_endpoint_id,
    )
    .await
    {
        Ok(prepared) => {
            let probe = probe_crp_models(
                provider_id,
                prepared.command,
                prepared.args,
                prepared.cwd,
                prepared.env,
            )
            .await;
            let probe_error = probe
                .err()
                .map(|error| redaction::redact_sensitive(&error.to_string()));
            Ok(ProviderAuthVerificationRuntimeProbe {
                selected_endpoint_id: prepared.selected_endpoint_id,
                probe_error,
            })
        }
        Err(PreparedProviderRuntimeProbeError::Verify(err)) => {
            Ok(ProviderAuthVerificationRuntimeProbe {
                selected_endpoint_id: requested_endpoint_id,
                probe_error: Some(redaction::redact_sensitive(&err)),
            })
        }
    }
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
    probe::provider_has_active_auth_for_workspace_runtime(
        state,
        workspace,
        provider_id,
        source_config,
    )
    .await
}

pub async fn probe_provider_options_env<H>(
    state: &H,
    workspace: &Workspace,
    provider_id: &str,
) -> ProviderRuntimeProbeStatus
where
    H: ProviderProbeHost,
{
    match probe::provider_probe_env_for_workspace_runtime(state, workspace, provider_id).await {
        Ok(_) => ProviderRuntimeProbeStatus {
            probe_ok: true,
            auth_required: false,
            probe_error: None,
        },
        Err(err) => classified_probe_status(redaction::redact_sensitive(&err)),
    }
}

pub async fn probe_selected_endpoint_runtime_launch<H>(
    state: &H,
    workspace: &Workspace,
    provider_id: &str,
    install_target: InstallTarget,
    endpoint_id: String,
) -> Result<ProviderRuntimeProbeStatus, PreparedProviderRuntimeProbeError>
where
    H: ProviderProbeHost,
{
    match prepare_provider_runtime_probe(
        state,
        workspace,
        provider_id,
        install_target,
        Some(endpoint_id),
    )
    .await
    {
        Ok(prepared) => {
            match probe_crp_models(
                provider_id,
                prepared.command,
                prepared.args,
                prepared.cwd,
                prepared.env,
            )
            .await
            {
                Ok(_) => Ok(ProviderRuntimeProbeStatus {
                    probe_ok: true,
                    auth_required: false,
                    probe_error: None,
                }),
                Err(err) => Ok(classified_probe_status(redaction::redact_sensitive(
                    &err.to_string(),
                ))),
            }
        }
        Err(PreparedProviderRuntimeProbeError::Verify(err)) => {
            Ok(classified_probe_status(redaction::redact_sensitive(&err)))
        }
    }
}

pub async fn probe_runtime_models_for_provider_options<H>(
    state: &H,
    workspace: &Workspace,
    provider_id: &str,
    install_target: InstallTarget,
) -> anyhow::Result<CrpModelsProbe>
where
    H: ProviderProbeHost,
{
    match prepare_provider_runtime_probe(state, workspace, provider_id, install_target, None).await
    {
        Ok(prepared) => {
            probe_crp_models(
                provider_id,
                prepared.command,
                prepared.args,
                prepared.cwd,
                prepared.env,
            )
            .await
        }
        Err(PreparedProviderRuntimeProbeError::Verify(err)) => Err(anyhow::anyhow!(err)),
    }
}

fn classified_probe_status(probe_error: String) -> ProviderRuntimeProbeStatus {
    let (_, auth_required, _) = classify_probe_error(&probe_error);
    ProviderRuntimeProbeStatus {
        probe_ok: false,
        auth_required: auth_required.unwrap_or(false),
        probe_error: Some(probe_error),
    }
}
