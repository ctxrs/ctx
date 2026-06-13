use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::json;
use std::future::Future;
use std::time::Duration;

mod common;

async fn bounded<T>(label: &'static str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(120), fut)
        .await
        .unwrap_or_else(|_| panic!("{label} timed out"))
}

#[tokio::test]
async fn post_message_idempotent_same_payload() {
    let repo = bounded(
        "init git repo",
        common::init_git_repo(&[("file.txt", "hello\n")]),
    )
    .await;
    let fixture = bounded(
        "fake daemon fixture",
        common::fake_daemon_fixture("http://127.0.0.1:0"),
    )
    .await;
    let app = fixture.router();

    let ws = bounded(
        "create workspace",
        common::create_workspace(&app, repo.path(), "ws"),
    )
    .await;
    let (_task, session) = bounded(
        "create task with session",
        common::create_task_with_session(&app, ws.id.0, "t1", "fake", "fake-model"),
    )
    .await;

    let message_id = common::fixed_uuid(1);
    let turn_id = common::fixed_uuid(2);
    let body = json!({
        "content": "hello",
        "id": message_id.to_string(),
        "turn_id": turn_id.to_string(),
    });

    let (status, msg1): (StatusCode, ctx_core::models::Message) = bounded(
        "first idempotent post",
        common::json_request(
            &app,
            Method::POST,
            format!("/api/sessions/{}/messages", session.id.0),
            Some(body.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(msg1.id.0, message_id);
    assert_eq!(msg1.turn_id.map(|id| id.0), Some(turn_id));

    let (status, msg2): (StatusCode, ctx_core::models::Message) = bounded(
        "repeat idempotent post",
        common::json_request(
            &app,
            Method::POST,
            format!("/api/sessions/{}/messages", session.id.0),
            Some(body),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(msg2.id.0, message_id);
    assert_eq!(msg2.turn_id.map(|id| id.0), Some(turn_id));
}

#[tokio::test]
async fn post_message_idempotent_conflict_on_change() {
    let repo = bounded(
        "init git repo",
        common::init_git_repo(&[("file.txt", "hello\n")]),
    )
    .await;
    let fixture = bounded(
        "fake daemon fixture",
        common::fake_daemon_fixture("http://127.0.0.1:0"),
    )
    .await;
    let app = fixture.router();

    let ws = bounded(
        "create workspace",
        common::create_workspace(&app, repo.path(), "ws"),
    )
    .await;
    let (_task, session) = bounded(
        "create task with session",
        common::create_task_with_session(&app, ws.id.0, "t1", "fake", "fake-model"),
    )
    .await;

    let message_id = common::fixed_uuid(10);
    let turn_id = common::fixed_uuid(11);
    let body = json!({
        "content": "hello",
        "id": message_id.to_string(),
        "turn_id": turn_id.to_string(),
    });
    let (status, _msg): (StatusCode, ctx_core::models::Message) = bounded(
        "initial conflict-control post",
        common::json_request(
            &app,
            Method::POST,
            format!("/api/sessions/{}/messages", session.id.0),
            Some(body),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let conflict_body = json!({
        "content": "hello changed",
        "id": message_id.to_string(),
        "turn_id": turn_id.to_string(),
    });
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/sessions/{}/messages", session.id.0))
        .header("content-type", "application/json")
        .body(Body::from(conflict_body.to_string()))
        .unwrap();
    let (status, _) = bounded("conflicting post", common::oneshot_bytes(&app, req)).await;
    assert_eq!(status, StatusCode::CONFLICT);
}
