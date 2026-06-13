use std::path::Path;

use ctx_observability::logs;
use ctx_provider_runtime::provider_launch::status::mark_provider_status_with_managed_config_error;
use ctx_provider_runtime::ProviderRuntime;
use ctx_providers::adapters::ProviderStatus;

#[derive(Debug)]
pub struct ProviderDiagnosticsSnapshot {
    pub providers: Vec<ProviderStatus>,
    pub managed_installs: serde_json::Value,
}

pub(in crate::daemon) async fn provider_diagnostics_snapshot_for_runtime(
    data_root: &Path,
    providers: &ProviderRuntime,
) -> ProviderDiagnosticsSnapshot {
    let (managed_installs, managed_config_error) =
        match ctx_managed_installs::load_agent_server_config(data_root).await {
            Ok(config) => (
                serde_json::to_value(config).unwrap_or_else(|_| serde_json::json!({})),
                None,
            ),
            Err(error) => {
                let error = logs::redact_sensitive(&error.to_string());
                (serde_json::json!({ "error": error }), Some(error))
            }
        };
    let managed_installs = redact_json_value(managed_installs);

    let mut providers = providers.provider_statuses().await;
    if let Some(config_error) = managed_config_error.as_deref() {
        for status in &mut providers {
            mark_provider_status_with_managed_config_error(status, config_error);
        }
    }

    let providers = providers
        .into_iter()
        .map(redact_provider_status_for_diagnostics)
        .collect();

    ProviderDiagnosticsSnapshot {
        providers,
        managed_installs,
    }
}

fn redact_provider_status_for_diagnostics(mut status: ProviderStatus) -> ProviderStatus {
    status.diagnostics = status
        .diagnostics
        .into_iter()
        .map(|diagnostic| logs::redact_sensitive(&diagnostic))
        .collect();
    status.details = status
        .details
        .into_iter()
        .filter(|(key, _)| !ctx_core::redaction::is_sensitive_key(key))
        .map(|(key, value)| (key, logs::redact_sensitive(&value)))
        .collect();
    status
}

fn redact_json_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (key, value) in map {
                if ctx_core::redaction::is_sensitive_key(&key) {
                    out.insert(key, serde_json::Value::String("[REDACTED]".to_string()));
                    continue;
                }
                out.insert(key, redact_json_value(value));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(redact_json_value).collect())
        }
        other => other,
    }
}
