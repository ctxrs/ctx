mod common;

use axum::body::Body;
use axum::http::Request;
use serde_json::json;

#[tokio::test]
async fn repo_validate_destination_rejects_non_empty_dir_when_required() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("existing");
    tokio::fs::create_dir_all(&path).await.unwrap();
    tokio::fs::write(path.join("README.md"), "hello\n")
        .await
        .unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("/api/repo/validate_destination")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "path": path.to_string_lossy().to_string(),
                "require_empty_if_exists": true,
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
async fn repo_validate_destination_rejects_existing_dir_when_must_not_exist() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("existing");
    tokio::fs::create_dir_all(&path).await.unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("/api/repo/validate_destination")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "path": path.to_string_lossy().to_string(),
                "must_not_exist": true,
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
        err.contains("destination already exists"),
        "expected exists rejection, got: {err}"
    );
}

#[tokio::test]
async fn repo_validate_destination_allows_missing_path() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing");

    let req = Request::builder()
        .method("POST")
        .uri("/api/repo/validate_destination")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "path": path.to_string_lossy().to_string(),
                "require_empty_if_exists": true,
            })
            .to_string(),
        ))
        .unwrap();

    let (status, resp): (axum::http::StatusCode, serde_json::Value) =
        common::oneshot_json(&app, req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let got = resp
        .get("path")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    assert!(
        got.ends_with("/missing"),
        "expected returned path to end with /missing, got: {got}"
    );
}

#[tokio::test]
async fn repo_validate_destination_accepts_get_query() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing");
    let uri = format!(
        "/api/repo/validate_destination?path={}&require_empty_if_exists=true",
        path.to_string_lossy()
    );

    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();

    let (status, resp): (axum::http::StatusCode, serde_json::Value) =
        common::oneshot_json(&app, req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let got = resp
        .get("path")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    assert!(
        got.ends_with("/missing"),
        "expected returned path to end with /missing, got: {got}"
    );
}
