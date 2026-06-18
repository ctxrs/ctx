use super::registry::registry_path;
use super::*;

#[tokio::test]
async fn amp_subscription_sets_persistent_home_env() {
    let root = tempfile::tempdir().expect("tempdir");
    let resolved = resolve_provider_source_for_run(root.path(), PROVIDER_AMP)
        .await
        .expect("resolved");
    assert_eq!(resolved.source_kind, HarnessSourceKind::Subscription);
    assert_eq!(
        resolved.runtime_source_mode(),
        HarnessRuntimeSourceMode::Subscription
    );
    let expected_home = root.path().join("providers").join("amp").join("home");
    assert_eq!(
        resolved.env.get("HOME"),
        Some(&expected_home.to_string_lossy().to_string())
    );
    assert_eq!(
        resolved.env.get("XDG_CONFIG_HOME"),
        Some(&expected_home.join(".config").to_string_lossy().to_string())
    );
    assert_eq!(
        resolved.env.get("XDG_CACHE_HOME"),
        Some(&expected_home.join(".cache").to_string_lossy().to_string())
    );
}

#[tokio::test]
async fn goose_subscription_sets_isolated_path_root() {
    let root = tempfile::tempdir().expect("tempdir");
    let runtime_root = tempfile::tempdir().expect("runtime tempdir");
    let resolved = resolve_provider_source_for_run_with_runtime_root(
        root.path(),
        PROVIDER_GOOSE,
        Some(runtime_root.path()),
    )
    .await
    .expect("resolved");
    assert_eq!(resolved.source_kind, HarnessSourceKind::Subscription);
    let expected_root = runtime_root
        .path()
        .join("providers")
        .join("goose")
        .join("path-root");
    assert_eq!(
        resolved.env.get("GOOSE_PATH_ROOT"),
        Some(&expected_root.to_string_lossy().to_string())
    );
    assert!(
        !resolved.env.contains_key("GOOSE_DISABLE_KEYRING"),
        "subscription flow should preserve native Goose auth storage behavior"
    );
}

