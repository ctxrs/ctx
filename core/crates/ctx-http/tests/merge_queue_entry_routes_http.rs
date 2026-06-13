mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use ctx_core::ids::WorkspaceId;
use ctx_core::models::{MergeQueueEntry, MergeQueueEntryStatus, VcsKind};
use serde_json::Value;

#[tokio::test]
async fn merge_queue_entry_routes_preserve_wire_shape_for_list_cancel_and_retry() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let repo = tempfile::tempdir().expect("repo tempdir");
    let workspace = fixture
        .daemon
        .seed_workspace_for_test("merge-queue-routes", repo.path(), VcsKind::Git)
        .await
        .expect("seed workspace");
    let seeded = fixture
        .daemon
        .seed_workspace_merge_queue_queued_entry_for_test(workspace.id, "queued-entry")
        .await
        .expect("seed merge queue entry");

    let (list_status, listed): (StatusCode, Vec<MergeQueueEntry>) = common::json_request(
        &app,
        Method::GET,
        format!(
            "/api/merge-queue/entries?workspace_id={}&limit=10",
            workspace.id.0
        ),
        None,
    )
    .await;
    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, seeded.id);
    assert_eq!(listed[0].status, MergeQueueEntryStatus::Queued);

    let (cancel_status, cancelled): (StatusCode, MergeQueueEntry) = common::json_request(
        &app,
        Method::POST,
        format!(
            "/api/workspaces/{}/merge_queue/entries/{}/cancel",
            workspace.id.0, seeded.id.0
        ),
        None,
    )
    .await;
    assert_eq!(cancel_status, StatusCode::OK);
    assert_eq!(cancelled.id, seeded.id);
    assert_eq!(cancelled.status, MergeQueueEntryStatus::Cancelled);

    let (retry_status, retry_body): (StatusCode, MergeQueueEntry) = common::json_request(
        &app,
        Method::POST,
        format!(
            "/api/workspaces/{}/merge_queue/entries/{}/retry",
            workspace.id.0, seeded.id.0
        ),
        None,
    )
    .await;
    assert_eq!(retry_status, StatusCode::OK);
    assert_eq!(retry_body.id, seeded.id);
    assert_eq!(retry_body.status, MergeQueueEntryStatus::Cancelled);
}

#[tokio::test]
async fn merge_queue_entry_routes_preserve_error_status_and_body_shapes() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let missing_workspace_id = WorkspaceId::new();

    let invalid_list = Request::builder()
        .method(Method::GET)
        .uri("/api/merge-queue/entries?workspace_id=not-a-workspace")
        .body(Body::empty())
        .unwrap();
    let (invalid_list_status, invalid_list_body) = common::oneshot_bytes(&app, invalid_list).await;
    assert_eq!(invalid_list_status, StatusCode::BAD_REQUEST);
    assert!(
        invalid_list_body.is_empty(),
        "list errors should remain bare status bodies: {:?}",
        String::from_utf8_lossy(&invalid_list_body)
    );

    let missing_list = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/api/merge-queue/entries?workspace_id={}",
            missing_workspace_id.0
        ))
        .body(Body::empty())
        .unwrap();
    let (missing_list_status, missing_list_body) = common::oneshot_bytes(&app, missing_list).await;
    assert_eq!(missing_list_status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        missing_list_body.is_empty(),
        "list errors should remain bare status bodies: {:?}",
        String::from_utf8_lossy(&missing_list_body)
    );

    let (invalid_entry_status, invalid_entry_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!(
            "/api/workspaces/{}/merge_queue/entries/not-an-entry/cancel",
            missing_workspace_id.0
        ),
        None,
    )
    .await;
    assert_eq!(invalid_entry_status, StatusCode::BAD_REQUEST);
    assert_eq!(invalid_entry_body["error"], "invalid entry id");

    let (invalid_workspace_status, invalid_workspace_body): (StatusCode, Value) =
        common::json_request(
            &app,
            Method::POST,
            "/api/workspaces/not-a-workspace/merge_queue/entries/not-an-entry/retry",
            None,
        )
        .await;
    assert_eq!(invalid_workspace_status, StatusCode::BAD_REQUEST);
    assert_eq!(invalid_workspace_body["error"], "invalid workspace id");

    let (missing_workspace_status, missing_workspace_body): (StatusCode, Value) =
        common::json_request(
            &app,
            Method::POST,
            format!(
                "/api/workspaces/{}/merge_queue/entries/{}/cancel",
                missing_workspace_id.0,
                uuid::Uuid::new_v4()
            ),
            None,
        )
        .await;
    assert_eq!(missing_workspace_status, StatusCode::BAD_REQUEST);
    assert!(
        missing_workspace_body["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "missing workspace action errors should remain ApiErrorResp: {missing_workspace_body:#?}"
    );
}
