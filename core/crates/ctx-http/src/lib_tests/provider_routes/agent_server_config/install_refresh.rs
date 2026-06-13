use super::*;

#[tokio::test]
async fn provider_install_surfaces_agent_server_config_errors() {
    let fixture = ProviderRouteFixture::new().await;
    write_invalid_agent_server_config(fixture.data_root());
    let app = fixture.app();

    let req = Request::builder()
        .method("POST")
        .uri("/api/providers/qwen/install?target=host")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["code"].as_str(),
        Some("agent_server_config_invalid")
    );
    assert!(payload["error"]
        .as_str()
        .is_some_and(|value| value.contains("parsing agent server config")));
}

#[tokio::test]
async fn install_all_providers_surfaces_agent_server_config_errors() {
    let fixture = ProviderRouteFixture::new().await;
    write_invalid_agent_server_config(fixture.data_root());
    let app = fixture.app();

    let req = Request::builder()
        .method("POST")
        .uri("/api/providers/install_all?target=host")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["code"].as_str(),
        Some("agent_server_config_invalid")
    );
    assert!(payload["error"]
        .as_str()
        .is_some_and(|value| value.contains("parsing agent server config")));
}

#[tokio::test]
async fn refresh_provider_matrix_surfaces_agent_server_config_errors() {
    let fixture = ProviderRouteFixture::new().await;
    write_invalid_agent_server_config(fixture.data_root());
    let app = fixture.app();

    let req = Request::builder()
        .method("POST")
        .uri("/api/providers/matrix/refresh")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["error"]
        .as_str()
        .is_some_and(|value| value.contains("failed to refresh provider statuses")));
    assert!(payload["error"]
        .as_str()
        .is_some_and(|value| value.contains("parsing agent server config")));
}

#[tokio::test]
async fn dev_restart_providers_returns_not_found_when_dev_mode_disabled() {
    let fixture = ProviderRouteFixture::new().await;
    let _dev_mode = EnvVarGuard::unset("CTX_DEV_MODE");

    let req = Request::builder()
        .method("POST")
        .uri("/api/dev/providers/restart")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"mode":"immediate"}"#))
        .unwrap();
    let res = fixture.app().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"].as_str(), Some("dev tools are disabled"));
}

#[tokio::test]
async fn dev_restart_providers_rejects_unknown_mode() {
    let fixture = ProviderRouteFixture::new().await;
    let _dev_mode = EnvVarGuard::set("CTX_DEV_MODE", "1");

    let req = Request::builder()
        .method("POST")
        .uri("/api/dev/providers/restart")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"mode":"later"}"#))
        .unwrap();
    let res = fixture.app().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"].as_str(),
        Some("mode must be 'immediate' or 'drain'")
    );
}

#[tokio::test]
async fn dev_restart_providers_returns_success_shape_when_enabled() {
    let fixture = ProviderRouteFixture::new().await;
    let _dev_mode = EnvVarGuard::set("CTX_DEV_MODE", "1");

    let req = Request::builder()
        .method("POST")
        .uri("/api/dev/providers/restart")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"mode":"drain"}"#))
        .unwrap();
    let res = fixture.app().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["mode"].as_str(), Some("drain"));
    assert_eq!(payload["results"].as_array().map(Vec::len), Some(0));
}
