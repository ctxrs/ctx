use super::*;
use ctx_providers::adapters::{ProviderHealth, ProviderUsability};

fn write_invalid_agent_server_config(data_root: &std::path::Path) {
    let path = data_root
        .join("providers")
        .join("agent-servers")
        .join("agent_servers.json");
    std::fs::create_dir_all(path.parent().expect("agent server config parent")).unwrap();
    std::fs::write(path, "{ not valid json").unwrap();
}

#[tokio::test]
async fn diagnostics_marks_provider_statuses_with_agent_server_config_errors() {
    let _serial = home_env_test_lock().lock().await;
    let _build_identity = EnvVarGuard::unset(ctx_update_service::BUILD_IDENTITY_PATH_ENV);
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    write_invalid_agent_server_config(data_dir.path());
    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let state = fixture.daemon();
    state
        .upsert_provider_status(
            "qwen".to_string(),
            ProviderStatus {
                provider_id: "qwen".to_string(),
                installed: true,
                detected_path: None,
                version: Some("0.1.0".to_string()),
                capabilities: None,
                health: ProviderHealth::Ok,
                diagnostics: Vec::new(),
                details: HashMap::new(),
                usability: ProviderUsability::default(),
            },
        )
        .await;
    let app = fixture.router();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/diagnostics")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let diagnostics: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let qwen = diagnostics["providers"]
        .as_array()
        .and_then(|providers| {
            providers
                .iter()
                .find(|provider| provider["provider_id"].as_str() == Some("qwen"))
        })
        .unwrap_or_else(|| panic!("missing qwen provider diagnostics: {diagnostics:#?}"));

    assert_eq!(qwen["health"].as_str(), Some("error"));
    assert_eq!(
        qwen.pointer("/usability/reason_code")
            .and_then(serde_json::Value::as_str),
        Some("managed_config_error")
    );
    assert_eq!(
        qwen.pointer("/details/managed_config_error")
            .and_then(serde_json::Value::as_str),
        Some("true")
    );
    assert!(qwen["diagnostics"]
        .as_array()
        .is_some_and(|values| values.iter().any(|value| value
            .as_str()
            .is_some_and(|message| message.contains("parsing agent server config")))));
    assert!(diagnostics["managed_installs"]["error"]
        .as_str()
        .is_some_and(|value| value.contains("parsing agent server config")));
}
