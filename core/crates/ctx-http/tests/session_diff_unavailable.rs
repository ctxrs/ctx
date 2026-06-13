mod common;

use std::path::Path;
use std::process::Command;

use axum::http::{Method, StatusCode};
use ctx_core::models::Worktree;
use serde_json::Value;

#[tokio::test]
async fn session_diff_endpoints_return_no_repo_unavailable() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "diff", "fake", "fake-model").await;
    let session_id = session.id;

    let (worktree_status, worktree): (StatusCode, Worktree) = common::json_request(
        &app,
        Method::GET,
        format!("/api/worktrees/{}", session.worktree_id.0),
        None,
    )
    .await;
    assert_eq!(worktree_status, StatusCode::OK);
    let worktree_root = Path::new(&worktree.root_path);
    let git_path = worktree_root.join(".git");
    let metadata = tokio::fs::metadata(&git_path)
        .await
        .expect("worktree .git should exist");
    if metadata.is_dir() {
        tokio::fs::remove_dir_all(&git_path).await.unwrap();
    } else {
        tokio::fs::remove_file(&git_path).await.unwrap();
    }

    let (diff_status, diff): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/sessions/{}/diff", session_id.0),
        None,
    )
    .await;
    assert_eq!(diff_status, StatusCode::OK);
    assert_eq!(diff.get("available").and_then(Value::as_bool), Some(false));
    assert_eq!(
        diff.get("unavailable_reason").and_then(Value::as_str),
        Some("no_repo")
    );
    assert_eq!(diff.get("diff").and_then(Value::as_str), Some(""));

    let (summary_status, summary): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/sessions/{}/diff/summary", session_id.0),
        None,
    )
    .await;
    assert_eq!(summary_status, StatusCode::OK);
    assert_eq!(
        summary.get("available").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        summary.get("unavailable_reason").and_then(Value::as_str),
        Some("no_repo")
    );
    assert_eq!(summary.get("file_count").and_then(Value::as_i64), Some(0));
    assert_eq!(
        summary.get("line_additions").and_then(Value::as_i64),
        Some(0)
    );
    assert_eq!(
        summary.get("line_deletions").and_then(Value::as_i64),
        Some(0)
    );
}

#[tokio::test]
async fn session_diff_endpoints_return_no_target_branch_unavailable() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let state = &fixture.daemon;
    let app = fixture.router();

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    state
        .seed_workspace_runtime_settings_without_target_branch_for_test(ws.id)
        .await
        .expect("clearing workspace target branch should succeed");

    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "diff", "fake", "fake-model").await;
    let session_id = session.id;

    let (diff_status, diff): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/sessions/{}/diff", session_id.0),
        None,
    )
    .await;
    assert_eq!(diff_status, StatusCode::OK);
    assert_eq!(diff.get("available").and_then(Value::as_bool), Some(false));
    assert_eq!(
        diff.get("unavailable_reason").and_then(Value::as_str),
        Some("no_target_branch")
    );
    assert_eq!(diff.get("diff").and_then(Value::as_str), Some(""));

    let (summary_status, summary): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/sessions/{}/diff/summary", session_id.0),
        None,
    )
    .await;
    assert_eq!(summary_status, StatusCode::OK);
    assert_eq!(
        summary.get("available").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        summary.get("unavailable_reason").and_then(Value::as_str),
        Some("no_target_branch")
    );
    assert_eq!(summary.get("file_count").and_then(Value::as_i64), Some(0));
    assert_eq!(
        summary.get("line_additions").and_then(Value::as_i64),
        Some(0)
    );
    assert_eq!(
        summary.get("line_deletions").and_then(Value::as_i64),
        Some(0)
    );
}

