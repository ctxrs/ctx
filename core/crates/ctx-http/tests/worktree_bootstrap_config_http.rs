mod common;

use axum::http::{Method, StatusCode};
use ctx_core::ids::WorkspaceId;
use serde_json::Value;

#[tokio::test]
async fn worktree_bootstrap_config_endpoint_supports_get_post_and_blank_clear() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;

    let (get_empty_status, empty): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!(
            "/api/workspaces/{}/worktree_bootstrap_config",
            workspace.id.0
        ),
        None,
    )
    .await;
    assert_eq!(get_empty_status, StatusCode::OK);
    assert_eq!(empty, serde_json::json!({}));

    let (set_status, set_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!(
            "/api/workspaces/{}/worktree_bootstrap_config",
            workspace.id.0
        ),
        Some(serde_json::json!({
            "setup_command": "  ./scripts/bootstrap.sh  ",
            "timeout_sec": 45,
            "wait_for_completion": true
        })),
    )
    .await;
    assert_eq!(set_status, StatusCode::OK);
    assert_eq!(set_body.get("ok").and_then(Value::as_bool), Some(true));

    let (get_status, configured): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!(
            "/api/workspaces/{}/worktree_bootstrap_config",
            workspace.id.0
        ),
        None,
    )
    .await;
    assert_eq!(get_status, StatusCode::OK);
    assert_eq!(
        configured.get("setup_command").and_then(Value::as_str),
        Some("./scripts/bootstrap.sh")
    );
    assert_eq!(
        configured.get("timeout_sec").and_then(Value::as_u64),
        Some(45)
    );
    assert_eq!(
        configured
            .get("wait_for_completion")
            .and_then(Value::as_bool),
        Some(true)
    );

    let (clear_status, clear_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!(
            "/api/workspaces/{}/worktree_bootstrap_config",
            workspace.id.0
        ),
        Some(serde_json::json!({
            "setup_command": "   "
        })),
    )
    .await;
    assert_eq!(clear_status, StatusCode::OK);
    assert_eq!(clear_body.get("ok").and_then(Value::as_bool), Some(true));

    let (get_cleared_status, cleared): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!(
            "/api/workspaces/{}/worktree_bootstrap_config",
            workspace.id.0
        ),
        None,
    )
    .await;
    assert_eq!(get_cleared_status, StatusCode::OK);
    assert_eq!(cleared, serde_json::json!({}));
}

#[tokio::test]
async fn worktree_bootstrap_config_preserves_id_error_statuses() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let (invalid_get_status, invalid_get_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        "/api/workspaces/not-a-workspace/worktree_bootstrap_config",
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
        "/api/workspaces/not-a-workspace/worktree_bootstrap_config",
        Some(serde_json::json!({})),
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
            "/api/workspaces/{}/worktree_bootstrap_config",
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
            "/api/workspaces/{}/worktree_bootstrap_config",
            missing_workspace_id.0
        ),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(missing_post_status, StatusCode::NOT_FOUND);
    assert_eq!(
        missing_post_body.get("error").and_then(Value::as_str),
        Some("workspace not found")
    );
}
