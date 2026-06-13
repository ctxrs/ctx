use super::*;
use base64::Engine;

#[tokio::test]
async fn mobile_secure_proxy_rejects_repo_path_management_routes() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let (app, fixture, device_id, key, _data_dir) = build_mobile_secure_proxy_app(true).await;
    let sandbox = tempfile::tempdir().unwrap();
    let clone_parent = sandbox.path().join("mobile-clone-parent");
    let init_path = sandbox.path().join("mobile-init-target");
    let existing_repo = setup_git_repo().await;
    let staging_root = fixture
        .daemon()
        .data_root()
        .join("workspaces")
        .join("staging");

    let cases = [
        (
            "POST",
            "/api/repo/clone",
            Some(json!({
                "repo_url": "https://example.com/org/repo.git",
                "dest_parent": clone_parent.to_string_lossy(),
                "dest_name": "repo"
            })),
        ),
        (
            "POST",
            "/api/repo/init",
            Some(json!({
                "path": init_path.to_string_lossy()
            })),
        ),
        (
            "POST",
            "/api/repo/status",
            Some(json!({
                "path": existing_repo.path().to_string_lossy()
            })),
        ),
        (
            "POST",
            "/api/repo/validate_destination",
            Some(json!({
                "path": existing_repo.path().to_string_lossy()
            })),
        ),
        (
            "GET",
            &format!(
                "/api/repo/validate_destination?path={}",
                existing_repo.path().to_string_lossy()
            ),
            None,
        ),
        ("GET", "/api/repo/staging_path", None),
    ];

    for (index, (method, path, body)) in cases.into_iter().enumerate() {
        let mut payload = json!({
            "method": method,
            "path": path,
            "headers": [],
        });
        if let Some(body) = body {
            payload["headers"] = json!([["content-type", "application/json"]]);
            payload["body_b64"] =
                json!(base64::engine::general_purpose::STANDARD.encode(body.to_string()));
        }
        let res =
            post_mobile_secure_request(&app, &device_id, &key, index as i64 + 1, payload).await;
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "{method} {path} outer secure response"
        );

        let payload = decode_mobile_secure_response(res, &device_id, &key).await;
        assert_eq!(payload["status"], 401, "{method} {path} proxied status");

        let body_bytes = base64::engine::general_purpose::STANDARD
            .decode(payload["body_b64"].as_str().unwrap())
            .unwrap();
        let body_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body_json["error"], "desktop auth required");
    }

    assert!(
        !clone_parent.exists(),
        "mobile secure proxy unexpectedly created clone parent path"
    );
    assert!(
        !init_path.exists(),
        "mobile secure proxy unexpectedly created init path"
    );
    assert!(
        !staging_root.exists(),
        "mobile secure proxy unexpectedly created repo staging path"
    );
}
