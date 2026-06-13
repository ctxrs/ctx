use ctx_core::ids::WorkspaceId;
use ctx_core::redaction;
use ctx_harness_sources as harness_sources;
use ctx_harness_sources::HarnessEndpointRecord;
use ctx_providers::adapters::ProviderStatus;

use crate::provider_launch::models::{
    endpoint_models_payload, subscription_models_payload_from_status,
};
use crate::provider_launch::options::runtime_probe_models_payload;
use crate::provider_launch::probe_error::classify_probe_error;
use crate::provider_usability::provider_status_unusable_reason;

fn attach_source_config(
    value: &mut serde_json::Value,
    source_config: Option<&harness_sources::HarnessProviderSourceConfig>,
) {
    if let Some(source) = source_config {
        value["source"] = serde_json::to_value(source).unwrap_or(serde_json::Value::Null);
    }
}

pub struct ProviderOptionsResponseBase<'a> {
    pub provider_id: &'a str,
    pub workspace_id: WorkspaceId,
    pub provider_status: &'a ProviderStatus,
    pub has_active_auth: bool,
    pub auth_mode: &'a str,
    pub source_config: Option<&'a harness_sources::HarnessProviderSourceConfig>,
}

pub struct ProviderOptionsProbeResult {
    pub probe_ok: bool,
    pub auth_required: bool,
    pub probe_error: Option<String>,
}

pub fn env_probe_provider_options_response(
    base: ProviderOptionsResponseBase<'_>,
    probe: ProviderOptionsProbeResult,
) -> serde_json::Value {
    let mut response = serde_json::json!({
        "provider_id": base.provider_id,
        "workspace_id": base.workspace_id.0,
        "installed": base.provider_status.installed,
        "probe_ok": probe.probe_ok,
        "supports_load": false,
        "auth_required": probe.auth_required,
        "has_active_auth": base.has_active_auth,
        "auth_mode": base.auth_mode,
        "probed_at": chrono::Utc::now().to_rfc3339(),
    });
    if let Some(probe_error) = probe.probe_error {
        response["probe_error"] = serde_json::json!(probe_error);
    }
    attach_source_config(&mut response, base.source_config);
    response
}

pub fn selected_endpoint_runtime_launch_options_response(
    base: ProviderOptionsResponseBase<'_>,
    endpoint: &HarnessEndpointRecord,
    probe: ProviderOptionsProbeResult,
) -> serde_json::Value {
    let now = chrono::Utc::now();
    let mut response = serde_json::json!({
        "provider_id": base.provider_id,
        "workspace_id": base.workspace_id.0,
        "installed": base.provider_status.installed,
        "probe_ok": probe.probe_ok,
        "supports_load": false,
        "auth_required": probe.auth_required,
        "has_active_auth": base.has_active_auth,
        "auth_mode": base.auth_mode,
        "models": endpoint_models_payload(base.provider_id, endpoint, now),
        "probed_at": now.to_rfc3339(),
    });
    if let Some(probe_error) = probe.probe_error {
        response["probe_error"] = serde_json::json!(probe_error);
    }
    attach_source_config(&mut response, base.source_config);
    response
}

pub fn config_error_provider_options_response(
    provider_id: &str,
    workspace_id: WorkspaceId,
    installed: Option<bool>,
    auth_mode: &str,
    config_error: &str,
    source_config: Option<&harness_sources::HarnessProviderSourceConfig>,
) -> serde_json::Value {
    let mut response = match installed {
        Some(installed) => serde_json::json!({
            "provider_id": provider_id,
            "workspace_id": workspace_id.0,
            "installed": installed,
            "probe_ok": false,
            "supports_load": false,
            "auth_required": false,
            "has_active_auth": false,
            "auth_mode": auth_mode,
            "probed_at": chrono::Utc::now().to_rfc3339(),
            "probe_error": config_error,
            "config_error": config_error,
        }),
        None => serde_json::json!({
            "provider_id": provider_id,
            "workspace_id": workspace_id.0,
            "probe_ok": false,
            "supports_load": false,
            "auth_required": false,
            "has_active_auth": false,
            "auth_mode": auth_mode,
            "probed_at": chrono::Utc::now().to_rfc3339(),
            "probe_error": config_error,
            "config_error": config_error,
        }),
    };
    attach_source_config(&mut response, source_config);
    response
}

