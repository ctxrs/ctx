use super::*;

#[tokio::test]
async fn cors_preflight_allows_archived_endpoint_for_tauri_origin() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());
    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("desktop-token".to_string())).await;
    let app = fixture.router();
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/api/workspaces/00000000-0000-0000-0000-000000000000/archived_task_summaries")
        .header(header::ORIGIN, "tauri://localhost")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .header(
            header::ACCESS_CONTROL_REQUEST_HEADERS,
            "authorization,content-type,traceparent,x-ctx-run-id",
        )
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert!(
        res.status().is_success(),
        "expected successful preflight, got {}",
        res.status()
    );
    let origin = res
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|value| value.to_str().ok());
    assert_eq!(origin, Some("tauri://localhost"));
    let allow_headers = res
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(allow_headers.contains("authorization"));
    assert!(allow_headers.contains("content-type"));
    assert!(allow_headers.contains("traceparent"));
    assert!(allow_headers.contains("x-ctx-run-id"));
}

#[tokio::test]
async fn cors_preflight_allows_health_endpoint_for_tauri_localhost_origin() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());
    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("desktop-token".to_string())).await;
    let app = fixture.router();
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/api/health")
        .header(header::ORIGIN, "http://tauri.localhost")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert!(
        res.status().is_success(),
        "expected successful preflight, got {}",
        res.status()
    );
    let origin = res
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|value| value.to_str().ok());
    assert_eq!(origin, Some("http://tauri.localhost"));
}
