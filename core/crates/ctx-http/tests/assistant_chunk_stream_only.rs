use std::time::Duration;

use axum::http::{Method, StatusCode};
use serde_json::json;

use ctx_core::models::SessionEventType;

mod common;

#[tokio::test]
async fn assistant_chunks_are_stream_only() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "t1", "fake", "fake-model").await;

    let (status, _msg): (StatusCode, ctx_core::models::Message) = common::json_request(
        &app,
        Method::POST,
        format!("/api/sessions/{}/messages", session.id.0),
        Some(json!({"content":"hello"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let snapshot = daemon
        .assistant_chunk_stream_snapshot_for_test(session.id, Duration::from_secs(20))
        .await
        .unwrap();
    assert!(snapshot
        .events
        .iter()
        .all(|event| !matches!(event.event_type, SessionEventType::AssistantChunk)));

    assert!(!snapshot.turns.is_empty());
    assert!(snapshot
        .turns
        .iter()
        .all(|turn| turn.assistant_partial.is_none()));
}
