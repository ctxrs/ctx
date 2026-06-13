use super::*;

#[tokio::test]
async fn enable_mobile_access_is_explicitly_unavailable_in_public_export() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let state = fixture.daemon();

    let app = fixture.router();
    let req = Request::builder()
        .method("POST")
        .uri("/api/mobile/access/enable")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .header("content-type", "application/json")
        .body(Body::from(json!({}).to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    let cfg = state
        .mobile_access_for_test()
        .mobile_access_config_for_test()
        .await
        .unwrap();
    assert!(cfg.is_none());
}
