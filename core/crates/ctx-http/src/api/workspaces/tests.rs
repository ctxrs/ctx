use std::path::Path;
use std::process::Command;

use super::*;
use axum::body::{to_bytes, Body};
use axum::http::Request;
use ctx_core::ids::WorktreeId;
use ctx_core::models::{VcsKind, Workspace, Worktree};
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

fn run_git_for_workspace_registry_test(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_git_repo_for_workspace_registry_test(root: &Path) {
    std::fs::create_dir_all(root).expect("create repo root");
    run_git_for_workspace_registry_test(root, &["init"]);
    run_git_for_workspace_registry_test(root, &["checkout", "-b", "main"]);
    run_git_for_workspace_registry_test(root, &["config", "user.email", "test@example.com"]);
    run_git_for_workspace_registry_test(root, &["config", "user.name", "Test"]);
    std::fs::write(root.join("file.txt"), "hello\n").expect("write fixture file");
    run_git_for_workspace_registry_test(root, &["add", "."]);
    run_git_for_workspace_registry_test(root, &["commit", "-m", "init"]);
}

#[tokio::test]
async fn workspace_registry_and_harness_status_routes_return_seeded_workspace() {
    let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
    let daemon = fixture.daemon();
    let workspace_root = fixture.data_root().join("repo");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let workspace = daemon
        .seed_workspace_for_test("workspace", &workspace_root, VcsKind::Git)
        .await
        .expect("seed workspace");

    let app = fixture.router();
    let req = Request::builder()
        .method("GET")
        .uri("/api/workspaces")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    let workspace_id = workspace.id.0.to_string();
    assert!(
        list.iter()
            .any(|item| item.get("id").and_then(serde_json::Value::as_str)
                == Some(workspace_id.as_str())),
        "workspace list did not include seeded workspace: {list:#?}"
    );

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/workspaces/{}", workspace.id.0))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let fetched: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        fetched.get("root_path").and_then(serde_json::Value::as_str),
        Some(workspace.root_path.as_str())
    );

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/workspaces/{}/harness_container",
            workspace.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status, serde_json::Value::Null);

    daemon.request_shutdown();
}

#[tokio::test]
async fn invalid_workspace_registry_route_returns_bad_request() {
    let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
    let daemon = fixture.daemon();
    let app = fixture.router();

    let req = Request::builder()
        .method("GET")
        .uri("/api/workspaces/not-a-workspace")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    daemon.request_shutdown();
}

#[tokio::test]
async fn missing_workspace_registry_route_returns_not_found() {
    let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
    let daemon = fixture.daemon();
    let missing_workspace_id = Uuid::new_v4().to_string();
    let app = fixture.router();

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/workspaces/{missing_workspace_id}"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    daemon.request_shutdown();
}

#[tokio::test]
async fn invalid_workspace_delete_route_returns_bad_request() {
    let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
    let daemon = fixture.daemon();
    let app = fixture.router();

    let req = Request::builder()
        .method("DELETE")
        .uri("/api/workspaces/not-a-workspace")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    daemon.request_shutdown();
}

#[tokio::test]
async fn missing_workspace_delete_route_returns_not_found() {
    let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
    let daemon = fixture.daemon();
    let missing_workspace_id = Uuid::new_v4().to_string();
    let app = fixture.router();

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/workspaces/{missing_workspace_id}"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    daemon.request_shutdown();
}

#[tokio::test]
async fn delete_workspace_route_removes_seeded_workspace() {
    let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
    let daemon = fixture.daemon();
    let workspace_root = fixture.data_root().join("delete-route-repo");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let workspace = daemon
        .seed_workspace_for_test("delete-route", &workspace_root, VcsKind::Git)
        .await
        .expect("seed workspace");
    let app = fixture.router();

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/workspaces/{}", workspace.id.0))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/workspaces/{}", workspace.id.0))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    daemon.request_shutdown();
}

#[tokio::test]
async fn create_workspace_route_persists_detected_primary_branch() {
    let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
    let daemon = fixture.daemon();
    let repo_root = fixture.data_root().join("created-repo");
    init_git_repo_for_workspace_registry_test(&repo_root);

    let app = fixture.router();
    let req = Request::builder()
        .method("POST")
        .uri("/api/workspaces")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "root_path": repo_root.to_string_lossy().to_string(),
                "name": "created"
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let workspace: Workspace = serde_json::from_slice(&body).unwrap();

    let primary_branch = daemon
        .workspace_primary_branch_for_test(workspace.id)
        .await
        .expect("load primary branch");
    assert_eq!(primary_branch.as_deref(), Some("main"));

    daemon.request_shutdown();
}

#[tokio::test]
async fn invalid_workspace_primary_branch_routes_return_bad_request() {
    let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
    let daemon = fixture.daemon();
    let app = fixture.router();

    let req = Request::builder()
        .method("GET")
        .uri("/api/workspaces/not-a-workspace/primary_branch")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let req = Request::builder()
        .method("POST")
        .uri("/api/workspaces/not-a-workspace/primary_branch")
        .header("content-type", "application/json")
        .body(Body::from(json!({"primary_branch": "main"}).to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    daemon.request_shutdown();
}

#[tokio::test]
async fn invalid_worktree_routes_return_bad_request() {
    let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
    let daemon = fixture.daemon();
    let app = fixture.router();

    let req = Request::builder()
        .method("GET")
        .uri("/api/worktrees/not-a-worktree")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let req = Request::builder()
        .method("GET")
        .uri("/api/worktrees/not-a-worktree/bootstrap/logs")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    daemon.request_shutdown();
}

#[tokio::test]
async fn get_worktree_returns_live_root_for_bound_sandbox_worktree() {
    let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
    let daemon = fixture.daemon();
    let workspace_root = fixture.data_root().join("repo");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let host_root = fixture.data_root().join("managed-worktree");
    std::fs::create_dir_all(&host_root).expect("create managed worktree");
    let worktree = daemon
        .seed_sandbox_bound_worktree_for_test("ws", &workspace_root, &host_root, "/ctx/ws")
        .await
        .expect("seed sandbox-bound worktree");

    let app = fixture.router();
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/worktrees/{}", worktree.id.0))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let response: Worktree = serde_json::from_slice(&body).unwrap();

    assert_eq!(
        response.root_path,
        format!("/ctx/ws/worktrees/{}", worktree.id.0)
    );
    assert_eq!(response.id, worktree.id);
    assert_eq!(response.workspace_id, worktree.workspace_id);
    daemon.request_shutdown();
}

#[tokio::test]
async fn missing_worktree_routes_return_not_found() {
    let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
    let daemon = fixture.daemon();
    let missing_worktree_id = WorktreeId(Uuid::new_v4()).0.to_string();
    let app = fixture.router();

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/worktrees/{missing_worktree_id}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/worktrees/{missing_worktree_id}/bootstrap/logs"
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    daemon.request_shutdown();
}
