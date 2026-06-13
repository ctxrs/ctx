use super::*;

#[tokio::test]
async fn pair_mobile_device_rejects_disabled_mobile_access_even_with_valid_token() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let daemon = fixture.daemon();
    let profile_id = insert_mobile_profile(daemon).await;
    daemon
        .mobile_access_for_test()
        .seed_default_mobile_access_config_for_test(
            profile_id,
            false,
            "daemon-public".to_string(),
            "daemon-private".to_string(),
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

    let app = fixture.router();
    let req = Request::builder()
        .method("POST")
        .uri("/api/mobile/pair")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "device_id": "33333333-3333-3333-3333-333333333333",
                "public_key": "device-public",
                "seq": 0,
                "nonce": "invalid",
                "ciphertext": "invalid"
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"], "mobile access not enabled");
    assert!(
        daemon
            .mobile_access_for_test()
            .consume_mobile_pairing_token_hash_for_test(&token_hash)
            .await
            .unwrap(),
        "disabled mobile access should not consume a valid pairing token"
    );
}
