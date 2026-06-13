use super::*;

#[tokio::test]
async fn mobile_secure_proxy_rejects_mobile_management_paths_after_trimming() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let (app, fixture, device_id, key, _data_dir) = build_mobile_secure_proxy_app(true).await;
    let target_device_id = "55555555-5555-5555-5555-555555555555";
    let pairing_token = "pairing-token-through-secure-proxy";
    fixture
        .daemon()
        .mobile_access_for_test()
        .seed_mobile_pairing_token_for_test(
            "pair-smuggle",
            pairing_token,
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .await
        .unwrap();

    let res = post_mobile_secure_request(
        &app,
        &device_id,
        &key,
        1,
        json!({
            "method": "POST",
            "path": " /api/mobile/pair",
            "headers": [["content-type", "application/json"]],
            "body_b64": base64::engine::general_purpose::STANDARD.encode(
                json!({
                    "pairing_token": pairing_token,
                    "device_id": target_device_id,
                    "device_label": "smuggled-device",
                    "platform": "ios",
                    "public_key": "smuggled-public-key"
                })
                .to_string()
            )
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"],
        "secure proxy cannot target mobile management endpoints"
    );
    assert!(
        fixture
            .daemon()
            .mobile_access_for_test()
            .mobile_device_for_test(MobileDeviceId(
                uuid::Uuid::parse_str(target_device_id).unwrap()
            ))
            .await
            .unwrap()
            .is_none(),
        "mobile secure proxy unexpectedly registered a device through a trimmed management path"
    );

    let res = post_mobile_secure_request(
        &app,
        &device_id,
        &key,
        2,
        json!({
            "method": "POST",
            "path": "/api/%6dobile/pair",
            "headers": [["content-type", "application/json"]],
            "body_b64": base64::engine::general_purpose::STANDARD.encode(
                json!({
                    "pairing_token": pairing_token,
                    "device_id": target_device_id,
                    "device_label": "encoded-smuggled-device",
                    "platform": "ios",
                    "public_key": "smuggled-public-key"
                })
                .to_string()
            )
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"], "secure proxy path must be normalized");
    assert!(
        fixture
            .daemon()
            .mobile_access_for_test()
            .mobile_device_for_test(MobileDeviceId(
                uuid::Uuid::parse_str(target_device_id).unwrap()
            ))
            .await
            .unwrap()
            .is_none(),
        "mobile secure proxy unexpectedly registered a device through an encoded management path"
    );
}
