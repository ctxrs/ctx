use std::collections::HashSet;

use chrono::{DateTime, Utc};
use ctx_harness_sources::{
    self as harness_sources, HarnessEndpointRecord, HarnessEndpointVerificationStatus,
};
use ctx_providers::adapters::ProviderStatus;
use serde_json::Value;

use super::probe_error::classify_probe_error;

pub fn subscription_models_payload_from_status(provider_status: &ProviderStatus) -> Option<Value> {
    if provider_status.provider_id == "fake" {
        return Some(serde_json::json!({
            "catalog_source": "fake_provider",
            "current_model_id": "fake-model",
            "models": [
                {
                    "id": "fake-model",
                    "name": "fake-model",
                }
            ],
            "meta": {
                "source_kind": "subscription",
                "catalog_source": "fake_provider",
                "refresh_pending": false,
            },
        }));
    }
    ctx_provider_accounts::pinned_subscription_models_value(
        &provider_status.provider_id,
        provider_status.version.as_deref(),
    )
}

fn endpoint_model_entries(endpoint: &HarnessEndpointRecord) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();

    for model in &endpoint.model_catalog_models {
        let id = model.id.trim();
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        entries.push(serde_json::json!({
            "id": id,
            "name": model.name.clone(),
        }));
    }

    for model_id in &endpoint.manual_model_ids {
        let id = model_id.trim();
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        entries.push(serde_json::json!({
            "id": id,
        }));
    }

    entries
}

