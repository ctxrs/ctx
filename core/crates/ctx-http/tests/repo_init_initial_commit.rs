mod common;

use axum::body::Body;
use axum::http::Request;
use serde_json::json;

#[tokio::test]
async fn repo_init_creates_initial_commit() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let dir = tempfile::tempdir().unwrap();
    let repo_path = dir.path().join("repo");

    let req = Request::builder()
        .method("POST")
        .uri("/api/repo/init")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "path": repo_path.to_string_lossy().to_string(),
                "allow_existing": false,
            })
            .to_string(),
        ))
        .unwrap();

    let (status, _resp): (axum::http::StatusCode, serde_json::Value) =
        common::oneshot_json(&app, req).await;
    assert_eq!(status, axum::http::StatusCode::OK);

    // Ensure the repo has a HEAD commit, which is required for worktree-based sessions.
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .arg("rev-parse")
        .arg("--verify")
        .arg("HEAD")
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "expected HEAD commit, got stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn repo_init_rejects_non_empty_dir_by_default() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let dir = tempfile::tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    tokio::fs::create_dir_all(&repo_path).await.unwrap();
    tokio::fs::write(repo_path.join("README.md"), "hello\n")
        .await
        .unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("/api/repo/init")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "path": repo_path.to_string_lossy().to_string(),
                "allow_existing": true,
            })
            .to_string(),
        ))
        .unwrap();

    let (status, resp): (axum::http::StatusCode, serde_json::Value) =
        common::oneshot_json(&app, req).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    let err = resp
        .get("error")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    assert!(
        err.contains("destination is not empty"),
        "expected non-empty rejection, got: {err}"
    );
}

#[tokio::test]
async fn repo_init_allows_non_empty_with_explicit_flag_without_staging_files() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let dir = tempfile::tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    tokio::fs::create_dir_all(&repo_path).await.unwrap();
    tokio::fs::write(repo_path.join("README.md"), "hello\n")
        .await
        .unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("/api/repo/init")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "path": repo_path.to_string_lossy().to_string(),
                "allow_existing": true,
                "allow_non_empty": true,
            })
            .to_string(),
        ))
        .unwrap();

    let (status, _resp): (axum::http::StatusCode, serde_json::Value) =
        common::oneshot_json(&app, req).await;
    assert_eq!(status, axum::http::StatusCode::OK);

    let head = common::run_git_output(&repo_path, &["rev-parse", "--verify", "HEAD"]).await;
    assert!(
        !head.trim().is_empty(),
        "expected HEAD commit for initialized repo"
    );

    let status_short = common::run_git_output(&repo_path, &["status", "--short"]).await;
    assert_eq!(
        status_short.trim(),
        "?? README.md",
        "existing files must remain untracked after init"
    );

    let tracked = common::run_git_output(&repo_path, &["ls-files"]).await;
    assert!(
        tracked.trim().is_empty(),
        "repo init should not stage or track existing files"
    );
}
