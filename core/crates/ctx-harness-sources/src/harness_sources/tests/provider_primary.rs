use super::runtime_resolution::{cline_endpoint_home, gemini_endpoint_home};
use super::*;

#[tokio::test]
async fn codex_endpoint_requires_verify_for_run_resolution() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_CODEX,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "OpenRouter".to_string(),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: None,
            api_key: Some("sk-test".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert");

    set_provider_source_selection(
        root.path(),
        PROVIDER_CODEX,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select");

    let err = resolve_provider_source_for_run(root.path(), PROVIDER_CODEX)
        .await
        .expect_err("expected verify gate error");
    assert!(err.to_string().contains("not verified"));

    mark_endpoint_verification(
        root.path(),
        PROVIDER_CODEX,
        &endpoint.id,
        HarnessEndpointVerificationStatus::Valid,
        None,
    )
    .await
    .expect("mark verified");

    let resolved = resolve_provider_source_for_run(root.path(), PROVIDER_CODEX)
        .await
        .expect("resolve run");
    assert_eq!(resolved.source_kind, HarnessSourceKind::Endpoint);
    assert_eq!(
        resolved.runtime_source_mode(),
        HarnessRuntimeSourceMode::Endpoint(HarnessRouteBackend::UserManaged)
    );
    assert_eq!(
        resolved
            .endpoint
            .as_ref()
            .map(|endpoint| endpoint.provider_id.as_str()),
        Some(PROVIDER_CODEX)
    );
    assert!(resolved.env.contains_key("CODEX_HOME"));
    assert_eq!(
        resolved.env.get("OPENAI_BASE_URL"),
        Some(&"https://openrouter.ai/api/v1".to_string())
    );
    assert_eq!(
        resolved.env.get("CTX_MODEL_PROVIDER"),
        Some(&"openrouter".to_string())
    );
}

#[tokio::test]
async fn codex_ctx_managed_relay_endpoint_keeps_provider_id_and_marks_runtime_backend() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_CODEX,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "ctx relay".to_string(),
            base_url: Some("https://api.ctx.rs/relay/openai/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("gpt-5.4".to_string()),
            api_key: Some("relay-test-token".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert");

    set_provider_source_selection(
        root.path(),
        PROVIDER_CODEX,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select");
    mark_endpoint_verification(
        root.path(),
        PROVIDER_CODEX,
        &endpoint.id,
        HarnessEndpointVerificationStatus::Valid,
        None,
    )
    .await
    .expect("mark verified");

    let resolved = resolve_provider_source_for_run(root.path(), PROVIDER_CODEX)
        .await
        .expect("resolve run");
    assert_eq!(resolved.source_kind, HarnessSourceKind::Endpoint);
    assert_eq!(
        resolved.runtime_source_mode(),
        HarnessRuntimeSourceMode::Endpoint(HarnessRouteBackend::CtxManagedRelay)
    );
    assert_eq!(
        resolved
            .endpoint
            .as_ref()
            .map(|endpoint| endpoint.provider_id.as_str()),
        Some(PROVIDER_CODEX)
    );
    assert_eq!(
        resolved
            .env
            .get(CTX_PROVIDER_ROUTE_BACKEND_ENV)
            .map(String::as_str),
        Some("ctx_managed")
    );
    assert_eq!(
        resolved.env.get("OPENAI_BASE_URL").map(String::as_str),
        None
    );
    assert_eq!(resolved.env.get("OPENAI_API_KEY").map(String::as_str), None);
    assert_eq!(resolved.env.get("CODEX_HOME").map(String::as_str), None);
    assert_eq!(
        resolved
            .env
            .get(CTX_LLM_RELAY_BASE_URL_ENV)
            .map(String::as_str),
        Some("https://api.ctx.rs/relay/openai/v1")
    );
    assert_eq!(
        resolved
            .env
            .get(CTX_LLM_RELAY_MODEL_ENV)
            .map(String::as_str),
        Some("gpt-5.4")
    );
}

#[tokio::test]
async fn codex_ctx_managed_relay_endpoint_requires_model_id() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_CODEX,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "ctx relay".to_string(),
            base_url: Some("https://api.ctx.rs/relay/openai/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: None,
            api_key: Some("relay-test-token".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert");

    set_provider_source_selection(
        root.path(),
        PROVIDER_CODEX,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select");
    mark_endpoint_verification(
        root.path(),
        PROVIDER_CODEX,
        &endpoint.id,
        HarnessEndpointVerificationStatus::Valid,
        None,
    )
    .await
    .expect("mark verified");

    let err = resolve_provider_source_for_run(root.path(), PROVIDER_CODEX)
        .await
        .expect_err("ctx-managed relay should require a model id");
    assert!(err.to_string().contains("missing a concrete model id"));
}

#[tokio::test]
async fn cline_endpoint_projects_isolated_config_and_env() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_CLINE,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "OpenRouter".to_string(),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("openai/gpt-5.2-codex".to_string()),
            api_key: Some("sk-cline".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert");

    set_provider_source_selection(
        root.path(),
        PROVIDER_CLINE,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select");

    let runtime_root = tempfile::tempdir().expect("runtime tempdir");
    let resolved = resolve_provider_source_for_run_with_runtime_root(
        root.path(),
        PROVIDER_CLINE,
        Some(runtime_root.path()),
    )
    .await
    .expect("resolve run");

    assert_eq!(resolved.source_kind, HarnessSourceKind::Endpoint);
    let cline_dir = cline_endpoint_home(runtime_root.path(), &endpoint.id);
    assert_eq!(
        resolved.env.get("CLINE_DIR"),
        Some(&cline_dir.to_string_lossy().to_string())
    );
    assert_eq!(
        resolved.env.get("CLINE_NO_AUTO_UPDATE"),
        Some(&"1".to_string())
    );
    assert_eq!(
        resolved.env.get("OPENAI_MODEL"),
        Some(&"openai/gpt-5.2-codex".to_string())
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
        Some("act")
    );
    assert!(!resolved.env.contains_key("OPENAI_API_KEY"));
    assert!(!resolved.env.contains_key("OPENAI_BASE_URL"));

    let global_state: serde_json::Value = serde_json::from_str(
        &tokio::fs::read_to_string(cline_dir.join("data/globalState.json"))
            .await
            .expect("read globalState.json"),
    )
    .expect("parse globalState.json");
    assert_eq!(
        global_state,
        serde_json::json!({
            "actModeApiProvider": "openrouter",
            "planModeApiProvider": "openrouter",
            "actModeOpenRouterModelId": "openai/gpt-5.2-codex",
            "planModeOpenRouterModelId": "openai/gpt-5.2-codex",
            "welcomeViewCompleted": true,
        })
    );

    let secrets: serde_json::Value = serde_json::from_str(
        &tokio::fs::read_to_string(cline_dir.join("data/secrets.json"))
            .await
            .expect("read secrets.json"),
    )
    .expect("parse secrets.json");
    assert_eq!(
        secrets,
        serde_json::json!({
            "openRouterApiKey": "sk-cline",
        })
    );

    let mcp_settings: serde_json::Value = serde_json::from_str(
        &tokio::fs::read_to_string(cline_dir.join("data/settings/cline_mcp_settings.json"))
            .await
            .expect("read cline_mcp_settings.json"),
    )
    .expect("parse cline_mcp_settings.json");
    assert_eq!(mcp_settings, serde_json::json!({ "mcpServers": {} }));
}

#[tokio::test]
async fn claude_endpoint_projects_anthropic_env_and_requires_verify_for_run() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_CLAUDE,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Anthropic".to_string(),
            base_url: Some("https://api.anthropic.com/v1".to_string()),
            api_shape: Some(HarnessApiShape::AnthropicMessages),
            auth_type: None,
            model_override: None,
            api_key: Some("sk-ant-api".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert");

    set_provider_source_selection(
        root.path(),
        PROVIDER_CLAUDE,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select");

    let probe = resolve_provider_source_for_probe(root.path(), PROVIDER_CLAUDE)
        .await
        .expect("resolve probe");
    assert_eq!(
        probe.env.get("ANTHROPIC_API_KEY"),
        Some(&"sk-ant-api".to_string())
    );
    assert_eq!(
        probe.env.get("ANTHROPIC_BASE_URL"),
        Some(&"https://api.anthropic.com".to_string())
    );

    let run_err = resolve_provider_source_for_run(root.path(), PROVIDER_CLAUDE)
        .await
        .expect_err("expected verify gate error");
    assert!(run_err.to_string().contains("not verified"));

    mark_endpoint_verification(
        root.path(),
        PROVIDER_CLAUDE,
        &endpoint.id,
        HarnessEndpointVerificationStatus::Valid,
        None,
    )
    .await
    .expect("mark verified");

    let run = resolve_provider_source_for_run(root.path(), PROVIDER_CLAUDE)
        .await
        .expect("resolve run");
    assert_eq!(
        run.env.get("ANTHROPIC_BASE_URL"),
        Some(&"https://api.anthropic.com".to_string())
    );
}

#[tokio::test]
async fn claude_endpoint_openrouter_v1_base_url_is_normalized_for_anthropic_shape() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_CLAUDE,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "OpenRouter".to_string(),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            api_shape: Some(HarnessApiShape::AnthropicMessages),
            auth_type: None,
            model_override: Some("anthropic/claude-opus-4.6".to_string()),
            api_key: Some("sk-or-v1".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert");

    assert_eq!(
        endpoint.base_url,
        Some("https://openrouter.ai/api".to_string())
    );

    set_provider_source_selection(
        root.path(),
        PROVIDER_CLAUDE,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select");

    mark_endpoint_verification(
        root.path(),
        PROVIDER_CLAUDE,
        &endpoint.id,
        HarnessEndpointVerificationStatus::Valid,
        None,
    )
    .await
    .expect("mark verified");

    let resolved = resolve_provider_source_for_run(root.path(), PROVIDER_CLAUDE)
        .await
        .expect("resolve run");
    assert_eq!(
        resolved.env.get("ANTHROPIC_BASE_URL"),
        Some(&"https://openrouter.ai/api".to_string())
    );
}

#[tokio::test]
async fn gemini_endpoint_does_not_require_verify_for_run_resolution() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_GEMINI,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Gemini Key".to_string(),
            base_url: None,
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: Some(GEMINI_AUTH_TYPE_GEMINI_API_KEY.to_string()),
            model_override: None,
            api_key: Some("gemini-key".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert");

    set_provider_source_selection(
        root.path(),
        PROVIDER_GEMINI,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select");

    let resolved = resolve_provider_source_for_run(root.path(), PROVIDER_GEMINI)
        .await
        .expect("resolve run");
    assert_eq!(resolved.source_kind, HarnessSourceKind::Endpoint);
    assert_eq!(endpoint.base_url, None);
    assert_eq!(
        resolved.env.get("GEMINI_API_KEY"),
        Some(&"gemini-key".to_string())
    );
    assert_eq!(resolved.env.get("GOOGLE_API_KEY"), Some(&String::new()));
    let home = PathBuf::from(
        resolved
            .env
            .get("HOME")
            .expect("HOME should be set for gemini endpoint"),
    );
    assert!(home.starts_with(root.path()));
    assert_eq!(
        resolved.env.get("GEMINI_CLI_HOME"),
        Some(&home.to_string_lossy().to_string())
    );
    assert_eq!(
        resolved.env.get("GEMINI_FORCE_FILE_STORAGE"),
        Some(&"true".to_string())
    );
    assert!(home.join(".gemini").exists());
    let settings = tokio::fs::read_to_string(
        gemini_endpoint_home(root.path(), &endpoint.id).join(".gemini/settings.json"),
    )
    .await
    .expect("read gemini settings");
    assert!(settings.contains("\"selectedType\": \"gemini-api-key\""));
    assert!(!resolved.env.contains_key("OPENAI_API_KEY"));
    assert!(!resolved.env.contains_key("OPENAI_BASE_URL"));
}

#[tokio::test]
async fn gemini_vertex_endpoint_projects_vertex_env_for_run_resolution() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_GEMINI,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Gemini Vertex".to_string(),
            base_url: None,
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: Some(GEMINI_AUTH_TYPE_VERTEX_AI.to_string()),
            model_override: None,
            api_key: None,
            service_account_json: Some(
                r#"{"type":"service_account","project_id":"vertex-project","private_key_id":"key-id","private_key":"-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\n","client_email":"ctx-vertex@test.iam.gserviceaccount.com","client_id":"1234567890"}"#
                    .to_string(),
            ),
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert");

    set_provider_source_selection(
        root.path(),
        PROVIDER_GEMINI,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select");

    let resolved = resolve_provider_source_for_run(root.path(), PROVIDER_GEMINI)
        .await
        .expect("resolve run");
    assert_eq!(resolved.source_kind, HarnessSourceKind::Endpoint);
    assert_eq!(
        resolved.env.get("GOOGLE_APPLICATION_CREDENTIALS"),
        Some(
            &gemini_endpoint_home(root.path(), &endpoint.id)
                .join(".gemini/vertex-service-account.json")
                .to_string_lossy()
                .to_string(),
        )
    );
    assert_eq!(
        resolved.env.get("GOOGLE_CLOUD_PROJECT"),
        Some(&"vertex-project".to_string())
    );
    assert_eq!(
        resolved.env.get("GOOGLE_CLOUD_PROJECT_ID"),
        Some(&"vertex-project".to_string())
    );
    assert_eq!(
        resolved.env.get("GOOGLE_CLOUD_LOCATION"),
        Some(&"global".to_string())
    );
    assert_eq!(
        resolved.env.get("GOOGLE_GENAI_USE_VERTEXAI"),
        Some(&"true".to_string())
    );
    assert_eq!(resolved.env.get("GEMINI_API_KEY"), Some(&String::new()));
    assert_eq!(resolved.env.get("GOOGLE_API_KEY"), Some(&String::new()));
    assert!(resolved.env.contains_key("HOME"));
    assert!(resolved.env.contains_key("GEMINI_CLI_HOME"));
    let settings = tokio::fs::read_to_string(
        gemini_endpoint_home(root.path(), &endpoint.id).join(".gemini/settings.json"),
    )
    .await
    .expect("read gemini settings");
    assert!(settings.contains("\"selectedType\": \"vertex-ai\""));
    let credentials = tokio::fs::read_to_string(
        gemini_endpoint_home(root.path(), &endpoint.id).join(".gemini/vertex-service-account.json"),
    )
    .await
    .expect("read vertex credentials");
    assert!(credentials.contains("\"project_id\":\"vertex-project\""));
    assert!(!resolved.env.contains_key("OPENAI_API_KEY"));
    assert!(!resolved.env.contains_key("OPENAI_BASE_URL"));
}

#[tokio::test]
async fn gemini_endpoint_rejects_custom_base_url() {
    let root = tempfile::tempdir().expect("tempdir");
    let err = upsert_provider_endpoint(
        root.path(),
        PROVIDER_GEMINI,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Gemini OpenAI".to_string(),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: Some(GEMINI_AUTH_TYPE_GEMINI_API_KEY.to_string()),
            model_override: Some("openai/gpt-5.2".to_string()),
            api_key: Some("openrouter-key".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect_err("upsert should fail");
    assert!(err
        .to_string()
        .contains("does not support custom endpoint base_url"));
}

#[tokio::test]
async fn gemini_endpoint_rejects_bearer_auth_type() {
    let root = tempfile::tempdir().expect("tempdir");
    let err = upsert_provider_endpoint(
        root.path(),
        PROVIDER_GEMINI,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Gemini Bearer".to_string(),
            base_url: None,
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: Some(CODEX_AUTH_TYPE_BEARER.to_string()),
            model_override: None,
            api_key: Some("key".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect_err("upsert should fail");
    assert!(err
        .to_string()
        .contains("auth_type 'bearer' is not supported"));
}
