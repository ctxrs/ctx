use super::*;

#[test]
fn endpoint_models_payload_includes_models_and_meta() {
    let now = Utc::now();
    let mut endpoint = test_endpoint("ep-1");
    endpoint.model_override = Some("openai/gpt-5.2".to_string());
    endpoint.model_catalog_status = harness_sources::EndpointModelCatalogStatus::Ready;
    endpoint.model_catalog_fetched_at = Some(now);
    endpoint.model_catalog_source = Some("mixed".to_string());
    endpoint.model_catalog_models = vec![harness_sources::EndpointModelRecord {
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
    endpoint.model_catalog_models = vec![harness_sources::EndpointModelRecord {
        id: "openai/gpt-5.2".to_string(),
        name: Some("GPT-5.2".to_string()),
    }];
    endpoint.manual_model_ids = vec![
        "openai/gpt-5.2".to_string(),
        "custom/manual-model".to_string(),
    ];
    endpoint.model_catalog_source = Some("mixed".to_string());
    endpoint.model_catalog_status = harness_sources::EndpointModelCatalogStatus::Ready;

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
