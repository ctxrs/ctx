use super::*;
use base64::Engine;

#[tokio::test]
async fn mobile_secure_proxy_rejects_provider_login_routes() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let (app, fixture, device_id, key, _data_dir) = build_mobile_secure_proxy_app(true).await;
    let cases = [
        (
            "POST",
            "/api/providers/gemini/accounts/login/start",
            Some(json!({})),
        ),
        ("GET", "/api/providers/gemini/accounts/login/test", None),
        (
            "POST",
            "/api/providers/qwen/accounts/login/start",
            Some(json!({})),
        ),
        ("GET", "/api/providers/qwen/accounts/login/test", None),
        (
            "POST",
            "/api/providers/amp/accounts/login/start",
            Some(json!({})),
        ),
        ("GET", "/api/providers/amp/accounts/login/test", None),
        (
            "POST",
            "/api/providers/mistral/accounts/login/start",
            Some(json!({})),
        ),
        ("GET", "/api/providers/mistral/accounts/login/test", None),
        (
            "POST",
            "/api/providers/kimi/accounts/login/start",
            Some(json!({})),
        ),
        ("GET", "/api/providers/kimi/accounts/login/test", None),
        (
            "POST",
            "/api/providers/claude/accounts/login/start",
            Some(json!({})),
        ),
        ("GET", "/api/providers/claude/accounts/login/test", None),
        (
            "POST",
            "/api/providers/codex/accounts/login/start",
            Some(json!({})),
        ),
        ("GET", "/api/providers/codex/accounts/login/test", None),
        (
            "POST",
            "/api/providers/codex/accounts/login/test",
            Some(json!({
                "callback_url": "http://127.0.0.1:4321/auth/callback?code=test",
                "completion_token": "token"
            })),
        ),
        (
            "POST",
            "/api/providers/cursor/accounts/login/start",
            Some(json!({})),
        ),
        ("GET", "/api/providers/cursor/accounts/login/test", None),
    ];

    for (index, (method, path, body)) in cases.into_iter().enumerate() {
        let mut payload = json!({
            "method": method,
            "path": path,
            "headers": []
        });
        if let Some(body) = body {
            payload["headers"] = json!([["content-type", "application/json"]]);
            payload["body_b64"] =
                json!(base64::engine::general_purpose::STANDARD.encode(body.to_string()));
        }

        let res =
            post_mobile_secure_request(&app, &device_id, &key, index as i64 + 1, payload).await;
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
        assert_eq!(
            body_json["error"], "desktop auth required",
            "{method} {path} proxied body"
        );
    }

    let res = post_mobile_secure_request(
        &app,
        &device_id,
        &key,
        100,
        json!({
            "method": "POST",
            "path": "/api/providers/qwen/accounts/%6cogin/start",
            "headers": [["content-type", "application/json"]],
            "body_b64": base64::engine::general_purpose::STANDARD.encode(json!({}).to_string())
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"], "secure proxy path must be normalized");

    assert!(fixture.daemon().provider_login_session_caches_empty().await);
}
