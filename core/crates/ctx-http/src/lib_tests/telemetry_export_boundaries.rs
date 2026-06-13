use super::*;

#[tokio::test]
async fn telemetry_export_reads_valid_daily_log() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let app = fixture.router();

    let path =
        ctx_observability::perf_telemetry::perf_log_path_for_date(data_dir.path(), "2026-04-24");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "metric\n").unwrap();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/telemetry/export?date=2026-04-24")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), b"metric\n");
}

#[tokio::test]
async fn telemetry_export_rejects_path_traversal_dates() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let app = fixture.router();

    let escaped_path = data_dir
        .path()
        .parent()
        .unwrap()
        .join("telemetry-export-escape.jsonl");
    std::fs::write(&escaped_path, "escaped\n").unwrap();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/telemetry/export?date=x/../../../telemetry-export-escape")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn telemetry_export_returns_not_found_for_missing_daily_log() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let app = fixture.router();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/telemetry/export?date=2026-04-25")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
