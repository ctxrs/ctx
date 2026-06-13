use super::runtime_resolution::{
    codex_endpoint_home, droid_endpoint_home, legacy_codex_endpoint_home, qwen_endpoint_home,
};
use super::*;

#[tokio::test]
async fn deleting_codex_endpoint_removes_endpoint_home() {
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

    resolve_provider_source_for_probe(root.path(), PROVIDER_CODEX)
        .await
        .expect("resolve probe");

    let endpoint_home = codex_endpoint_home(root.path(), &endpoint.id);
    assert!(endpoint_home.join("auth.json").exists());

    delete_provider_endpoint(root.path(), PROVIDER_CODEX, &endpoint.id)
        .await
        .expect("delete endpoint");

    assert!(!endpoint_home.exists());
}

#[tokio::test]
async fn codex_endpoint_resolution_migrates_legacy_endpoint_home() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_CODEX,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Legacy Codex".to_string(),
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

    let legacy_endpoint_home = legacy_codex_endpoint_home(root.path(), &endpoint.id);
    tokio::fs::create_dir_all(&legacy_endpoint_home)
        .await
        .expect("mkdir legacy endpoint home");
    tokio::fs::write(legacy_endpoint_home.join("legacy.txt"), b"legacy")
        .await
        .expect("write legacy marker");

    set_provider_source_selection(
        root.path(),
        PROVIDER_CODEX,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select");

    resolve_provider_source_for_probe(root.path(), PROVIDER_CODEX)
        .await
        .expect("resolve probe");

    let endpoint_home = codex_endpoint_home(root.path(), &endpoint.id);
    assert!(endpoint_home.exists());
    assert!(endpoint_home.join("legacy.txt").exists());
    assert!(!legacy_endpoint_home.exists());
}

#[tokio::test]
async fn pi_endpoint_uses_openrouter_provider_for_openrouter_base_urls() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_PI,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Pi OpenRouter".to_string(),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("google/gemini-3-flash-preview".to_string()),
            api_key: Some("pi-openrouter-key".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert endpoint");

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
        resolved.env.get("PI_ACP_PROVIDER"),
        Some(&"openrouter".to_string())
    );
    assert_eq!(
        resolved.env.get("OPENAI_BASE_URL"),
        Some(&"https://openrouter.ai/api/v1".to_string())
    );
    assert_eq!(
        resolved.env.get("PI_ACP_MODEL"),
        Some(&"google/gemini-3-flash-preview".to_string())
    );
}

#[tokio::test]
async fn deleting_qwen_endpoint_removes_endpoint_home() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = upsert_provider_endpoint(
        root.path(),
        PROVIDER_QWEN,
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Qwen endpoint".to_string(),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
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
    .expect("upsert");

    set_provider_source_selection(
        root.path(),
        PROVIDER_QWEN,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select");

    resolve_provider_source_for_probe(root.path(), PROVIDER_QWEN)
        .await
        .expect("resolve probe");

    let endpoint_home = qwen_endpoint_home(root.path(), &endpoint.id);
    assert!(endpoint_home.join(".qwen").join("settings.json").exists());

    delete_provider_endpoint(root.path(), PROVIDER_QWEN, &endpoint.id)
        .await
        .expect("delete endpoint");

    assert!(!endpoint_home.exists());
}

#[tokio::test]
async fn deleting_droid_endpoint_removes_endpoint_home() {
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
        PROVIDER_DROID,
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select");

    resolve_provider_source_for_probe(root.path(), PROVIDER_DROID)
        .await
        .expect("resolve probe");

    let endpoint_home = droid_endpoint_home(root.path(), &endpoint.id);
    assert!(endpoint_home
        .join(".factory")
        .join("settings.json")
        .exists());

    delete_provider_endpoint(root.path(), PROVIDER_DROID, &endpoint.id)
        .await
        .expect("delete endpoint");

    assert!(!endpoint_home.exists());
}
