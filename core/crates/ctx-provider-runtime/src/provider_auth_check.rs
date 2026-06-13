use ctx_core::ids::WorkspaceId;
use ctx_core::models::Workspace;
use ctx_core::redaction::{redact_json_value, redact_sensitive};
use ctx_harness_sources::{HarnessEndpointRecord, HarnessEndpointVerificationStatus};
use ctx_provider_install::install_state::InstallTarget;
use serde::Serialize;
use tokio::sync::mpsc;

use crate::provider_harness_config::{
    mark_provider_endpoint_verification, refresh_provider_endpoint_model_catalog,
};
use crate::provider_launch::models::{
    endpoint_catalog_runtime_probe_failure, endpoint_catalog_verify_outcome,
};
use crate::provider_launch::options::endpoint_supports_model_catalog_verify;
use crate::provider_launch::probe::{self, ProviderProbeHost};
use crate::provider_launch::probe_error::classify_probe_error;
use crate::provider_launch::{config_snapshot, resolver};
use crate::provider_options::cache::store_provider_verify_cache_value;
use crate::provider_runtime_probe_service as runtime_probe_service;
use crate::provider_usability::{provider_status_is_usable, provider_status_unusable_reason};
use crate::ProviderRuntimeHost;

#[derive(Clone, Debug, Serialize)]
pub struct ProviderAuthCheckSnapshot {
    pub provider_id: String,
    pub workspace_id: String,
    pub status: String,
    pub auth_required: Option<bool>,
    pub checked_at: Option<String>,
    pub message: Option<String>,
}

pub struct ProviderVerifyOutcome {
    checked_at: String,
    status: String,
    auth_required: Option<bool>,
    message: Option<String>,
    endpoint_status: HarnessEndpointVerificationStatus,
    selected_endpoint_id: Option<String>,
    endpoint_catalog_result: bool,
}

impl ProviderVerifyOutcome {
    pub fn new(checked_at: String, selected_endpoint_id: Option<String>) -> Self {
        Self {
            checked_at,
            status: "ok".to_string(),
            auth_required: Some(false),
            message: None,
            endpoint_status: HarnessEndpointVerificationStatus::Valid,
            selected_endpoint_id,
            endpoint_catalog_result: false,
        }
    }

    pub fn is_ok(&self) -> bool {
        self.status == "ok"
    }

    pub fn selected_endpoint_id(&self) -> Option<&str> {
        self.selected_endpoint_id.as_deref()
    }

    pub fn endpoint_status(&self) -> HarnessEndpointVerificationStatus {
        self.endpoint_status
    }

    pub fn message(&self) -> Option<&String> {
        self.message.as_ref()
    }

    pub fn has_endpoint_catalog_result(&self) -> bool {
        self.endpoint_catalog_result
    }

    pub fn set_selected_endpoint_id(&mut self, selected_endpoint_id: Option<String>) {
        self.selected_endpoint_id = selected_endpoint_id;
    }

    pub fn apply_unusable_provider(&mut self, message: String) {
        self.status = "error".to_string();
        self.auth_required = Some(false);
        self.message = Some(message);
        self.endpoint_status = HarnessEndpointVerificationStatus::Error;
    }

    pub fn apply_endpoint_catalog_refresh(&mut self, refreshed_endpoint: HarnessEndpointRecord) {
        self.endpoint_catalog_result = true;
        self.selected_endpoint_id = Some(refreshed_endpoint.id.clone());
        let (status, auth_required, message, endpoint_status) =
            endpoint_catalog_verify_outcome(&refreshed_endpoint);
        self.status = status;
        self.auth_required = auth_required;
        self.message = message;
        self.endpoint_status = endpoint_status;
    }

    pub fn apply_classified_probe_error(&mut self, message: String) {
        let (status, auth_required, endpoint_status) = classify_probe_error(&message);
        self.status = status.to_string();
        self.auth_required = auth_required;
        self.message = Some(message);
        self.endpoint_status = endpoint_status;
    }

    pub fn apply_endpoint_catalog_runtime_probe_failure(&mut self, message: String) {
        let (status, auth_required, message, endpoint_status) =
            endpoint_catalog_runtime_probe_failure(message, self.endpoint_status);
        self.status = status;
        self.auth_required = auth_required;
        self.message = message;
        self.endpoint_status = endpoint_status;
    }

    pub fn into_snapshot(self, provider_id: &str, ws_id: WorkspaceId) -> ProviderAuthCheckSnapshot {
        ProviderAuthCheckSnapshot {
            provider_id: provider_id.to_string(),
            workspace_id: ws_id.0.to_string(),
            status: self.status,
            auth_required: self.auth_required,
            checked_at: Some(self.checked_at),
            message: self.message,
        }
    }
}

