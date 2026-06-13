use super::*;

#[tokio::test]
async fn shape_compatibility_rejects_mismatch() {
    let root = tempfile::tempdir().expect("tempdir");
    let err = upsert_provider_endpoint(
        root.path(),
        PROVIDER_CODEX,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "wrong".to_string(),
            base_url: Some("https://example.com".to_string()),
            api_shape: Some(HarnessApiShape::AnthropicMessages),
            auth_type: None,
            model_override: None,
            api_key: Some("k".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect_err("expected shape mismatch");
    assert!(err
        .to_string()
        .contains("codex requires api_shape=openai_responses"));
}

#[tokio::test]
async fn unsafe_endpoint_id_is_rejected() {
    let root = tempfile::tempdir().expect("tempdir");
    let err = upsert_provider_endpoint(
        root.path(),
        PROVIDER_CODEX,
        HarnessEndpointUpsert {
            endpoint_id: Some("../escape".to_string()),
            name: "bad".to_string(),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: None,
            api_key: Some("k".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect_err("unsafe endpoint id should fail");
    assert!(err
        .to_string()
        .contains("endpoint_id may only contain ASCII letters"));
}

#[tokio::test]
async fn deleting_missing_provider_endpoint_returns_unknown_endpoint() {
    let root = tempfile::tempdir().expect("tempdir");
    let err = delete_provider_endpoint(root.path(), PROVIDER_QWEN, "missing")
        .await
        .expect_err("missing endpoint should fail");
    assert!(err.to_string().contains("unknown endpoint"));
}

#[tokio::test]
async fn get_provider_source_config_fails_on_missing_selected_endpoint_without_repair() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_CODEX,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Codex endpoint".to_string(),
            base_url: Some("https://api.openai.com/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("gpt-5.4".to_string()),
            api_key: Some("sk-test".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert endpoint");

    set_provider_source_selection(
        root.path(),
        PROVIDER_CODEX,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select endpoint");

    let mut registry = registry::load_registry(root.path())
        .await
        .expect("load registry");
    let provider = registry
        .providers
        .get_mut(PROVIDER_CODEX)
        .expect("provider entry");
    provider.endpoints.clear();
    registry::save_registry(root.path(), &registry)
        .await
        .expect("save stale registry");

    let err = get_provider_source_config(root.path(), PROVIDER_CODEX)
        .await
        .expect_err("stale selected endpoint should fail");
    assert!(err.to_string().contains("selected endpoint"));

    let repaired = registry::load_registry(root.path())
        .await
        .expect("reload registry");
    let provider = repaired.providers.get(PROVIDER_CODEX).expect("provider");
    assert_eq!(provider.selected_source_kind, HarnessSourceKind::Endpoint);
    assert_eq!(
        provider.selected_endpoint_id.as_deref(),
        Some(endpoint.id.as_str())
    );
    assert!(provider.endpoints.is_empty());
}

#[tokio::test]
async fn get_provider_source_config_marks_missing_endpoint_secret_as_unconfigured() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_CODEX,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Codex endpoint".to_string(),
            base_url: Some("https://api.openai.com/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("gpt-5.4".to_string()),
            api_key: Some("sk-test".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert endpoint");
    assert!(endpoint.has_api_key);

    let registry = registry::load_registry(root.path())
        .await
        .expect("load registry");
    let provider = registry
        .providers
        .get(PROVIDER_CODEX)
        .expect("provider entry");
    let internal = provider
        .endpoints
        .iter()
        .find(|candidate| candidate.id == endpoint.id)
        .expect("internal endpoint");
    std::fs::remove_file(
        secrets::endpoint_secret_path(root.path(), &internal.secret_ref).expect("secret path"),
    )
    .expect("remove endpoint secret");

    let cfg = get_provider_source_config(root.path(), PROVIDER_CODEX)
        .await
        .expect("get source config");
    assert_eq!(cfg.endpoints.len(), 1);
    assert!(!cfg.endpoints[0].has_api_key);
}

#[tokio::test]
async fn delete_provider_endpoint_skips_unsafe_secret_ref_and_preserves_outside_file() {
    let root = tempfile::tempdir().expect("tempdir");
    let outside_dir = tempfile::tempdir().expect("outside tempdir");
    let outside_path = outside_dir.path().join("outside-secret.json");
    std::fs::write(&outside_path, b"{\"api_key\":\"outside\"}\n").expect("write outside secret");

    let endpoint_id = "endpoint-1".to_string();
    let mut registry_state = HarnessSourceRegistryInternal {
        version: REGISTRY_VERSION,
        providers: BTreeMap::new(),
    };
    let provider = registry_state
        .providers
        .entry(PROVIDER_CODEX.to_string())
        .or_default();
    provider.selected_source_kind = HarnessSourceKind::Endpoint;
    provider.selected_endpoint_id = Some(endpoint_id.clone());
    provider.endpoints.push(HarnessEndpointRecordInternal {
        id: endpoint_id.clone(),
        provider_id: PROVIDER_CODEX.to_string(),
        name: "Poisoned endpoint".to_string(),
        base_url: "https://api.openai.com/v1".to_string(),
        api_shape: HarnessApiShape::OpenaiResponses,
        auth_type: CODEX_AUTH_TYPE_BEARER.to_string(),
        model_override: Some("gpt-5.4".to_string()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        last_verification_status: HarnessEndpointVerificationStatus::Unknown,
        last_verification_at: None,
        last_error: None,
        model_catalog_status: EndpointModelCatalogStatus::Unknown,
        model_catalog_fetched_at: None,
        model_catalog_error: None,
        model_catalog_models: Vec::new(),
        manual_model_ids: Vec::new(),
        model_catalog_source: None,
        secret_ref: outside_path.to_string_lossy().to_string(),
    });
    registry::save_registry(root.path(), &registry_state)
        .await
        .expect("save poisoned registry");

    delete_provider_endpoint(root.path(), PROVIDER_CODEX, &endpoint_id)
        .await
        .expect("delete poisoned endpoint");

    assert_eq!(
        std::fs::read_to_string(&outside_path).expect("outside file must remain"),
        "{\"api_key\":\"outside\"}\n"
    );

    let repaired = registry::load_registry(root.path())
        .await
        .expect("reload registry");
    let provider = repaired.providers.get(PROVIDER_CODEX).expect("provider");
    assert!(provider.endpoints.is_empty());
    assert_eq!(
        provider.selected_source_kind,
        HarnessSourceKind::Subscription
    );
    assert!(provider.selected_endpoint_id.is_none());
}

#[tokio::test]
async fn resolve_provider_source_for_run_fails_on_missing_selected_endpoint_without_repair() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_CODEX,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Codex endpoint".to_string(),
            base_url: Some("https://api.openai.com/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("gpt-5.4".to_string()),
            api_key: Some("sk-test".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert endpoint");

    set_provider_source_selection(
        root.path(),
        PROVIDER_CODEX,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select endpoint");

    let mut registry = registry::load_registry(root.path())
        .await
        .expect("load registry");
    let provider = registry
        .providers
        .get_mut(PROVIDER_CODEX)
        .expect("provider entry");
    provider.endpoints.clear();
    registry::save_registry(root.path(), &registry)
        .await
        .expect("save stale registry");

    let err = resolve_provider_source_for_run(root.path(), PROVIDER_CODEX)
        .await
        .expect_err("stale selected endpoint should fail");
    assert!(err.to_string().contains("selected endpoint"));

    let repaired = registry::load_registry(root.path())
        .await
        .expect("reload registry");
    let provider = repaired.providers.get(PROVIDER_CODEX).expect("provider");
    assert_eq!(provider.selected_source_kind, HarnessSourceKind::Endpoint);
    assert_eq!(
        provider.selected_endpoint_id.as_deref(),
        Some(endpoint.id.as_str())
    );
}

#[tokio::test]
async fn get_provider_source_config_fails_on_stray_selected_endpoint_when_subscription_is_active() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_CODEX,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Codex endpoint".to_string(),
            base_url: Some("https://api.openai.com/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("gpt-5.4".to_string()),
            api_key: Some("sk-test".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert endpoint");

    let mut registry = registry::load_registry(root.path())
        .await
        .expect("load registry");
    let provider = registry
        .providers
        .get_mut(PROVIDER_CODEX)
        .expect("provider entry");
    provider.selected_source_kind = HarnessSourceKind::Subscription;
    provider.selected_endpoint_id = Some(endpoint.id.clone());
    registry::save_registry(root.path(), &registry)
        .await
        .expect("save stray endpoint selection");

    let err = get_provider_source_config(root.path(), PROVIDER_CODEX)
        .await
        .expect_err("stray selected endpoint should fail");
    assert!(err.to_string().contains("configured for subscription"));

    let repaired = registry::load_registry(root.path())
        .await
        .expect("reload registry");
    let provider = repaired.providers.get(PROVIDER_CODEX).expect("provider");
    assert_eq!(
        provider.selected_source_kind,
        HarnessSourceKind::Subscription
    );
    assert_eq!(
        provider.selected_endpoint_id.as_deref(),
        Some(endpoint.id.as_str())
    );
}