#[tokio::test]
async fn goose_endpoint_projects_openrouter_compatible_env() {
    let root = tempfile::tempdir().expect("tempdir");
    let runtime_root = tempfile::tempdir().expect("runtime tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_GOOSE,
        HarnessEndpointUpsert {
            endpoint_id: Some("ep-goose".to_string()),
            name: "Goose OpenRouter".to_string(),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("openai/gpt-5.2-codex".to_string()),
            api_key: Some("goose-key".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert goose endpoint");

    set_provider_source_selection(
        root.path(),
        PROVIDER_GOOSE,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select goose endpoint");

    let resolved = resolve_provider_source_for_run_with_runtime_root(
        root.path(),
        PROVIDER_GOOSE,
        Some(runtime_root.path()),
    )
    .await
    .expect("resolve goose endpoint");
    assert_eq!(resolved.source_kind, HarnessSourceKind::Endpoint);
    assert_eq!(
        resolved.env.get("OPENROUTER_API_KEY").map(String::as_str),
        Some("goose-key")
    );
    assert_eq!(
        resolved.env.get("GOOSE_PROVIDER").map(String::as_str),
        Some("openrouter")
    );
    assert_eq!(
        resolved
            .env
            .get("GOOSE_DISABLE_KEYRING")
            .map(String::as_str),
        Some("1")
    );
    assert_eq!(
        resolved.env.get("GOOSE_MODEL").map(String::as_str),
        Some("openai/gpt-5.2-codex")
    );
    assert_eq!(
        resolved.env.get("GOOSE_MODE").map(String::as_str),
        Some("auto")
    );
    assert_eq!(
        resolved.env.get("CTX_PROVIDER_MODE").map(String::as_str),
        Some("")
    );
    assert!(!resolved.env.contains_key("OPENAI_API_KEY"));
    assert!(!resolved.env.contains_key("OPENAI_BASE_URL"));
    assert!(!resolved.env.contains_key("OPENAI_HOST"));
    assert!(!resolved.env.contains_key("OPENAI_MODEL"));
    assert!(!resolved.env.contains_key("OPENROUTER_BASE_URL"));
    assert!(!resolved.env.contains_key("OPENROUTER_HOST"));
    assert!(!resolved.env.contains_key("OPENROUTER_MODEL"));
    assert_eq!(
        resolved.env.get("GOOSE_PATH_ROOT"),
        Some(
            &runtime_resolution::goose_endpoint_path_root(runtime_root.path(), "ep-goose")
                .to_string_lossy()
                .to_string()
        )
    );
    let config_path = runtime_resolution::goose_endpoint_path_root(runtime_root.path(), "ep-goose")
        .join("config")
        .join("config.yaml");
    let config = tokio::fs::read_to_string(&config_path)
        .await
        .expect("read goose config");
    assert!(config.contains("developer:\n    enabled: true"));
    assert!(config.contains("analyze:\n    enabled: false"));
    assert!(config.contains("extensionmanager:\n    enabled: false"));
    assert!(config.contains("apps:\n    enabled: false"));
}

#[test]
fn goose_endpoint_path_root_scopes_endpoint_state_under_runtime_root() {
    let runtime_root = tempfile::tempdir().expect("runtime tempdir");
    let expected_root = runtime_root
        .path()
        .join("providers")
        .join("goose")
        .join("endpoint-path-roots")
        .join("ep-goose");

    assert_eq!(
        runtime_resolution::goose_endpoint_path_root(runtime_root.path(), "ep-goose"),
        expected_root
    );
}

#[tokio::test]
async fn goose_endpoint_rejects_non_openrouter_base_url() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_GOOSE,
        HarnessEndpointUpsert {
            endpoint_id: Some("ep-goose-custom".to_string()),
            name: "Goose Custom".to_string(),
            base_url: Some("https://example.test/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("openai/gpt-5.2-codex".to_string()),
            api_key: Some("goose-key".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert goose endpoint");

    set_provider_source_selection(
        root.path(),
        PROVIDER_GOOSE,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select goose endpoint");

    let err = resolve_provider_source_for_run(root.path(), PROVIDER_GOOSE)
        .await
        .expect_err("non-openrouter Goose endpoint should fail");
    assert!(
        err.to_string()
            .contains("goose harness endpoints currently require an OpenRouter base_url"),
        "unexpected error: {err:#}"
    );
}

#[tokio::test]
async fn openhands_endpoint_projects_persistence_backed_llm_contract() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_OPENHANDS,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "OpenHands OpenRouter".to_string(),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("openai/gpt-5.2".to_string()),
            api_key: Some("openhands-key".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert openhands endpoint");

    set_provider_source_selection(
        root.path(),
        PROVIDER_OPENHANDS,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select openhands endpoint");

    let resolved = resolve_provider_source_for_run(root.path(), PROVIDER_OPENHANDS)
        .await
        .expect("resolve openhands endpoint");
    assert_eq!(resolved.source_kind, HarnessSourceKind::Endpoint);
    assert_eq!(
        resolved.env.get("LLM_API_KEY").map(String::as_str),
        Some("openhands-key")
    );
    assert_eq!(
        resolved.env.get("LLM_BASE_URL").map(String::as_str),
        Some("https://openrouter.ai/api/v1")
    );
    assert_eq!(
        resolved.env.get("LLM_MODEL").map(String::as_str),
        Some("openrouter/openai/gpt-5.2")
    );
    assert_eq!(
        resolved
            .env
            .get("CTX_CRP_DISABLE_MODEL_OVERRIDE")
            .map(String::as_str),
        Some("1")
    );
    assert_eq!(
        resolved.env.get("CTX_PROVIDER_MODE").map(String::as_str),
        Some("always-approve")
    );
    assert!(!resolved.env.contains_key("OPENAI_API_KEY"));
    assert!(!resolved.env.contains_key("OPENAI_BASE_URL"));
    assert!(!resolved.env.contains_key("OPENAI_MODEL"));
    assert_eq!(
        resolved.env.get("OPENHANDS_PERSISTENCE_DIR"),
        Some(
            &runtime_resolution::openhands_endpoint_home(root.path(), &endpoint.id)
                .to_string_lossy()
                .to_string()
        )
    );
    let pyhooks_dir =
        runtime_resolution::openhands_endpoint_home(root.path(), &endpoint.id).join("pyhooks");
    let mut pythonpath_parts = vec![pyhooks_dir.as_os_str().to_os_string()];
    if let Some(existing) = std::env::var_os("PYTHONPATH") {
        pythonpath_parts.extend(std::env::split_paths(&existing).map(|path| path.into_os_string()));
    }
    let expected_pythonpath = std::env::join_paths(pythonpath_parts)
        .expect("join expected PYTHONPATH")
        .to_string_lossy()
        .to_string();
    assert_eq!(resolved.env.get("PYTHONPATH"), Some(&expected_pythonpath));
    let agent_settings: serde_json::Value = serde_json::from_str(
        &tokio::fs::read_to_string(
            runtime_resolution::openhands_endpoint_home(root.path(), &endpoint.id)
                .join("agent_settings.json"),
        )
        .await
        .expect("read agent_settings.json"),
    )
    .expect("parse agent_settings.json");
    assert_eq!(
        agent_settings,
        serde_json::json!({
            "llm": {
                "model": "openrouter/openai/gpt-5.2",
                "api_key": "openhands-key",
                "base_url": "https://openrouter.ai/api/v1",
                "usage_id": "agent",
            },
            "tools": [
                { "name": "file_editor", "params": {} },
            ],
            "mcp_config": {},
            "kind": "Agent",
        })
    );
    let sitecustomize = tokio::fs::read_to_string(pyhooks_dir.join("sitecustomize.py"))
        .await
        .expect("read sitecustomize.py");
    assert!(sitecustomize.contains("AgentStore._resolve_tools = _ctx_resolve_tools"));
    assert!(sitecustomize.contains("agent is not None and agent.tools"));
}

#[tokio::test]
async fn refresh_provider_endpoint_model_catalog_returns_error_state_for_unsupported_discovery() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_GEMINI,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Gemini Native".to_string(),
            base_url: None,
            api_shape: None,
            auth_type: Some(GEMINI_AUTH_TYPE_GEMINI_API_KEY.to_string()),
            model_override: None,
            api_key: Some("gemini-key".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert endpoint");

    let refreshed =
        refresh_provider_endpoint_model_catalog(root.path(), PROVIDER_GEMINI, &endpoint.id)
            .await
            .expect("refresh should not fail");

    assert_eq!(
        refreshed.model_catalog_status,
        EndpointModelCatalogStatus::Error
    );
    assert!(refreshed
        .model_catalog_error
        .as_deref()
        .unwrap_or_default()
        .contains("model discovery is unsupported"));
}

#[tokio::test]
async fn invalid_registry_json_returns_error() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = registry_path(root.path());
    tokio::fs::create_dir_all(path.parent().expect("parent"))
        .await
        .expect("mkdir");
    tokio::fs::write(&path, b"{not-json")
        .await
        .expect("write invalid");

    let err = get_provider_source_config(root.path(), PROVIDER_CODEX)
        .await
        .expect_err("invalid registry should fail");
    assert!(err.to_string().contains("parsing harness source registry"));
}
