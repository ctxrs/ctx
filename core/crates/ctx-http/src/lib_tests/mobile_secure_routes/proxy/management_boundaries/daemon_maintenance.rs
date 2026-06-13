use super::*;
use base64::Engine;

#[tokio::test]
async fn mobile_secure_proxy_rejects_daemon_maintenance_routes() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let (app, _daemon, device_id, key, _data_dir) = build_mobile_secure_proxy_app(true).await;

    let cases = [
        ("POST", "/api/execution/linux_sandbox_runtime/prepare"),
        ("POST", "/api/logs/open"),
        ("POST", "/api/updates/appimage/apply"),
    ];

    for (index, (method, path)) in cases.into_iter().enumerate() {
        let res = post_mobile_secure_request(
            &app,
            &device_id,
            &key,
            index as i64 + 1,
            json!({
                "method": method,
                "path": path,
                "headers": []
            }),
        )
        .await;
        if res.status() != StatusCode::OK {
            let status = res.status();
            let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
            panic!(
                "{method} {path} outer secure response was {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }

        let payload = decode_mobile_secure_response(res, &device_id, &key).await;
        assert_eq!(payload["status"], 401, "{method} {path} proxied status");
        let body_bytes = base64::engine::general_purpose::STANDARD
            .decode(payload["body_b64"].as_str().unwrap())
            .unwrap();
        let body_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body_json["error"], "desktop auth required");
    }
}
