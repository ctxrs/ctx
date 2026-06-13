use super::*;

#[tokio::test]
async fn pair_mobile_device_rejects_profiles_without_device_registration_scope() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let daemon = fixture.daemon();
    let profile_id =
        insert_mobile_profile_with_scopes(daemon, &["workspace_read", "workspace_stream"]).await;
    let (daemon_public_key, daemon_private_key) =
        ctx_transport_runtime::mobile_e2ee::generate_keypair();
    daemon
        .mobile_access_for_test()
        .seed_default_mobile_access_config_for_test(
            profile_id,
            true,
            daemon_public_key.clone(),
            daemon_private_key,
        )
        .await
        .unwrap();

    let token = "valid-pairing-token";
    let token_hash = daemon
        .mobile_access_for_test()
        .seed_mobile_pairing_token_for_test(
            "pair-1",
            token,
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .await
        .unwrap();

    let device_id = "33333333-3333-3333-3333-333333333333";
    let (device_public_key, device_secret_key) =
        ctx_transport_runtime::mobile_e2ee::generate_keypair();
    let encrypted_request = encrypted_pair_request_value(
        token,
        device_id,
        &daemon_public_key,
        &device_public_key,
        &device_secret_key,
    );
    let app = fixture.router();
    let req = Request::builder()
        .method("POST")
        .uri("/api/mobile/pair")
        .header("content-type", "application/json")
        .body(Body::from(encrypted_request.to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"],
        "mobile profile lacks device_registration scope"
    );
    assert!(
        daemon
            .mobile_access_for_test()
            .consume_mobile_pairing_token_hash_for_test(&token_hash)
            .await
            .unwrap(),
        "scope failures should not consume a valid pairing token"
    );
}
