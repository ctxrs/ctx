use super::*;

#[tokio::test]
async fn mobile_secure_proxy_rejects_disabled_mobile_access_for_existing_device() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let daemon = fixture.daemon();
    let profile_id = insert_mobile_profile(daemon).await;

    let device_id = "44444444-4444-4444-4444-444444444444";
    let (daemon_public_key, daemon_private_key) =
        ctx_transport_runtime::mobile_e2ee::generate_keypair();
    let (device_public_key, device_secret_key) =
        ctx_transport_runtime::mobile_e2ee::generate_keypair();
    daemon
        .mobile_access_for_test()
        .seed_default_mobile_access_config_for_test(
            profile_id,
            false,
            daemon_public_key,
            daemon_private_key.clone(),
        )
        .await
        .unwrap();
    daemon
        .mobile_access_for_test()
        .seed_mobile_device_for_test(
            MobileDeviceId(uuid::Uuid::parse_str(device_id).unwrap()),
            profile_id,
            device_public_key.clone(),
            "phone",
        )
        .await
        .unwrap();

    let key = ctx_transport_runtime::mobile_e2ee::derive_client_key(
        device_id,
        &device_secret_key,
        &daemon
            .mobile_access_for_test()
            .mobile_access_config_for_test()
            .await
            .unwrap()
            .unwrap()
            .daemon_public_key,
    )
    .unwrap();
    let plaintext = serde_json::to_vec(&json!({
        "method": "GET",
        "path": "/api/workspaces",
        "headers": []
    }))
    .unwrap();
    let envelope =
        ctx_transport_runtime::mobile_e2ee::encrypt(&key, device_id, 1, &plaintext).unwrap();

    let app = fixture.router();
    let req = Request::builder()
        .method("POST")
        .uri("/api/mobile/secure")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "device_id": envelope.device_id,
                "seq": envelope.seq,
                "nonce": envelope.nonce_b64,
                "ciphertext": envelope.ciphertext_b64
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"], "mobile access not enabled");
}