fn endpoint_current_model_id(
    provider_id: &str,
    endpoint: &HarnessEndpointRecord,
) -> Option<String> {
    if provider_id == "droid" {
        return harness_sources::droid_cli_model_id_for_endpoint_model(
            endpoint.model_override.as_deref(),
            endpoint.base_url.as_deref(),
        );
    }
    endpoint
        .model_override
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn endpoint_models_payload(
    provider_id: &str,
    endpoint: &HarnessEndpointRecord,
    now: DateTime<Utc>,
) -> Value {
    let stale = harness_sources::endpoint_model_catalog_is_stale(endpoint, now);
    serde_json::json!({
        "models": endpoint_model_entries(endpoint),
        "current_model_id": endpoint_current_model_id(provider_id, endpoint),
        "meta": {
            "source_kind": "endpoint",
            "catalog_status": endpoint.model_catalog_status,
            "catalog_source": endpoint.model_catalog_source,
            "fetched_at": endpoint.model_catalog_fetched_at,
            "last_error": endpoint.model_catalog_error,
            "stale": stale,
        },
    })
}

pub fn supplement_models_payload_with_endpoint_metadata(
    models: &mut Value,
    provider_id: &str,
    endpoint: &HarnessEndpointRecord,
    now: DateTime<Utc>,
) {
    let endpoint_payload = endpoint_models_payload(provider_id, endpoint, now);
    let Some(models_obj) = models.as_object_mut() else {
        *models = endpoint_payload;
        return;
    };

    let missing_current_model = models_obj
        .get("current_model_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::is_empty)
        .unwrap_or(true);
    if missing_current_model {
        if let Some(current_model_id) = endpoint_payload
            .get("current_model_id")
            .cloned()
            .filter(|value| !value.is_null())
        {
            models_obj.insert("current_model_id".to_string(), current_model_id);
        }
    }

    let endpoint_meta = endpoint_payload.get("meta").cloned().unwrap_or(Value::Null);
    match models_obj.get_mut("meta") {
        Some(Value::Object(meta_obj)) => {
            meta_obj.insert("endpoint".to_string(), endpoint_meta);
        }
        _ => {
            models_obj.insert(
                "meta".to_string(),
                serde_json::json!({
                    "endpoint": endpoint_meta,
                }),
            );
        }
    }
}

pub fn endpoint_catalog_runtime_probe_failure(
    message: String,
    endpoint_status: HarnessEndpointVerificationStatus,
) -> (
    String,
    Option<bool>,
    Option<String>,
    HarnessEndpointVerificationStatus,
) {
    let (status, auth_required, _) = classify_probe_error(&message);
    (
        status.to_string(),
        auth_required,
        Some(message),
        endpoint_status,
    )
}

pub fn endpoint_catalog_verify_outcome(
    endpoint: &HarnessEndpointRecord,
) -> (
    String,
    Option<bool>,
    Option<String>,
    HarnessEndpointVerificationStatus,
) {
    match endpoint.model_catalog_status {
        harness_sources::EndpointModelCatalogStatus::Ready
        | harness_sources::EndpointModelCatalogStatus::ManualOnly => (
            "ok".to_string(),
            Some(false),
            None,
            HarnessEndpointVerificationStatus::Valid,
        ),
        harness_sources::EndpointModelCatalogStatus::Unknown
        | harness_sources::EndpointModelCatalogStatus::Error => {
            let detail = endpoint
                .model_catalog_error
                .as_ref()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| {
                    "endpoint model catalog is unavailable; refresh endpoint models in Settings"
                        .to_string()
                });
            let redacted = ctx_core::redaction::redact_sensitive(&detail);
            let (status, auth_required, endpoint_status) = classify_probe_error(&redacted);
            (
                status.to_string(),
                auth_required,
                Some(redacted),
                endpoint_status,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ctx_harness_sources::{
        EndpointModelCatalogStatus, EndpointModelRecord, HarnessApiShape, HarnessEndpointRecord,
        HarnessEndpointVerificationStatus,
    };

    use super::{
        endpoint_catalog_runtime_probe_failure, endpoint_catalog_verify_outcome,
        endpoint_models_payload, supplement_models_payload_with_endpoint_metadata,
    };

    fn test_endpoint(id: &str) -> HarnessEndpointRecord {
        HarnessEndpointRecord {
            id: id.to_string(),
            provider_id: "codex".to_string(),
            name: "Test endpoint".to_string(),
            base_url: Some("https://api.openai.com/v1".to_string()),
            api_shape: HarnessApiShape::OpenaiResponses,
            auth_type: "bearer".to_string(),
            model_override: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_verification_status: HarnessEndpointVerificationStatus::Unknown,
            last_verification_at: None,
            last_error: None,
            has_api_key: true,
            model_catalog_status: EndpointModelCatalogStatus::Unknown,
            model_catalog_fetched_at: None,
            model_catalog_error: None,
            model_catalog_models: Vec::new(),
            manual_model_ids: Vec::new(),
            model_catalog_source: None,
        }
    }

    #[test]
    fn endpoint_models_payload_includes_models_and_meta() {
        let now = Utc::now();
        let mut endpoint = test_endpoint("ep-1");
        endpoint.model_override = Some("openai/gpt-5.2".to_string());
        endpoint.model_catalog_status = EndpointModelCatalogStatus::Ready;
        endpoint.model_catalog_fetched_at = Some(now);
        endpoint.model_catalog_source = Some("mixed".to_string());
        endpoint.model_catalog_models = vec![EndpointModelRecord {
            id: "openai/gpt-5.2".to_string(),
            name: Some("GPT-5.2".to_string()),
        }];

        let payload = endpoint_models_payload("codex", &endpoint, now);
        assert_eq!(
            payload
                .pointer("/models/0/id")
                .and_then(serde_json::Value::as_str),
            Some("openai/gpt-5.2")
        );
        assert_eq!(
            payload
                .pointer("/current_model_id")
                .and_then(serde_json::Value::as_str),
            Some("openai/gpt-5.2")
        );
        assert_eq!(
            payload
                .pointer("/meta/catalog_status")
                .and_then(serde_json::Value::as_str),
            Some("ready")
        );
        assert_eq!(
            payload
                .pointer("/meta/catalog_source")
                .and_then(serde_json::Value::as_str),
            Some("mixed")
        );
        assert_eq!(
            payload
                .pointer("/meta/source_kind")
                .and_then(serde_json::Value::as_str),
            Some("endpoint")
        );
        assert_eq!(
            payload
                .pointer("/meta/stale")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn endpoint_models_payload_uses_droid_custom_model_selector() {
        let now = Utc::now();
        let mut endpoint = test_endpoint("ep-1");
        endpoint.base_url = Some("https://openrouter.ai/api/v1".to_string());
        endpoint.model_override = Some("openai/gpt-5.2".to_string());

        let payload = endpoint_models_payload("droid", &endpoint, now);
        assert_eq!(
            payload
                .pointer("/current_model_id")
                .and_then(serde_json::Value::as_str),
            Some("custom:openai/gpt-5.2-[openrouter]-0")
        );
    }

    #[test]
    fn endpoint_models_payload_merges_manual_model_ids() {
        let now = Utc::now();
        let mut endpoint = test_endpoint("ep-1");
        endpoint.model_catalog_models = vec![EndpointModelRecord {
            id: "openai/gpt-5.2".to_string(),
            name: Some("GPT-5.2".to_string()),
        }];
        endpoint.manual_model_ids = vec![
            "openai/gpt-5.2".to_string(),
            "custom/manual-model".to_string(),
        ];
        endpoint.model_catalog_source = Some("mixed".to_string());
        endpoint.model_catalog_status = EndpointModelCatalogStatus::Ready;

        let payload = endpoint_models_payload("codex", &endpoint, now);
        let models = payload
            .get("models")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .expect("models array");
        assert_eq!(models.len(), 2);
        assert_eq!(
            models[1].get("id").and_then(serde_json::Value::as_str),
            Some("custom/manual-model")
        );
    }

    #[test]
    fn supplement_models_payload_preserves_live_probe_catalog() {
        let mut endpoint = test_endpoint("ep-1");
        endpoint.name = "OpenRouter".to_string();
        endpoint.base_url = Some("https://openrouter.ai/api/v1".to_string());
        endpoint.model_override = Some("openai/gpt-5.2".to_string());
        endpoint.model_catalog_status = EndpointModelCatalogStatus::Ready;
        endpoint.model_catalog_fetched_at = Some(Utc::now());
        endpoint.model_catalog_models = vec![EndpointModelRecord {
            id: "openai/gpt-5.2".to_string(),
            name: Some("GPT-5.2".to_string()),
        }];
        endpoint.manual_model_ids = vec!["manual/fallback".to_string()];
        endpoint.model_catalog_source = Some("mixed".to_string());
        let now = Utc::now();
        let mut models = serde_json::json!({
            "models": [
                { "id": "openai/gpt-5.4", "name": "GPT-5.4" },
                { "id": "openai/o3", "name": "o3" }
            ],
            "current_model_id": "openai/gpt-5.4",
            "meta": {
                "source_kind": "subscription",
                "catalog_source": "runtime_probe_live",
                "refresh_pending": false,
            },
        });

        supplement_models_payload_with_endpoint_metadata(&mut models, "codex", &endpoint, now);

        let model_ids = models
            .get("models")
            .and_then(serde_json::Value::as_array)
            .expect("models array")
            .iter()
            .filter_map(|entry| entry.get("id").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(model_ids, vec!["openai/gpt-5.4", "openai/o3"]);
        assert_eq!(
            models
                .get("current_model_id")
                .and_then(serde_json::Value::as_str),
            Some("openai/gpt-5.4")
        );
        assert_eq!(
            models
                .pointer("/meta/source_kind")
                .and_then(serde_json::Value::as_str),
            Some("subscription")
        );
        assert_eq!(
            models
                .pointer("/meta/endpoint/catalog_status")
                .and_then(serde_json::Value::as_str),
            Some("ready")
        );
        assert_eq!(
            models
                .pointer("/meta/endpoint/catalog_source")
                .and_then(serde_json::Value::as_str),
            Some("mixed")
        );
    }

    #[test]
    fn supplement_models_payload_uses_endpoint_current_model_when_probe_has_none() {
        let mut endpoint = test_endpoint("ep-1");
        endpoint.name = "OpenRouter".to_string();
        endpoint.base_url = Some("https://openrouter.ai/api/v1".to_string());
        endpoint.model_override = Some("openai/gpt-5.2".to_string());
        endpoint.model_catalog_status = EndpointModelCatalogStatus::Ready;
        endpoint.model_catalog_fetched_at = Some(Utc::now());
        endpoint.model_catalog_models = vec![EndpointModelRecord {
            id: "openai/gpt-5.2".to_string(),
            name: Some("GPT-5.2".to_string()),
        }];
        endpoint.manual_model_ids = vec!["manual/fallback".to_string()];
        endpoint.model_catalog_source = Some("mixed".to_string());
        let now = Utc::now();
        let mut models = serde_json::json!({
            "models": [
                { "id": "openai/gpt-5.4", "name": "GPT-5.4" }
            ],
            "meta": {
                "source_kind": "subscription",
                "catalog_source": "runtime_probe_live",
                "refresh_pending": false,
            },
        });

        supplement_models_payload_with_endpoint_metadata(&mut models, "codex", &endpoint, now);

        assert_eq!(
            models
                .get("current_model_id")
                .and_then(serde_json::Value::as_str),
            Some("openai/gpt-5.2")
        );
    }

    #[test]
    fn endpoint_catalog_verify_outcome_ready_and_manual_only_are_valid() {
        let mut ready = test_endpoint("ep-ready");
        ready.model_catalog_status = EndpointModelCatalogStatus::Ready;
        let (status, auth_required, message, endpoint_status) =
            endpoint_catalog_verify_outcome(&ready);
        assert_eq!(status, "ok");
        assert_eq!(auth_required, Some(false));
        assert!(message.is_none());
        assert_eq!(endpoint_status, HarnessEndpointVerificationStatus::Valid);

        let mut manual = test_endpoint("ep-manual");
        manual.model_catalog_status = EndpointModelCatalogStatus::ManualOnly;
        let (status, auth_required, message, endpoint_status) =
            endpoint_catalog_verify_outcome(&manual);
        assert_eq!(status, "ok");
        assert_eq!(auth_required, Some(false));
        assert!(message.is_none());
        assert_eq!(endpoint_status, HarnessEndpointVerificationStatus::Valid);
    }

    #[test]
    fn endpoint_catalog_verify_outcome_classifies_auth_errors() {
        let mut endpoint = test_endpoint("ep-auth");
        endpoint.model_catalog_status = EndpointModelCatalogStatus::Error;
        endpoint.model_catalog_error = Some("model discovery failed with status 401".to_string());
        let (status, auth_required, message, endpoint_status) =
            endpoint_catalog_verify_outcome(&endpoint);
        assert_eq!(status, "auth_required");
        assert_eq!(auth_required, Some(true));
        assert!(message
            .as_deref()
            .is_some_and(|value| value.contains("status 401")));
        assert_eq!(endpoint_status, HarnessEndpointVerificationStatus::Invalid);
    }

    #[test]
    fn endpoint_catalog_runtime_probe_failure_preserves_endpoint_status() {
        let (status, auth_required, message, endpoint_status) =
            endpoint_catalog_runtime_probe_failure(
                "connection refused while launching bundled runtime".to_string(),
                HarnessEndpointVerificationStatus::Valid,
            );
        assert_eq!(status, "network_error");
        assert_eq!(auth_required, Some(false));
        assert!(message
            .as_deref()
            .is_some_and(|value| value.contains("connection refused")));
        assert_eq!(endpoint_status, HarnessEndpointVerificationStatus::Valid);
    }
}
