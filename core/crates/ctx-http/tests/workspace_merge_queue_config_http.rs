mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use ctx_core::ids::WorkspaceId;
use ctx_core::models::MergeQueueEntryStatus;
use serde_json::Value;

#[tokio::test]
async fn enabling_merge_queue_on_open_workspace_reschedules_queued_rows() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;

    daemon.spawn_merge_queue_runner();

    let entry = daemon
        .seed_workspace_merge_queue_queued_entry_for_test(workspace.id, "queued-before-enable")
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;
    let queued = daemon
        .load_workspace_merge_queue_entry_for_test(workspace.id, entry.id)
        .await
        .unwrap();
    assert_eq!(queued.status, MergeQueueEntryStatus::Queued);

    let (set_status, set_resp): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/merge_queue_config", workspace.id.0),
        Some(serde_json::json!({
            "enabled": true,
            "target_branch": "main"
        })),
    )
    .await;
    assert_eq!(set_status, StatusCode::OK);
    assert_eq!(set_resp.get("ok").and_then(Value::as_bool), Some(true));

    let resumed = daemon
        .wait_for_workspace_merge_queue_entry_to_leave_queued_for_test(
            workspace.id,
            entry.id,
            Duration::from_secs(2),
        )
        .await
        .unwrap();
    assert_ne!(resumed.status, MergeQueueEntryStatus::Queued);
    assert_ne!(resumed.status, MergeQueueEntryStatus::Cancelled);
    assert_ne!(
        resumed.error_message.as_deref(),
        Some("merge queue disabled while entry was queued")
    );
}

#[tokio::test]
async fn disabling_merge_queue_cancels_existing_queued_rows() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;

    daemon.spawn_merge_queue_runner();

    let (enable_status, _enable_resp): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/merge_queue_config", workspace.id.0),
        Some(serde_json::json!({
            "enabled": true,
            "target_branch": "main"
        })),
    )
    .await;
    assert_eq!(enable_status, StatusCode::OK);

    let entry = daemon
        .seed_workspace_merge_queue_queued_entry_for_test(workspace.id, "queued-before-disable")
        .await
        .unwrap();

    let (disable_status, disable_resp): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/merge_queue_config", workspace.id.0),
        Some(serde_json::json!({
            "enabled": false,
            "target_branch": "main"
        })),
    )
    .await;
    assert_eq!(disable_status, StatusCode::OK);
    assert_eq!(disable_resp.get("ok").and_then(Value::as_bool), Some(true));

    let disabled = daemon
        .wait_for_workspace_merge_queue_entry_to_leave_queued_for_test(
            workspace.id,
            entry.id,
            Duration::from_secs(2),
        )
        .await
        .unwrap();
    assert_eq!(disabled.status, MergeQueueEntryStatus::Cancelled);
    assert_eq!(
        disabled.error_message.as_deref(),
        Some("merge queue disabled while entry was queued")
    );
}

#[tokio::test]
async fn workspace_merge_queue_config_preserves_id_error_statuses() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let (invalid_get_status, invalid_get_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        "/api/workspaces/not-a-workspace/merge_queue_config",
        None,
    )
    .await;
    assert_eq!(invalid_get_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid_get_body.get("error").and_then(Value::as_str),
        Some("invalid workspace id")
    );

    let (invalid_post_status, invalid_post_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        "/api/workspaces/not-a-workspace/merge_queue_config",
        Some(serde_json::json!({
            "enabled": true,
            "target_branch": "main"
        })),
    )
    .await;
    assert_eq!(invalid_post_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid_post_body.get("error").and_then(Value::as_str),
        Some("invalid workspace id")
    );

    let missing_workspace_id = WorkspaceId::new();
    let (missing_get_status, missing_get_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!(
            "/api/workspaces/{}/merge_queue_config",
            missing_workspace_id.0
        ),
        None,
    )
    .await;
    assert_eq!(missing_get_status, StatusCode::NOT_FOUND);
    assert_eq!(
        missing_get_body.get("error").and_then(Value::as_str),
        Some("workspace not found")
    );

    let (missing_post_status, missing_post_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!(
            "/api/workspaces/{}/merge_queue_config",
            missing_workspace_id.0
        ),
        Some(serde_json::json!({
            "enabled": true,
            "target_branch": "main"
        })),
    )
    .await;
    assert_eq!(missing_post_status, StatusCode::NOT_FOUND);
    assert_eq!(
        missing_post_body.get("error").and_then(Value::as_str),
        Some("workspace not found")
    );
}