pub fn config_error_snapshot(
    provider_id: &str,
    ws_id: WorkspaceId,
    checked_at: &str,
    message: String,
) -> ProviderAuthCheckSnapshot {
    ProviderAuthCheckSnapshot {
        provider_id: provider_id.to_string(),
        workspace_id: ws_id.0.to_string(),
        status: "error".to_string(),
        auth_required: Some(false),
        checked_at: Some(checked_at.to_string()),
        message: Some(message),
    }
}

pub enum ProviderWorkspaceAuthenticationError {
    Verify(String),
}

pub struct ProviderWorkspaceAuthentication {
    pub install_target: InstallTarget,
    pub checked_at: String,
    pub error_message: Option<String>,
}

pub enum ProviderAuthCheckServiceError {
    ProviderLaunchConfig(config_snapshot::ProviderLaunchConfigError),
    Verify(String),
}

pub async fn authenticate_provider_for_workspace_runtime<H>(
    state: &H,
    workspace: &Workspace,
    workspace_id: WorkspaceId,
    provider_id: &str,
    install_target: InstallTarget,
    method_id: Option<String>,
) -> Result<ProviderAuthCheckSnapshot, ProviderWorkspaceAuthenticationError>
where
    H: ProviderProbeHost + ProviderRuntimeHost,
{
    let auth = authenticate_provider_session_for_workspace_runtime(
        state,
        workspace,
        provider_id,
        install_target,
        method_id,
    )
    .await?;

    let snapshot = match auth.error_message {
        None => ProviderAuthCheckSnapshot {
            provider_id: provider_id.to_string(),
            workspace_id: workspace_id.0.to_string(),
            status: "ok".to_string(),
            auth_required: Some(false),
            checked_at: Some(auth.checked_at),
            message: None,
        },
        Some(message) => {
            let (status, auth_required, _) = classify_probe_error(&message);
            ProviderAuthCheckSnapshot {
                provider_id: provider_id.to_string(),
                workspace_id: workspace_id.0.to_string(),
                status: status.to_string(),
                auth_required,
                checked_at: Some(auth.checked_at),
                message: Some(message),
            }
        }
    };
    store_provider_auth_check_cache(
        state.provider_runtime(),
        workspace_id,
        auth.install_target,
        provider_id,
        &snapshot,
    )
    .await;
    Ok(snapshot)
}

async fn authenticate_provider_session_for_workspace_runtime<H>(
    state: &H,
    workspace: &Workspace,
    provider_id: &str,
    install_target: InstallTarget,
    method_id: Option<String>,
) -> Result<ProviderWorkspaceAuthentication, ProviderWorkspaceAuthenticationError>
where
    H: ProviderProbeHost + ProviderRuntimeHost,
{
    let probe_context =
        probe::provider_auth_context_for_workspace_runtime(state, workspace, provider_id)
            .await
            .map_err(ProviderWorkspaceAuthenticationError::Verify)?;
    if probe_context.source.source_kind == ctx_harness_sources::HarnessSourceKind::Endpoint {
        return Err(ProviderWorkspaceAuthenticationError::Verify(
            "selected source is endpoint; update endpoint key/config directly instead of interactive authenticate"
                .to_string(),
        ));
    }

    let (event_tx, mut event_rx) = mpsc::channel(32);
    tokio::spawn(async move { while event_rx.recv().await.is_some() {} });
    let checked_at = chrono::Utc::now().to_rfc3339();
    let result = match resolver::ensure_provider_adapter_for_target(
        state,
        provider_id,
        install_target,
    )
    .await
    {
        Ok(adapter) => {
            adapter
                .authenticate_session(
                    format!("auth-{}", uuid::Uuid::new_v4()),
                    probe_context.cwd,
                    probe_context.env,
                    method_id,
                    event_tx,
                    ctx_providers::adapters::ProviderRunHooks::default(),
                )
                .await
        }
        Err(err) => Err(err),
    };

    let error_message = result
        .err()
        .map(|error| redact_sensitive(&format!("{error:#}")));

    Ok(ProviderWorkspaceAuthentication {
        install_target,
        checked_at,
        error_message,
    })
}

