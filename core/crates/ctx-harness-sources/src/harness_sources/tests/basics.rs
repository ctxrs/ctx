use super::model_catalog::{
    infer_endpoint_model_provider_namespace, normalize_namespaced_model_override,
    parse_openai_models_payload, truncate_discovery_error,
};
use super::validation::normalize_manual_model_ids;
use super::*;

#[test]
fn endpoint_support_gating_includes_cline_goose_and_openhands_but_not_cursor() {
    assert!(supports_harness_endpoint(PROVIDER_CLINE));
    assert!(supports_harness_endpoint(PROVIDER_CLAUDE));
    assert!(!supports_harness_endpoint(PROVIDER_CURSOR));
    assert!(supports_harness_endpoint(PROVIDER_CODEX));
    assert_eq!(
        default_shape_for_provider(PROVIDER_CLINE),
        Some(HarnessApiShape::OpenaiResponses)
    );
    assert!(supports_harness_endpoint(PROVIDER_GOOSE));
    assert!(supports_harness_endpoint(PROVIDER_OPENHANDS));
}

#[tokio::test]
async fn cursor_source_config_defaults_to_subscription_without_endpoints() {
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = get_provider_source_config(root.path(), PROVIDER_CURSOR)
        .await
        .expect("config");
    assert_eq!(cfg.selected_source_kind, HarnessSourceKind::Subscription);
    assert!(cfg.selected_endpoint_id.is_none());
    assert!(cfg.endpoints.is_empty());
}

#[tokio::test]
async fn cursor_rejects_endpoint_source_selection() {
    let root = tempfile::tempdir().expect("tempdir");
    let err = set_provider_source_selection(
        root.path(),
        PROVIDER_CURSOR,
        HarnessSourceKind::Endpoint,
        Some("ep-1".to_string()),
    )
    .await
    .expect_err("cursor endpoint mode should be rejected");
    assert!(err
        .to_string()
        .contains("provider does not support harness endpoints"));
}

#[test]
fn parse_openai_models_payload_extracts_unique_ids() {
    let payload = serde_json::json!({
        "data": [
            { "id": "openai/gpt-5.2", "name": "GPT-5.2" },
            { "id": "openai/gpt-5.2" },
            { "id": "openai/gpt-4.1" },
            { "id": "" },
            {}
        ]
    });
    let models = parse_openai_models_payload(&payload).expect("models should parse");
    assert_eq!(
        models,
        vec![
            EndpointModelRecord {
                id: "openai/gpt-5.2".to_string(),
                name: Some("GPT-5.2".to_string()),
            },
            EndpointModelRecord {
                id: "openai/gpt-4.1".to_string(),
                name: None,
            },
        ]
    );
}

#[test]
fn infer_endpoint_model_provider_namespace_prefers_non_generic_host_label() {
    assert_eq!(
        infer_endpoint_model_provider_namespace("https://openrouter.ai/api/v1"),
        Some("openrouter".to_string())
    );
    assert_eq!(
        infer_endpoint_model_provider_namespace("https://api.myawesomeprovider.example/v1"),
        Some("myawesomeprovider".to_string())
    );
}

#[test]
fn ctx_managed_relay_url_matching_respects_origin_and_path_boundaries() {
    let prefix = Url::parse("https://relay.ctx.rs/relay").expect("prefix url");
    let matching_child =
        Url::parse("https://relay.ctx.rs/relay/openai/v1").expect("matching child url");
    let matching_exact = Url::parse("https://relay.ctx.rs/relay").expect("matching exact url");
    let bad_host =
        Url::parse("https://relay.ctx.rs.evil.example/relay/openai/v1").expect("bad host url");
    let bad_path = Url::parse("https://relay.ctx.rs/relayx/openai/v1").expect("bad path url");

    assert!(base_url_matches_ctx_managed_prefix(
        &matching_child,
        &prefix
    ));
    assert!(base_url_matches_ctx_managed_prefix(
        &matching_exact,
        &prefix
    ));
    assert!(!base_url_matches_ctx_managed_prefix(&bad_host, &prefix));
    assert!(!base_url_matches_ctx_managed_prefix(&bad_path, &prefix));
    assert!(!base_url_matches_ctx_managed_prefix(
        &matching_child,
        &Url::parse("https://relay.ctx.rs").expect("origin-only prefix url")
    ));
    assert!(base_url_uses_ctx_managed_relay(
        "https://api.ctx.rs/relay/openai/v1"
    ));
    assert!(!base_url_uses_ctx_managed_relay(
        "https://api.ctx.rs/functions/v1/openai"
    ));
    assert!(!base_url_uses_ctx_managed_relay(
        "https://api.ctx.rs/relayx/openai/v1"
    ));
}

