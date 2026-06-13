use super::*;

#[tokio::test]
async fn resource_utilization_rejects_invalid_workspace_id_before_snapshot_lookup() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let app = fixture.router();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/resource_utilization?workspace_id=not-a-uuid")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert!(body.is_empty(), "resource errors should stay bare status");
}