#[tokio::test]
async fn workspace_primary_branch_endpoint_updates_branch() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let branch_status = Command::new("git")
        .current_dir(repo.path())
        .args(["branch", "merge-target"])
        .status()
        .expect("git branch command should run");
    assert!(
        branch_status.success(),
        "git branch merge-target should succeed"
    );

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let state = &fixture.daemon;
    let app = fixture.router();
    let ws = common::create_workspace(&app, repo.path(), "ws").await;

    let (get_status, before): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/workspaces/{}/primary_branch", ws.id.0),
        None,
    )
    .await;
    assert_eq!(get_status, StatusCode::OK);
    let before_branch = before
        .get("primary_branch")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        !before_branch.trim().is_empty(),
        "workspace primary branch should be auto-detected"
    );

    let (set_status, set_resp): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/primary_branch", ws.id.0),
        Some(serde_json::json!({ "primary_branch": "merge-target" })),
    )
    .await;
    assert_eq!(set_status, StatusCode::OK);
    assert_eq!(
        set_resp.get("primary_branch").and_then(Value::as_str),
        Some("merge-target")
    );

    let (get_status_after, after): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/workspaces/{}/primary_branch", ws.id.0),
        None,
    )
    .await;
    assert_eq!(get_status_after, StatusCode::OK);
    assert_eq!(
        after.get("primary_branch").and_then(Value::as_str),
        Some("merge-target")
    );

    let (_task, session) = common::create_task_with_session(
        &app,
        ws.id.0,
        "primary-branch-refresh",
        "fake",
        "fake-model",
    )
    .await;
    let worktree = state
        .load_worktree_for_test(session.worktree_id)
        .await
        .expect("worktree should exist");
    state.mark_worktree_vcs_active_for_test(worktree.id).await;
    state
        .emit_worktree_vcs_snapshot_for_worktree(&worktree, true)
        .await
        .expect("initial vcs snapshot emission should succeed");
    let before_snapshot = state
        .worktree_vcs_snapshot(worktree.id)
        .await
        .expect("expected cached worktree vcs snapshot");
    assert_eq!(
        before_snapshot.target_branch.as_deref(),
        Some("merge-target")
    );

    let branch_status = Command::new("git")
        .current_dir(repo.path())
        .args(["branch", "release-target"])
        .status()
        .expect("git branch command should run");
    assert!(
        branch_status.success(),
        "git branch release-target should succeed"
    );

    let (set_status, set_resp): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/primary_branch", ws.id.0),
        Some(serde_json::json!({ "primary_branch": "release-target" })),
    )
    .await;
    assert_eq!(set_status, StatusCode::OK);
    assert_eq!(
        set_resp.get("primary_branch").and_then(Value::as_str),
        Some("release-target")
    );

    let refreshed = state
        .worktree_vcs_snapshot(worktree.id)
        .await
        .expect("expected refreshed worktree vcs snapshot");
    assert_eq!(refreshed.target_branch.as_deref(), Some("release-target"));
}

#[tokio::test]
async fn workspace_primary_branch_preserves_invalid_id_error_contract() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let (get_status, get_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        "/api/workspaces/not-a-workspace/primary_branch",
        None,
    )
    .await;
    assert_eq!(get_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        get_body.get("error").and_then(Value::as_str),
        Some("invalid workspace id")
    );

    let (post_status, post_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        "/api/workspaces/not-a-workspace/primary_branch",
        Some(serde_json::json!({ "primary_branch": "main" })),
    )
    .await;
    assert_eq!(post_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        post_body.get("error").and_then(Value::as_str),
        Some("invalid workspace id")
    );
}

#[tokio::test]
async fn workspace_merge_queue_config_endpoint_supports_get_and_post() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let ws = common::create_workspace(&app, repo.path(), "ws").await;

    let (get_status_before, before): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/workspaces/{}/merge_queue_config", ws.id.0),
        None,
    )
    .await;
    assert_eq!(get_status_before, StatusCode::OK);
    assert_eq!(
        before.get("target_branch").and_then(Value::as_str),
        Some("main")
    );
    assert_eq!(
        before.get("push_on_success").and_then(Value::as_bool),
        Some(false)
    );

    let (set_status, set_resp): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/merge_queue_config", ws.id.0),
        Some(serde_json::json!({
            "enabled": true,
            "target_branch": "merge-target",
            "verify_command": "pnpm test",
            "push_on_success": true,
            "push_remote": "origin",
            "push_branch": "merge-target"
        })),
    )
    .await;
    assert_eq!(set_status, StatusCode::OK);
    assert_eq!(set_resp.get("ok").and_then(Value::as_bool), Some(true));

    let (get_status_after, after): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/workspaces/{}/merge_queue_config", ws.id.0),
        None,
    )
    .await;
    assert_eq!(get_status_after, StatusCode::OK);
    assert_eq!(
        after.get("target_branch").and_then(Value::as_str),
        Some("merge-target")
    );
    assert_eq!(
        after.get("verify_command").and_then(Value::as_str),
        Some("pnpm test")
    );
    assert_eq!(
        after.get("push_on_success").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        after.get("push_remote").and_then(Value::as_str),
        Some("origin")
    );
    assert_eq!(
        after.get("push_branch").and_then(Value::as_str),
        Some("merge-target")
    );
}