#[test]
fn normalize_namespaced_model_override_always_prefixes_namespace() {
    assert_eq!(
        normalize_namespaced_model_override("openai/gpt-5.2-codex", Some("openrouter")),
        "openrouter/openai/gpt-5.2-codex"
    );
    assert_eq!(
        normalize_namespaced_model_override("openrouter/openai/gpt-5.2-codex", Some("openrouter"),),
        "openrouter/openrouter/openai/gpt-5.2-codex"
    );
    assert_eq!(
        normalize_namespaced_model_override(
            "myawesomeprovider/openai/gpt-5.2-codex",
            Some("openrouter"),
        ),
        "openrouter/myawesomeprovider/openai/gpt-5.2-codex"
    );
}

#[test]
fn parse_openai_models_payload_requires_data_array() {
    let payload = serde_json::json!({
        "models": []
    });
    let err = parse_openai_models_payload(&payload).expect_err("missing data should error");
    assert!(err
        .to_string()
        .contains("models payload missing array field 'data'"));
}

#[test]
fn normalize_manual_model_ids_deduplicates_and_trims() {
    let input = vec![
        " openai/gpt-5.2 ".to_string(),
        "".to_string(),
        "openai/gpt-5.2".to_string(),
        "anthropic/claude-sonnet-4.5".to_string(),
    ];
    assert_eq!(
        normalize_manual_model_ids(&input),
        vec![
            "openai/gpt-5.2".to_string(),
            "anthropic/claude-sonnet-4.5".to_string(),
        ]
    );
}

#[test]
fn truncate_discovery_error_preserves_utf8_boundaries() {
    let raw = format!("error: {}", "界".repeat(400));
    let truncated = truncate_discovery_error(&raw);
    assert!(truncated.ends_with("..."));
    assert!(truncated.len() <= 283);
    assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
}

#[test]
fn endpoint_catalog_stale_logic_handles_status_and_age() {
    let now = Utc::now();
    let mut endpoint = HarnessEndpointRecord {
        id: "ep-1".to_string(),
        provider_id: PROVIDER_CODEX.to_string(),
        name: "OpenRouter".to_string(),
        base_url: Some("https://openrouter.ai/api/v1".to_string()),
        api_shape: HarnessApiShape::OpenaiResponses,
        auth_type: CODEX_AUTH_TYPE_BEARER.to_string(),
        model_override: None,
        created_at: now,
        updated_at: now,
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
    };
    assert!(endpoint_model_catalog_is_stale(&endpoint, now));

    endpoint.model_catalog_status = EndpointModelCatalogStatus::ManualOnly;
    assert!(!endpoint_model_catalog_is_stale(&endpoint, now));

    endpoint.model_catalog_status = EndpointModelCatalogStatus::Ready;
    endpoint.model_catalog_fetched_at = Some(now - chrono::Duration::hours(1));
    assert!(!endpoint_model_catalog_is_stale(&endpoint, now));

    endpoint.model_catalog_fetched_at = Some(now - chrono::Duration::hours(30));
    assert!(endpoint_model_catalog_is_stale(&endpoint, now));
}

#[tokio::test]
async fn defaults_to_subscription_without_registry() {
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = get_provider_source_config(root.path(), PROVIDER_CODEX)
        .await
        .expect("config");
    assert_eq!(cfg.selected_source_kind, HarnessSourceKind::Subscription);
    assert!(cfg.selected_endpoint_id.is_none());
    assert!(cfg.endpoints.is_empty());
}