pub fn unusable_provider_options_response(
    provider_id: &str,
    workspace_id: WorkspaceId,
    provider_status: &ProviderStatus,
    has_active_auth: bool,
    auth_mode: &str,
    source_config: Option<&harness_sources::HarnessProviderSourceConfig>,
) -> serde_json::Value {
    let mut response = serde_json::json!({
        "provider_id": provider_id,
        "workspace_id": workspace_id.0,
        "installed": provider_status.installed,
        "health": provider_status.health,
        "diagnostics": provider_status.diagnostics,
        "usability": provider_status.usability,
        "probe_ok": false,
        "probe_error": provider_status_unusable_reason(provider_status)
            .unwrap_or_else(|| "provider not ready for use".to_string()),
        "has_active_auth": has_active_auth,
        "auth_mode": auth_mode,
        "probed_at": chrono::Utc::now().to_rfc3339(),
    });
    attach_source_config(&mut response, source_config);
    response
}

pub fn runtime_models_provider_options_response(
    provider_id: &str,
    workspace_id: WorkspaceId,
    provider_status: &ProviderStatus,
    probe: anyhow::Result<ctx_providers::crp::CrpModelsProbe>,
    has_active_auth: bool,
    auth_mode: &str,
    source_config: Option<&harness_sources::HarnessProviderSourceConfig>,
) -> serde_json::Value {
    let mut response = match probe {
        Ok(probe) => runtime_models_success_response(
            provider_id,
            workspace_id,
            provider_status,
            probe,
            has_active_auth,
            auth_mode,
        ),
        Err(err) => runtime_models_error_response(
            provider_id,
            workspace_id,
            provider_status,
            err,
            has_active_auth,
            auth_mode,
        ),
    };
    attach_source_config(&mut response, source_config);
    response
}

fn runtime_models_success_response(
    provider_id: &str,
    workspace_id: WorkspaceId,
    provider_status: &ProviderStatus,
    probe: ctx_providers::crp::CrpModelsProbe,
    has_active_auth: bool,
    auth_mode: &str,
) -> serde_json::Value {
    let fallback_current_model_id = subscription_models_payload_from_status(provider_status)
        .and_then(|models| {
            models
                .get("current_model_id")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        });
    let mut value = serde_json::json!({
        "provider_id": provider_id,
        "workspace_id": workspace_id.0,
        "installed": provider_status.installed,
        "probe_ok": true,
        "supports_load": false,
        "auth_required": false,
        "has_active_auth": has_active_auth,
        "auth_mode": auth_mode,
        "probed_at": chrono::Utc::now().to_rfc3339(),
    });
    if let Some(models) =
        runtime_probe_models_payload(provider_id, &probe, fallback_current_model_id.as_deref())
    {
        value["models"] = models;
        return value;
    }

    missing_runtime_catalog_response(
        provider_id,
        workspace_id,
        provider_status,
        &probe,
        has_active_auth,
        auth_mode,
        value
            .get("probed_at")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
    )
}

fn missing_runtime_catalog_response(
    provider_id: &str,
    workspace_id: WorkspaceId,
    provider_status: &ProviderStatus,
    probe: &ctx_providers::crp::CrpModelsProbe,
    has_active_auth: bool,
    auth_mode: &str,
    probed_at: &str,
) -> serde_json::Value {
    let catalog_source = probe.catalog_source.as_deref().unwrap_or("missing");
    let current_model_id = probe.current_model_id.as_deref().unwrap_or("missing");
    let model_count = probe.models.len();
    serde_json::json!({
        "provider_id": provider_id,
        "workspace_id": workspace_id.0,
        "installed": provider_status.installed,
        "probe_ok": false,
        "probe_error": format!(
            "runtime_model_catalog_missing: provider={provider_id} catalog_source={catalog_source} current_model_id={current_model_id} model_count={model_count}"
        ),
        "auth_required": false,
        "has_active_auth": has_active_auth,
        "auth_mode": auth_mode,
        "probed_at": probed_at,
        "supports_load": false,
    })
}

fn runtime_models_error_response(
    provider_id: &str,
    workspace_id: WorkspaceId,
    provider_status: &ProviderStatus,
    err: anyhow::Error,
    has_active_auth: bool,
    auth_mode: &str,
) -> serde_json::Value {
    let probe_error = redaction::redact_sensitive(&err.to_string());
    let (_, auth_required, _) = classify_probe_error(&probe_error);
    serde_json::json!({
        "provider_id": provider_id,
        "workspace_id": workspace_id.0,
        "installed": provider_status.installed,
        "probe_ok": false,
        "probe_error": probe_error,
        "auth_required": auth_required.unwrap_or(false),
        "has_active_auth": has_active_auth,
        "auth_mode": auth_mode,
        "probed_at": chrono::Utc::now().to_rfc3339(),
    })
}
