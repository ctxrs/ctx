#![cfg(feature = "fault_injection")]

use std::time::Duration;

use axum::http::{Method, StatusCode};
use ctx_core::models::{Message, SessionEventType, SessionTurnStatus};
use serde_json::json;

mod common;

fn clear_all_failpoints() {
    ctx_http::fault_injection::clear_failpoints();
    ctx_store::fault_injection::clear_failpoints();
}

async fn post_message(app: &axum::Router, session_id: uuid::Uuid, content: &str) -> Message {
    let (status, msg): (StatusCode, Message) = common::json_request(
        app,
        Method::POST,
        format!("/api/sessions/{session_id}/messages"),
        Some(json!({"content": content})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    msg
}

#[tokio::test]
async fn assistant_message_persistence_faults_recover_or_fail_honestly() {
    clear_all_failpoints();
    {
        let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
        let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
        let daemon = &fixture.daemon;
        let app = fixture.router();

        ctx_http::fault_injection::set_failpoint("ctx_http.persist_assistant_message.transient", 1);

        let ws = common::create_workspace(&app, repo.path(), "ws").await;
        let (_task, session) =
            common::create_task_with_session(&app, ws.id.0, "t1", "fake", "fake-model").await;
        let user_message = post_message(&app, session.id.0, "retry me").await;
        let turn_id = user_message.turn_id.expect("turn id");

        let snapshot = daemon
            .wait_for_terminal_turn_persistence_snapshot_for_test(
                session.id,
                turn_id,
                Duration::from_secs(120),
            )
            .await
            .unwrap();
        assert_eq!(snapshot.turn.status, SessionTurnStatus::Completed);
        assert!(
            !snapshot
                .events
                .iter()
                .any(|event| matches!(event.event_type, SessionEventType::Error)),
            "transient persistence failure should recover cleanly: {:#?}",
            snapshot.events
        );
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| matches!(
                    event.event_type,
                    SessionEventType::AssistantMessageInserted
                ))
                .count(),
            1,
            "expected exactly one inserted assistant message after retry: {:#?}",
            snapshot.events
        );

        assert_eq!(
            snapshot.assistant_messages.len(),
            1,
            "assistant message should persist once"
        );
        assert_eq!(snapshot.assistant_messages[0].content, "done: retry me");

        let turn_finished_statuses = snapshot
            .events
            .iter()
            .filter(|event| matches!(event.event_type, SessionEventType::TurnFinished))
            .map(|event| {
                event
                    .payload_json
                    .get("status")
                    .and_then(|value| value.as_str())
                    .unwrap_or("<missing>")
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(turn_finished_statuses, vec!["completed".to_string()]);

        clear_all_failpoints();
    }

    {
        let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
        let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
        let daemon = &fixture.daemon;
        let app = fixture.router();

        let ws = common::create_workspace(&app, repo.path(), "ws").await;
        let (_task, session) =
            common::create_task_with_session(&app, ws.id.0, "t1", "fake", "fake-model").await;
        let user_message = post_message(&app, session.id.0, "retry after partial write").await;
        ctx_store::fault_injection::set_failpoint("ctx_store.insert_message.after_insert", 1);
        let turn_id = user_message.turn_id.expect("turn id");

        let snapshot = daemon
            .wait_for_terminal_turn_persistence_snapshot_for_test(
                session.id,
                turn_id,
                Duration::from_secs(120),
            )
            .await
            .unwrap();
        assert_eq!(snapshot.turn.status, SessionTurnStatus::Completed);
        assert!(
            !snapshot
                .events
                .iter()
                .any(|event| matches!(event.event_type, SessionEventType::Error)),
            "post-insert transient persistence failure should recover cleanly: {:#?}",
            snapshot.events
        );
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| matches!(
                    event.event_type,
                    SessionEventType::AssistantMessageInserted
                ))
                .count(),
            1,
            "expected exactly one inserted assistant message after transactional retry: {:#?}",
            snapshot.events
        );

        assert_eq!(
            snapshot.assistant_messages.len(),
            1,
            "assistant message should not be duplicated after post-insert retry"
        );
        assert_eq!(
            snapshot.assistant_messages[0].content,
            "done: retry after partial write"
        );

        clear_all_failpoints();
    }

    {
        let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
        let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
        let daemon = &fixture.daemon;
        let app = fixture.router();

        ctx_http::fault_injection::set_failpoint("ctx_http.persist_assistant_message.fatal", 1);

        let ws = common::create_workspace(&app, repo.path(), "ws").await;
        let (_task, session) =
            common::create_task_with_session(&app, ws.id.0, "t1", "fake", "fake-model").await;
        let user_message = post_message(&app, session.id.0, "fail me").await;
        let turn_id = user_message.turn_id.expect("turn id");

        let snapshot = daemon
            .wait_for_terminal_turn_persistence_snapshot_for_test(
                session.id,
                turn_id,
                Duration::from_secs(120),
            )
            .await
            .unwrap();
        assert_eq!(snapshot.turn.status, SessionTurnStatus::Failed);
        assert!(
            snapshot.events.iter().any(|event| {
                matches!(event.event_type, SessionEventType::TurnFinished)
                    && event
                        .payload_json
                        .get("status")
                        .and_then(|value| value.as_str())
                        == Some("failed")
            }),
            "fatal assistant persistence failure must surface as a failed turn_finished event: {:#?}",
            snapshot.events
        );
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| matches!(
                    event.event_type,
                    SessionEventType::AssistantMessageInserted
                ))
                .count(),
            0,
            "assistant message insert should fail in this scenario: {:#?}",
            snapshot.events
        );

        let turn_finished_statuses = snapshot
            .events
            .iter()
            .filter(|event| matches!(event.event_type, SessionEventType::TurnFinished))
            .map(|event| {
                event
                    .payload_json
                    .get("status")
                    .and_then(|value| value.as_str())
                    .unwrap_or("<missing>")
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            turn_finished_statuses,
            vec!["failed".to_string()],
            "fatal assistant persistence failure must not emit a completed TurnFinished event: {:#?}",
            snapshot.events
        );

        assert!(
            snapshot.assistant_messages.is_empty(),
            "no assistant message should be persisted when the fatal failpoint is armed"
        );

        clear_all_failpoints();
    }
}
