use super::runtime_resolution::{droid_endpoint_home, seed_droid_auth_from_host_path};
use super::*;

#[tokio::test]
async fn gemini_endpoint_rejects_unknown_auth_type() {
    let root = tempfile::tempdir().expect("tempdir");
    let err = upsert_provider_endpoint(
        root.path(),
        PROVIDER_GEMINI,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Gemini Invalid".to_string(),
            base_url: None,
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: Some("invalid".to_string()),
            model_override: None,
            api_key: Some("gemini-key".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect_err("upsert should fail");
    assert!(err
        .to_string()
        .contains("auth_type 'invalid' is not supported"));
}

#[tokio::test]
async fn kimi_endpoint_projects_env_for_run_resolution() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_KIMI,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Kimi Key".to_string(),
            base_url: Some("https://api.moonshot.ai/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("kimi-k2".to_string()),
            api_key: Some("kimi-key".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert");

    set_provider_source_selection(
        root.path(),
        PROVIDER_KIMI,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select");

    let resolved = resolve_provider_source_for_run(root.path(), PROVIDER_KIMI)
        .await
        .expect("resolve run");
    assert_eq!(resolved.source_kind, HarnessSourceKind::Endpoint);
    assert_eq!(
        resolved.env.get("KIMI_BASE_URL").map(String::as_str),
        Some("https://api.moonshot.ai/v1")
    );
    assert_eq!(
        resolved.env.get("OPENAI_BASE_URL").map(String::as_str),
        Some("https://api.moonshot.ai/v1")
    );
    assert_eq!(
        resolved.env.get("KIMI_API_KEY").map(String::as_str),
        Some("kimi-key")
    );
    assert_eq!(
        resolved.env.get("OPENAI_API_KEY").map(String::as_str),
        Some("kimi-key")
    );
    assert_eq!(
        resolved.env.get("KIMI_MODEL_NAME").map(String::as_str),
        Some("kimi-k2")
    );
    assert_eq!(
        resolved.env.get("OPENAI_MODEL").map(String::as_str),
        Some("kimi-k2")
    );
    assert_eq!(
        resolved
            .env
            .get("CTX_CRP_DISABLE_MODEL_OVERRIDE")
            .map(String::as_str),
        Some("1")
    );
    let kimi_share_dir = resolved
        .env
        .get("KIMI_SHARE_DIR")
        .expect("KIMI_SHARE_DIR should be set");
    let kimi_share_path = PathBuf::from(kimi_share_dir);
    assert!(kimi_share_path.starts_with(root.path()));
    let token_path = kimi_share_path.join("credentials").join("kimi-code.json");
    let token = tokio::fs::read_to_string(&token_path)
        .await
        .expect("read seeded kimi endpoint token");
    let parsed: serde_json::Value = serde_json::from_str(&token).expect("parse kimi token json");
    assert_eq!(
        parsed
            .get("access_token")
            .and_then(serde_json::Value::as_str),
        Some("ctx-endpoint-access-token")
    );
}

#[tokio::test]
async fn qwen_model_override_is_opaque_and_opencode_uses_endpoint_namespace() {
    let root = tempfile::tempdir().expect("tempdir");
    let base_url = "https://api.myawesomeprovider.example/v1";
    let model_override = "openai/gpt-5.2-codex";

    let qwen_endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_QWEN,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Qwen custom".to_string(),
            base_url: Some(base_url.to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some(model_override.to_string()),
            api_key: Some("qwen-key".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert qwen endpoint");
    set_provider_source_selection(
        root.path(),
        PROVIDER_QWEN,
        HarnessSourceKind::Endpoint,
        Some(qwen_endpoint.id.clone()),
    )
    .await
    .expect("select qwen endpoint");
    let qwen_resolved = resolve_provider_source_for_run(root.path(), PROVIDER_QWEN)
        .await
        .expect("resolve qwen");
    assert_eq!(
        qwen_resolved.env.get("OPENAI_MODEL"),
        Some(&"openai/gpt-5.2-codex".to_string())
    );
    let qwen_home = PathBuf::from(
        qwen_resolved
            .env
            .get("HOME")
            .expect("HOME should be set for qwen endpoint"),
    );
    let qwen_settings = tokio::fs::read_to_string(qwen_home.join(".qwen").join("settings.json"))
        .await
        .expect("read qwen settings");
    assert!(qwen_settings.contains("\"selectedType\": \"openai\""));

    let opencode_endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_OPENCODE,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "OpenCode custom".to_string(),
            base_url: Some(base_url.to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some(model_override.to_string()),
            api_key: Some("opencode-key".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert opencode endpoint");
    set_provider_source_selection(
        root.path(),
        PROVIDER_OPENCODE,
        HarnessSourceKind::Endpoint,
        Some(opencode_endpoint.id.clone()),
    )
    .await
    .expect("select opencode endpoint");
    let opencode_resolved = resolve_provider_source_for_run(root.path(), PROVIDER_OPENCODE)
        .await
        .expect("resolve opencode");
    let config = opencode_resolved
        .env
        .get("OPENCODE_CONFIG_CONTENT")
        .expect("opencode config env");
    let parsed: serde_json::Value = serde_json::from_str(config).expect("valid json");
    assert_eq!(
        parsed.get("model").and_then(serde_json::Value::as_str),
        Some("myawesomeprovider/openai/gpt-5.2-codex")
    );
    assert_eq!(
        parsed
            .get("permission")
            .and_then(|permission| permission.get("edit"))
            .and_then(serde_json::Value::as_str),
        Some("deny")
    );
    assert_eq!(
        parsed
            .get("permission")
            .and_then(|permission| permission.get("bash"))
            .and_then(serde_json::Value::as_str),
        Some("allow")
    );
    assert!(
        parsed
            .get("provider")
            .and_then(|provider| provider.get("myawesomeprovider"))
            .is_some(),
        "expected namespaced provider config key"
    );
    assert!(
        !opencode_resolved.env.contains_key("OPENROUTER_API_KEY"),
        "custom namespace should not force OPENROUTER_* compatibility env vars"
    );
}

#[tokio::test]
async fn additional_provider_endpoint_env_projection_smoke() {
    let root = tempfile::tempdir().expect("tempdir");
    let cases: &[(&str, &[&str])] = &[
        (
            PROVIDER_QWEN,
            &["OPENAI_API_KEY", "OPENAI_BASE_URL", "HOME"],
        ),
        (
            PROVIDER_OPENCODE,
            &[
                "OPENAI_API_KEY",
                "OPENROUTER_API_KEY",
                "OPENCODE_CONFIG_CONTENT",
            ],
        ),
        (PROVIDER_MISTRAL, &["MISTRAL_API_KEY", "MISTRAL_BASE_URL"]),
        (
            PROVIDER_DROID,
            &[
                "HOME",
                "OPENAI_API_KEY",
                "OPENAI_BASE_URL",
                "DROID_DEFAULT_MODEL",
            ],
        ),
        (PROVIDER_COPILOT, &["GH_TOKEN", "GITHUB_TOKEN"]),
        (
            PROVIDER_PI,
            &["OPENAI_API_KEY", "PI_ACP_PROVIDER", "PI_ACP_MODEL"],
        ),
    ];

    for (provider_id, required_keys) in cases {
        let endpoint = upsert_provider_endpoint(
            root.path(),
            provider_id,
            HarnessEndpointUpsert {
                endpoint_id: None,
                name: format!("{provider_id} endpoint"),
                base_url: if *provider_id == PROVIDER_COPILOT || *provider_id == PROVIDER_PI {
                    None
                } else {
                    Some("https://openrouter.ai/api/v1".to_string())
                },
                api_shape: if *provider_id == PROVIDER_COPILOT || *provider_id == PROVIDER_PI {
                    None
                } else {
                    Some(HarnessApiShape::OpenaiResponses)
                },
                auth_type: None,
                model_override: Some("test-model".to_string()),
                api_key: Some("test-key".to_string()),
                service_account_json: None,
                project_id: None,
                location: None,
            },
        )
        .await
        .expect("upsert endpoint");

        set_provider_source_selection(
            root.path(),
            provider_id,
            HarnessSourceKind::Endpoint,
            Some(endpoint.id.clone()),
        )
        .await
        .expect("select endpoint");

        let resolved = resolve_provider_source_for_run(root.path(), provider_id)
            .await
            .expect("resolve run");
        for key in *required_keys {
            assert!(
                resolved.env.contains_key(*key),
                "{provider_id} missing env key {key}"
            );
        }
    }
}

#[tokio::test]
async fn droid_endpoint_writes_factory_settings_for_generic_endpoint() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_DROID,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Droid endpoint".to_string(),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("openai/gpt-5.2-codex".to_string()),
            api_key: Some("sk-or-test".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert endpoint");
    set_provider_source_selection(
        root.path(),
        PROVIDER_DROID,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select endpoint");

    let resolved = resolve_provider_source_for_run(root.path(), PROVIDER_DROID)
        .await
        .expect("resolve run");
    let home = PathBuf::from(
        resolved
            .env
            .get("HOME")
            .expect("HOME should be set for droid endpoint"),
    );
    let settings = tokio::fs::read_to_string(home.join(".factory").join("settings.json"))
        .await
        .expect("read droid settings");
    assert!(settings.contains("\"provider\": \"generic-chat-completion-api\""));
    assert!(settings.contains("\"baseUrl\": \"https://openrouter.ai/api/v1\""));
    assert!(settings.contains("\"model\": \"openai/gpt-5.2-codex\""));
    assert!(settings.contains("\"displayName\": \"openai/gpt-5.2-codex [openrouter]\""));
    assert!(!resolved.env.contains_key("FACTORY_API_KEY"));
    assert_eq!(
        resolved.env.get("DROID_DEFAULT_MODEL"),
        Some(&"custom:openai/gpt-5.2-codex-[openrouter]-0".to_string())
    );
}

#[tokio::test]
async fn droid_endpoint_requires_explicit_model() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_DROID,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Droid endpoint".to_string(),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: None,
            api_key: Some("sk-or-test".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert endpoint");
    set_provider_source_selection(
        root.path(),
        PROVIDER_DROID,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select endpoint");

    let err = resolve_provider_source_for_run(root.path(), PROVIDER_DROID)
        .await
        .expect_err("droid endpoint should require explicit model");
    assert!(err.to_string().contains("missing a concrete model id"));
}

#[tokio::test]
async fn seed_droid_auth_from_host_path_copies_auth_encrypted_into_endpoint_home() {
    let root = tempfile::tempdir().expect("tempdir");
    let host = tempfile::tempdir().expect("tempdir");
    let host_auth_path = host.path().join("auth.encrypted");
    tokio::fs::write(&host_auth_path, b"seeded-droid-auth")
        .await
        .expect("write host auth");

    let endpoint_home = droid_endpoint_home(root.path(), "ep-1");
    let changed = seed_droid_auth_from_host_path(&endpoint_home, &host_auth_path)
        .await
        .expect("seed auth");

    assert!(changed);
    let copied = tokio::fs::read(endpoint_home.join(".factory").join("auth.encrypted"))
        .await
        .expect("read copied auth");
    assert_eq!(copied, b"seeded-droid-auth");
}

#[tokio::test]
async fn droid_endpoint_requires_base_url() {
    let root = tempfile::tempdir().expect("tempdir");
    let err = upsert_provider_endpoint(
        root.path(),
        PROVIDER_DROID,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "droid endpoint".to_string(),
            base_url: None,
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("openai/gpt-5.2-codex".to_string()),
            api_key: Some("sk-test".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect_err("base_url should be required");
    assert!(
        err.to_string().contains("base_url is required"),
        "droid should reject missing base_url: {err}"
    );
}

#[tokio::test]
async fn copilot_endpoint_allows_token_only_upsert() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_COPILOT,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Copilot token".to_string(),
            base_url: None,
            api_shape: None,
            auth_type: None,
            model_override: None,
            api_key: Some("ghp_test".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert endpoint");

    assert!(endpoint.base_url.is_none());
    assert_eq!(endpoint.api_shape, HarnessApiShape::OpenaiResponses);

    let cfg = get_provider_source_config(root.path(), PROVIDER_COPILOT)
        .await
        .expect("get source config");
    let stored = cfg
        .endpoints
        .iter()
        .find(|candidate| candidate.id == endpoint.id)
        .expect("stored endpoint");
    assert!(stored.base_url.is_none());
    assert_eq!(stored.api_shape, HarnessApiShape::OpenaiResponses);
}

#[tokio::test]
async fn pi_endpoint_allows_token_only_upsert_and_optional_base_url() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_PI,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Pi token".to_string(),
            base_url: None,
            api_shape: None,
            auth_type: None,
            model_override: Some("gpt-5".to_string()),
            api_key: Some("pi-key".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert endpoint");

    assert!(endpoint.base_url.is_none());
    assert_eq!(endpoint.api_shape, HarnessApiShape::OpenaiResponses);

    set_provider_source_selection(
        root.path(),
        PROVIDER_PI,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select endpoint");

    let resolved = resolve_provider_source_for_run(root.path(), PROVIDER_PI)
        .await
        .expect("resolve run");
    assert_eq!(
        resolved.env.get("OPENAI_API_KEY"),
        Some(&"pi-key".to_string())
    );
    assert_eq!(
        resolved.env.get("PI_ACP_PROVIDER"),
        Some(&"openai".to_string())
    );
    assert_eq!(resolved.env.get("PI_ACP_MODEL"), Some(&"gpt-5".to_string()));
    assert!(!resolved.env.contains_key("OPENAI_BASE_URL"));
}