pub async fn verify_provider_for_workspace_runtime<H>(
    state: &H,
    workspace: &Workspace,
    workspace_id: WorkspaceId,
    provider_id: &str,
    install_target: InstallTarget,
) -> Result<ProviderAuthCheckSnapshot, ProviderAuthCheckServiceError>
where
    H: ProviderProbeHost + ProviderRuntimeHost,
{
    let launch_config =
        config_snapshot::load_provider_launch_config_snapshot(state, provider_id).await;
    launch_config
        .ensure_known_provider(state, provider_id)
        .await
        .map_err(ProviderAuthCheckServiceError::ProviderLaunchConfig)?;
    let checked_at = chrono::Utc::now().to_rfc3339();
    let selected_endpoint = launch_config.selected_endpoint_record();
    let mut selected_endpoint_id = launch_config.selected_endpoint_id();

    if let Some(config_error) = launch_config.managed_config_error.as_ref() {
        return Ok(config_error_snapshot(
            provider_id,
            workspace_id,
            &checked_at,
            config_error.clone(),
        ));
    }

    if let Some(config_error) = launch_config.source_config_error.as_ref() {
        return Ok(config_error_snapshot(
            provider_id,
            workspace_id,
            &checked_at,
            config_error.clone(),
        ));
    }

    let provider_status = launch_config
        .provider_status(state, provider_id, install_target)
        .await;
    let mut outcome = ProviderVerifyOutcome::new(checked_at, selected_endpoint_id.take());

    if !provider_status_is_usable(&provider_status) {
        outcome.apply_unusable_provider(
            provider_status_unusable_reason(&provider_status)
                .unwrap_or_else(|| "provider not ready for use".to_string()),
        );
    } else if let Some(endpoint) = selected_endpoint
        .as_ref()
        .filter(|endpoint| endpoint_supports_model_catalog_verify(endpoint))
    {
        match refresh_provider_endpoint_model_catalog(
            <H as ProviderRuntimeHost>::data_root(state),
            provider_id,
            &endpoint.id,
        )
        .await
        {
            Ok(refreshed_endpoint) => {
                outcome.apply_endpoint_catalog_refresh(refreshed_endpoint);
            }
            Err(err) => {
                outcome.apply_classified_probe_error(redact_sensitive(&err.to_string()));
            }
        }

        if outcome.is_ok() {
            apply_auth_verification_probe(
                state,
                workspace,
                provider_id,
                install_target,
                &mut outcome,
            )
            .await?;
        }
    } else {
        apply_auth_verification_probe(state, workspace, provider_id, install_target, &mut outcome)
            .await?;
    }

    if let Some(endpoint_id) = outcome.selected_endpoint_id() {
        let _ = mark_provider_endpoint_verification(
            <H as ProviderRuntimeHost>::data_root(state),
            provider_id,
            endpoint_id,
            outcome.endpoint_status(),
            outcome.message().cloned(),
        )
        .await;
    }

    let snapshot = outcome.into_snapshot(provider_id, workspace_id);
    store_provider_auth_check_cache(
        state.provider_runtime(),
        workspace_id,
        install_target,
        provider_id,
        &snapshot,
    )
    .await;
    Ok(snapshot)
}

async fn apply_auth_verification_probe<H>(
    state: &H,
    workspace: &Workspace,
    provider_id: &str,
    install_target: InstallTarget,
    outcome: &mut ProviderVerifyOutcome,
) -> Result<(), ProviderAuthCheckServiceError>
where
    H: ProviderProbeHost,
{
    let probe = runtime_probe_service::probe_provider_auth_verification_runtime(
        state,
        workspace,
        provider_id,
        install_target,
        outcome.selected_endpoint_id().map(str::to_string),
    )
    .await
    .map_err(|error| match error {
        runtime_probe_service::PreparedProviderRuntimeProbeError::Verify(error) => {
            ProviderAuthCheckServiceError::Verify(error)
        }
    })?;
    outcome.set_selected_endpoint_id(probe.selected_endpoint_id);
    if let Some(probe_error) = probe.probe_error {
        if outcome.has_endpoint_catalog_result() {
            outcome.apply_endpoint_catalog_runtime_probe_failure(probe_error);
        } else {
            outcome.apply_classified_probe_error(probe_error);
        }
    }
    Ok(())
}

async fn store_provider_auth_check_cache(
    runtime: &crate::ProviderRuntime,
    workspace_id: WorkspaceId,
    install_target: InstallTarget,
    provider_id: &str,
    snapshot: &ProviderAuthCheckSnapshot,
) {
    let verify_value =
        redact_json_value(serde_json::to_value(snapshot).unwrap_or(serde_json::Value::Null));
    store_provider_verify_cache_value(
        runtime,
        workspace_id,
        install_target,
        provider_id,
        verify_value,
    )
    .await;
}
