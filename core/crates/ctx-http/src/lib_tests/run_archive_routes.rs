use super::*;
use ctx_core::ids::{RunId, WorkspaceId};
use ctx_core::models::{RunArchiveIngestBatch, RunArchiveIngestCursor, RunArchiveIngestScope};

async fn get_ingest_batch_json(
    app: &axum::Router,
    workspace_id: WorkspaceId,
    run_id: RunId,
    query: &str,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/workspaces/{}/runs/{}/archive/ingest_batch{}",
            workspace_id.0, run_id.0, query
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn get_ingest_batch_json_raw(
    app: &axum::Router,
    workspace_id: &str,
    run_id: &str,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/workspaces/{workspace_id}/runs/{run_id}/archive/ingest_batch"
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn post_ingest_ack_json(
    app: &axum::Router,
    workspace_id: WorkspaceId,
    run_id: RunId,
    query: &str,
    batch: &RunArchiveIngestBatch,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/workspaces/{}/runs/{}/archive/ingest_ack{}",
            workspace_id.0, run_id.0, query
        ))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(batch).unwrap()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn post_ingest_ack_json_raw(
    app: &axum::Router,
    workspace_id: &str,
    run_id: &str,
    batch: &RunArchiveIngestBatch,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/workspaces/{workspace_id}/runs/{run_id}/archive/ingest_ack"
        ))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(batch).unwrap()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn run_archive_routes_build_and_acknowledge_org_visible_batch() {
    let git_repo = setup_git_repo().await;
    let data_dir = tempfile::tempdir().unwrap();

    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let state = fixture.daemon();
    let app = fixture.router();

    let workspace = create_workspace_via_api(&app, &git_repo.path().to_string_lossy()).await;
    let fixture = state
        .seed_org_visible_run_archive_fixture_for_test(workspace.id, git_repo.path())
        .await
        .unwrap();
    let run_id = fixture.run_id;

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/workspaces/{}/runs/{}/archive/ingest_batch?max_items=25",
            workspace.id.0, run_id.0
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let batch: Option<RunArchiveIngestBatch> = serde_json::from_slice(&body).unwrap();
    let batch = batch.expect("org-visible run should produce archive batch");
    assert_eq!(batch.run.id, run_id);
    assert_eq!(batch.run.workspace_id, workspace.id);
    assert_eq!(batch.scope, RunArchiveIngestScope::Evidence);
    assert_eq!(batch.messages.len(), 1);
    let serialized = serde_json::to_string(&batch).unwrap();
    assert!(!serialized.contains("/home/fixture"));
    assert!(!serialized.contains("sk-12345678901234567890"));

    for query in ["", "?max_items=1", "?max_items=1000"] {
        let (status, body) = get_ingest_batch_json(&app, workspace.id, run_id, query).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.is_object(),
            "accepted query {query} should produce a batch"
        );
    }

    for query in ["?max_items=0", "?max_items=1001"] {
        let (status, body) = post_ingest_ack_json(&app, workspace.id, run_id, query, &batch).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body,
            json!({"error": "max_items must be between 1 and 1000"})
        );
    }

    let mut wrong_workspace_batch = batch.clone();
    wrong_workspace_batch.run.workspace_id = WorkspaceId::new();
    let (status, body) =
        post_ingest_ack_json(&app, workspace.id, run_id, "", &wrong_workspace_batch).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({"error": "archive ingest batch workspace_id must match route workspace id"})
    );

    let mut wrong_run_batch = batch.clone();
    wrong_run_batch.run.id = RunId::new();
    let (status, body) =
        post_ingest_ack_json(&app, workspace.id, run_id, "", &wrong_run_batch).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({"error": "archive ingest batch run id must match route run id"})
    );

    let mut private_org_batch = batch.clone();
    private_org_batch.run.org_id = None;
    let (status, body) =
        post_ingest_ack_json(&app, workspace.id, run_id, "", &private_org_batch).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({"error": "archive ingest acknowledgement requires an org-visible batch"})
    );

    let mut local_visibility_batch = batch.clone();
    local_visibility_batch.scope = RunArchiveIngestScope::None;
    let (status, body) =
        post_ingest_ack_json(&app, workspace.id, run_id, "", &local_visibility_batch).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({"error": "archive ingest acknowledgement requires an org-visible batch"})
    );

    let (status, body) =
        post_ingest_ack_json_raw(&app, "not-a-uuid", &run_id.0.to_string(), &batch).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"error": "invalid workspace id"}));

    let (status, body) =
        post_ingest_ack_json_raw(&app, &workspace.id.0.to_string(), "not-a-uuid", &batch).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"error": "invalid run id"}));

    let mut tampered_batch = batch.clone();
    tampered_batch.to.session_event_seq += 100;
    tampered_batch.to.audit_event_seq += 100;
    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/workspaces/{}/runs/{}/archive/ingest_ack",
            workspace.id.0, run_id.0
        ))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&tampered_batch).unwrap()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    let missing_workspace_id = WorkspaceId::new();
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/workspaces/{}/runs/{}/archive/ingest_batch?max_items=25",
            missing_workspace_id.0, run_id.0
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let mut missing_workspace_batch = batch.clone();
    missing_workspace_batch.run.workspace_id = missing_workspace_id;
    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/workspaces/{}/runs/{}/archive/ingest_ack",
            missing_workspace_id.0, run_id.0
        ))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&missing_workspace_batch).unwrap(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/workspaces/{}/runs/{}/archive/ingest_ack",
            workspace.id.0, run_id.0
        ))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&batch).unwrap()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let cursor: RunArchiveIngestCursor = serde_json::from_slice(&body).unwrap();
    assert_eq!(cursor.run_id, run_id);
    assert_eq!(cursor.watermark, batch.to);

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/workspaces/{}/runs/{}/archive/ingest_batch",
            workspace.id.0, run_id.0
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let batch: Option<RunArchiveIngestBatch> = serde_json::from_slice(&body).unwrap();
    assert!(batch.is_none());
}

#[tokio::test]
async fn run_archive_routes_reject_invalid_max_items_before_store_lookup() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let app = fixture.router();
    let workspace_id = WorkspaceId::new();
    let run_id = RunId::new();

    for query in ["?max_items=0", "?max_items=1001"] {
        let (status, body) = get_ingest_batch_json(&app, workspace_id, run_id, query).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body,
            json!({"error": "max_items must be between 1 and 1000"})
        );
    }
}

#[tokio::test]
async fn run_archive_routes_reject_invalid_route_ids_before_store_lookup() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let app = fixture.router();

    let (status, body) =
        get_ingest_batch_json_raw(&app, "not-a-uuid", &RunId::new().0.to_string()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"error": "invalid workspace id"}));

    let (status, body) =
        get_ingest_batch_json_raw(&app, &WorkspaceId::new().0.to_string(), "not-a-uuid").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"error": "invalid run id"}));
}
