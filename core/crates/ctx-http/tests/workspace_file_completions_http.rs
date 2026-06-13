mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use ctx_core::ids::WorkspaceId;

async fn get_json<T: serde::de::DeserializeOwned>(
    app: &axum::Router,
    uri: impl Into<String>,
) -> (StatusCode, T) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri.into())
        .body(Body::empty())
        .unwrap();
    common::oneshot_json(app, req).await
}

async fn get_status(app: &axum::Router, uri: impl Into<String>) -> StatusCode {
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri.into())
        .body(Body::empty())
        .unwrap();
    common::oneshot_bytes(app, req).await.0
}

#[tokio::test]
async fn workspace_file_completions_lists_and_filters_git_files() {
    let repo = common::init_git_repo(&[
        ("README.md", "hello\n"),
        ("src/main.rs", "fn main() {}\n"),
        ("src/lib.rs", "pub fn lib() {}\n"),
    ])
    .await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;

    let (status, files): (StatusCode, Vec<String>) = get_json(
        &app,
        format!(
            "/api/workspaces/{}/completions/files?query=main&limit=5",
            workspace.id.0
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(files, vec!["src/main.rs"]);
}

#[tokio::test]
async fn workspace_file_completions_preserves_route_status_parity() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    assert_eq!(
        get_status(&app, "/api/workspaces/not-a-workspace/completions/files").await,
        StatusCode::BAD_REQUEST
    );

    let missing_workspace_id = WorkspaceId::new();
    assert_eq!(
        get_status(
            &app,
            format!(
                "/api/workspaces/{}/completions/files",
                missing_workspace_id.0
            ),
        )
        .await,
        StatusCode::NOT_FOUND
    );
}
