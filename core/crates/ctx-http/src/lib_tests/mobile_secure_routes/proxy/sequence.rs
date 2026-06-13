use super::*;

#[tokio::test]
async fn mobile_secure_proxy_rejects_stale_sequence_without_rolling_back_counter() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let (app, _daemon, device_id, key, _data_dir) = build_mobile_secure_proxy_app(true).await;

    let first = post_mobile_secure_request(
        &app,
        &device_id,
        &key,
        2,
        json!({
            "method": "GET",
            "path": "/api/health",
            "headers": []
        }),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let first_payload = decode_mobile_secure_response(first, &device_id, &key).await;
    assert_eq!(first_payload["status"], 200);

    let stale = post_mobile_secure_request(
        &app,
        &device_id,
        &key,
        1,
        json!({
            "method": "GET",
            "path": "/api/health",
            "headers": []
        }),
    )
    .await;
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let stale_body = to_bytes(stale.into_body(), usize::MAX).await.unwrap();
    let stale_payload: serde_json::Value = serde_json::from_slice(&stale_body).unwrap();
    assert_eq!(stale_payload["error"], "stale request sequence");

    let replay = post_mobile_secure_request(
        &app,
        &device_id,
        &key,
        2,
        json!({
            "method": "GET",
            "path": "/api/health",
            "headers": []
        }),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::CONFLICT);
    let replay_body = to_bytes(replay.into_body(), usize::MAX).await.unwrap();
    let replay_payload: serde_json::Value = serde_json::from_slice(&replay_body).unwrap();
    assert_eq!(replay_payload["error"], "stale request sequence");
}
