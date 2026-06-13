mod common;

use axum::body::Body;
use axum::http::Request;
use serde_json::json;

#[tokio::test]
async fn repo_status_reports_existing_repo() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let repo = common::init_git_repo(&[("README.md", "hello\n")]).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/repo/status")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "path": repo.path().to_string_lossy().to_string(),
            })
            .to_string(),
        ))
        .unwrap();

    let (status, resp): (axum::http::StatusCode, serde_json::Value) =
        common::oneshot_json(&app, req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(
        resp.get("is_repo").and_then(|value| value.as_bool()),
        Some(true)
    );
    assert!(resp.get("error").is_none());
}

#[tokio::test]
async fn repo_status_rejects_missing_path() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing");

    let req = Request::builder()
        .method("POST")
        .uri("/api/repo/status")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "path": missing.to_string_lossy().to_string(),
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
        err.starts_with("invalid path '"),
        "expected invalid path error, got: {err}"
    );
}

#[tokio::test]
async fn repo_staging_path_uses_daemon_data_root() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let req = Request::builder()
        .method("GET")
        .uri("/api/repo/staging_path")
        .body(Body::empty())
        .unwrap();

    let (status, resp): (axum::http::StatusCode, serde_json::Value) =
        common::oneshot_json(&app, req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let path = std::path::PathBuf::from(
        resp.get("path")
            .and_then(|value| value.as_str())
            .unwrap_or_default(),
    );
    assert!(path.exists());
    let expected_root = fixture
        .data_dir
        .path()
        .canonicalize()
        .expect("canonical data dir")
        .join("workspaces")
        .join("staging");
    assert!(path.starts_with(expected_root));
}
