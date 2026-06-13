mod common;

use axum::body::Body;
use axum::http::Request;
use serde_json::json;

#[tokio::test]
async fn repo_clone_accepts_branch_option() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let src = common::init_git_repo(&[("README.md", "hi")]).await;
    let src_root = src.path();
    common::run_git(src_root, &["checkout", "-b", "feature"]).await;
    tokio::fs::write(src_root.join("feature.txt"), "feature")
        .await
        .unwrap();
    common::run_git(src_root, &["add", "."]).await;
    common::run_git(src_root, &["commit", "-m", "feature"]).await;

    let dest_parent = tempfile::tempdir().unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("/api/repo/clone")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "repo_url": src_root.to_string_lossy().to_string(),
                "dest_parent": dest_parent.path().to_string_lossy().to_string(),
                "branch": "feature",
                "dest_name": "cloned",
            })
            .to_string(),
        ))
        .unwrap();

    let (status, resp): (axum::http::StatusCode, serde_json::Value) =
        common::oneshot_json(&app, req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let cloned_path = resp
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    let cloned_root = std::path::PathBuf::from(cloned_path);

    assert!(cloned_root.join("feature.txt").exists());
    let head = common::run_git_output(&cloned_root, &["rev-parse", "--abbrev-ref", "HEAD"]).await;
    assert_eq!(head.trim(), "feature");
}

#[tokio::test]
async fn repo_clone_rejects_dest_name_traversal() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let src = common::init_git_repo(&[("README.md", "hi")]).await;
    let src_root = src.path();
    let dest_parent = tempfile::tempdir().unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("/api/repo/clone")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "repo_url": src_root.to_string_lossy().to_string(),
                "dest_parent": dest_parent.path().to_string_lossy().to_string(),
                "dest_name": "../escape",
            })
            .to_string(),
        ))
        .unwrap();

    let (status, resp): (axum::http::StatusCode, serde_json::Value) =
        common::oneshot_json(&app, req).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(err.contains("dest_name"), "unexpected error: {err}");
}
