use super::*;
use base64::Engine;

#[tokio::test]
async fn mobile_secure_proxy_rejects_provider_management_routes() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let (app, fixture, device_id, key, _data_dir) = build_mobile_secure_proxy_app(true).await;
    let cases = [
        ("GET", "/api/providers", None),
        ("GET", "/api/providers/auth/import/candidates", None),
        ("GET", "/api/providers/auth/import/profiles", None),
        (
            "POST",
            "/api/providers/auth/import",
            Some(json!({
                "candidate_ids": ["host:codex"]
            })),
        ),
        ("GET", "/api/providers/codex/import/host", None),
        (
            "POST",
            "/api/providers/codex/import/host",
            Some(json!({
                "label": "Imported Codex"
            })),
        ),
        ("GET", "/api/providers/qwen/harness_config", None),
        (
            "POST",
            "/api/providers/qwen/harness_config/select",
            Some(json!({
                "source_kind": "endpoint",
                "endpoint_id": "mobile-endpoint"
            })),
        ),
        (
            "POST",
            "/api/providers/qwen/harness_config/endpoints",
            Some(json!({
                "endpoint_id": "mobile-endpoint",
                "name": "Mobile Endpoint",
                "base_url": "https://example.com",
                "api_shape": "openai",
                "auth_type": "bearer",
                "api_key": "secret"
            })),
        ),
        (
            "DELETE",
            "/api/providers/qwen/harness_config/endpoints/mobile-endpoint",
            None,
        ),
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

    let codex_registry =
        ctx_provider_accounts::provider_accounts::load_codex_registry(fixture.daemon().data_root())
            .await
            .unwrap();
    assert!(codex_registry.accounts.is_empty());
    assert!(codex_registry.active_account_id.is_none());

    let qwen_config = ctx_harness_sources::harness_sources::get_provider_source_config(
        fixture.daemon().data_root(),
        "qwen",
    )
    .await
    .unwrap();
    assert!(qwen_config.endpoints.is_empty());
    assert!(qwen_config.selected_endpoint_id.is_none());
}
