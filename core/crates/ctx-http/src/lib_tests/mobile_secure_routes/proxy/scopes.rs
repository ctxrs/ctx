use super::*;

#[tokio::test]
async fn mobile_secure_proxy_grants_mobile_auth_for_proxied_api_routes() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let (app, _daemon, device_id, key, _data_dir) = build_mobile_secure_proxy_app(true).await;
    let res = post_mobile_secure_request(
        &app,
        &device_id,
        &key,
        1,
        json!({
            "method": "GET",
            "path": "/api/workspaces",
            "headers": []
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);

    let payload = decode_mobile_secure_response(res, &device_id, &key).await;
    assert_eq!(payload["status"], 200);
    let body_bytes = base64::engine::general_purpose::STANDARD
        .decode(payload["body_b64"].as_str().unwrap())
        .unwrap();
    let body_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body_json, json!([]));
}

#[tokio::test]
async fn mobile_secure_proxy_health_respects_desktop_bearer_auth_for_sensitive_fields() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let (app, _daemon, device_id, key, _data_dir) = build_mobile_secure_proxy_app(true).await;
    let public = post_mobile_secure_request(
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
    assert_eq!(public.status(), StatusCode::OK);
    let public_payload = decode_mobile_secure_response(public, &device_id, &key).await;
    assert_eq!(public_payload["status"], 200);
    let public_body = decode_secure_proxy_json_body(&public_payload);
    assert_eq!(public_body["auth_required"], true);
    assert!(public_body.get("data_root").is_none());
    assert!(public_body.get("pid").is_none());

    let sensitive = post_mobile_secure_request(
        &app,
        &device_id,
        &key,
        2,
        json!({
            "method": "GET",
            "path": "/api/health",
            "headers": [["Authorization", "Bearer daemon-secret"]]
        }),
    )
    .await;
    assert_eq!(sensitive.status(), StatusCode::OK);
    let sensitive_payload = decode_mobile_secure_response(sensitive, &device_id, &key).await;
    assert_eq!(sensitive_payload["status"], 200);
    let sensitive_body = decode_secure_proxy_json_body(&sensitive_payload);
    assert_eq!(sensitive_body["auth_required"], true);
    assert!(sensitive_body["data_root"].is_string());
    assert!(sensitive_body["pid"].is_number());
}

#[tokio::test]
async fn mobile_secure_proxy_returns_seeded_workspace_list_and_detail() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let (app, _daemon, device_id, key, _data_dir) = build_mobile_secure_proxy_app(true).await;
    let git_repo = setup_git_repo().await;
    let workspace =
        create_workspace_with_desktop_auth(&app, &git_repo.path().to_string_lossy()).await;

    let list = post_mobile_secure_request(
        &app,
        &device_id,
        &key,
        1,
        json!({
            "method": "GET",
            "path": "/api/workspaces",
            "headers": []
        }),
    )
    .await;
    assert_eq!(list.status(), StatusCode::OK);
    let list_payload = decode_mobile_secure_response(list, &device_id, &key).await;
    assert_eq!(list_payload["status"], 200);
    let list_body = decode_secure_proxy_json_body(&list_payload);
    assert_eq!(list_body.as_array().unwrap().len(), 1);
    assert_eq!(list_body[0]["id"], workspace.id.0.to_string());

    let detail = post_mobile_secure_request(
        &app,
        &device_id,
        &key,
        2,
        json!({
            "method": "GET",
            "path": format!("/api/workspaces/{}", workspace.id.0),
            "headers": []
        }),
    )
    .await;
    assert_eq!(detail.status(), StatusCode::OK);
    let detail_payload = decode_mobile_secure_response(detail, &device_id, &key).await;
    assert_eq!(detail_payload["status"], 200);
    let detail_body = decode_secure_proxy_json_body(&detail_payload);
    assert_eq!(detail_body["id"], workspace.id.0.to_string());
    assert_eq!(detail_body["name"], workspace.name);
}

#[tokio::test]
async fn mobile_secure_proxy_rejects_profiles_without_workspace_read_scope() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let (app, _daemon, device_id, key, _data_dir) =
        build_mobile_secure_proxy_app_with_scopes(true, &["device_registration"]).await;
    let res = post_mobile_secure_request(
        &app,
        &device_id,
        &key,
        1,
        json!({
            "method": "GET",
            "path": "/api/workspaces",
            "headers": []
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);

    let payload = decode_mobile_secure_response(res, &device_id, &key).await;
    assert_eq!(payload["status"], 401);
    let body_bytes = base64::engine::general_purpose::STANDARD
        .decode(payload["body_b64"].as_str().unwrap())
        .unwrap();
    let body_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(
        body_json["error"],
        "mobile profile lacks workspace_read scope"
    );
}

fn decode_secure_proxy_json_body(payload: &serde_json::Value) -> serde_json::Value {
    let body_bytes = base64::engine::general_purpose::STANDARD
        .decode(payload["body_b64"].as_str().unwrap())
        .unwrap();
    serde_json::from_slice(&body_bytes).unwrap()
}

async fn create_workspace_with_desktop_auth(
    app: &axum::Router,
    root_path: &str,
) -> ctx_core::models::Workspace {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/workspaces")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::from(
            json!({
                "root_path": root_path,
                "name": "ws"
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn mobile_secure_proxy_migrates_legacy_empty_scope_profiles() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let (app, fixture, device_id, key, _data_dir) =
        build_mobile_secure_proxy_app_with_scopes(true, &[]).await;
    let res = post_mobile_secure_request(
        &app,
        &device_id,
        &key,
        1,
        json!({
            "method": "GET",
            "path": "/api/workspaces",
            "headers": []
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);

    let payload = decode_mobile_secure_response(res, &device_id, &key).await;
    assert_eq!(payload["status"], 200);

    let cfg = fixture
        .daemon()
        .mobile_access_for_test()
        .mobile_access_config_for_test()
        .await
        .unwrap()
        .expect("mobile access config should exist");
    let profile = fixture
        .daemon()
        .mobile_access_for_test()
        .mobile_profile_for_test(cfg.profile_id)
        .await
        .unwrap()
        .expect("mobile profile should still exist");
    assert_eq!(
        profile.scopes,
        vec![
            "device_registration".to_string(),
            "workspace_read".to_string(),
            "workspace_stream".to_string(),
        ]
    );
}
