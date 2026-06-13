use super::*;

#[tokio::test]
async fn disable_mobile_access_clears_outstanding_pairing_tokens() {
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
            true,
            "daemon-public".to_string(),
            "daemon-private".to_string(),
        )
        .await
        .unwrap();
    daemon
        .mobile_access_for_test()
        .seed_mobile_device_for_test(
            MobileDeviceId(uuid::Uuid::parse_str("55555555-5555-5555-5555-555555555555").unwrap()),
            profile_id,
            "device-public".to_string(),
            "old-phone",
        )
        .await
        .unwrap();

    let token = "pairing-token-to-clear";
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
        .uri("/api/mobile/access/disable")
        .header("content-type", "application/json")
        .body(Body::from(json!({ "supabase_token": "token" }).to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let cfg = daemon
        .mobile_access_for_test()
        .mobile_access_config_for_test()
        .await
        .unwrap();
    assert!(cfg.is_none());
    let profile = daemon
        .mobile_access_for_test()
        .mobile_profile_for_test(profile_id)
        .await
        .unwrap();
    assert!(profile.is_none());
    let device = daemon
        .mobile_access_for_test()
        .mobile_device_for_test(MobileDeviceId(
            uuid::Uuid::parse_str("55555555-5555-5555-5555-555555555555").unwrap(),
        ))
        .await
        .unwrap();
    assert!(device.is_none());
    let still_allowed = daemon
        .mobile_access_for_test()
        .consume_mobile_pairing_token_hash_for_test(&token_hash)
        .await
        .unwrap();
    assert!(!still_allowed);
}
