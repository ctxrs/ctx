use std::path::Path;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::json;

use ctx_core::models::SessionEventType;

mod common;

const LIFECYCLE_EVENT_TIMEOUT: Duration = Duration::from_secs(120);
const STORAGE_GUARD_EMERGENCY_FREE_BYTES: u64 = 1024 * 1024 * 1024;
const QUEUED_MESSAGES_ENABLED_ENV: &str = "CTX_QUEUED_MESSAGES_ENABLED";

fn enable_queued_messages_for_test_binary() {
    static ENABLE: std::sync::Once = std::sync::Once::new();
    ENABLE.call_once(|| std::env::set_var(QUEUED_MESSAGES_ENABLED_ENV, "1"));
}

fn storage_guard_would_trip(repo_path: &Path, data_root: &Path) -> bool {
    [
        fs2::available_space(repo_path).ok(),
        fs2::available_space(data_root).ok(),
        fs2::available_space(std::env::temp_dir()).ok(),
    ]
    .into_iter()
    .flatten()
    .min()
    .unwrap_or(u64::MAX)
        <= STORAGE_GUARD_EMERGENCY_FREE_BYTES
}

#[tokio::test]
async fn queued_message_emits_lifecycle_events_in_order_with_interrupt() {
    enable_queued_messages_for_test_binary();
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let data_dir = tempfile::tempdir().unwrap();
    if storage_guard_would_trip(repo.path(), data_dir.path()) {
        eprintln!("skipping: storage guard would trip on low-disk test host");
        return;
    }
    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        common::fake_providers(),
        "http://127.0.0.1:0",
    )
    .await;
    let daemon = &fixture.daemon;
    let app = fixture.router();

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "t1", "fake", "fake-model").await;

    let (status, msg1): (StatusCode, ctx_core::models::Message) = common::json_request(
        &app,
        Method::POST,
        format!("/api/sessions/{}/messages", session.id.0),
        Some(json!({"content":"first slow-diff-test"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, msg2): (StatusCode, ctx_core::models::Message) = common::json_request(
        &app,
        Method::POST,
        format!("/api/sessions/{}/messages", session.id.0),
        Some(json!({"content":"second","delivery":"queued"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let turn_id_one = msg1.turn_id.expect("first turn id");
    let turn_id_two = msg2.turn_id.expect("second turn id");

    daemon
        .wait_for_scheduler_runtime_events_for_test(
            session.id,
            LIFECYCLE_EVENT_TIMEOUT,
            "lifecycle events",
            |events| {
                let saw_started = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_one)
                        && matches!(event.event_type, SessionEventType::ToolCall)
                });
                let saw_queued = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_two)
                        && matches!(event.event_type, SessionEventType::TurnQueued)
                });
                Ok(saw_started && saw_queued)
            },
        )
        .await
        .unwrap();

    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/sessions/{}/interrupt", session.id.0))
        .body(Body::empty())
        .unwrap();
    let (status, _) = common::oneshot_bytes(&app, req).await;
    assert_eq!(status, StatusCode::OK);

    let events = daemon
        .wait_for_scheduler_runtime_events_for_test(
            session.id,
            LIFECYCLE_EVENT_TIMEOUT,
            "interrupt + queue lifecycle events",
            |events| {
                let saw_interrupted = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_one)
                        && matches!(event.event_type, SessionEventType::TurnInterrupted)
                });
                let saw_finished = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_one)
                        && matches!(event.event_type, SessionEventType::TurnFinished)
                });
                let saw_queue_lifecycle = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_two)
                        && matches!(event.event_type, SessionEventType::InputQueued)
                }) && events.iter().any(|event| {
                    event.turn_id == Some(turn_id_two)
                        && matches!(event.event_type, SessionEventType::MessageQueueAdded)
                }) && events.iter().any(|event| {
                    event.turn_id == Some(turn_id_two)
                        && matches!(event.event_type, SessionEventType::TurnQueued)
                });
                Ok(saw_interrupted && saw_finished && saw_queue_lifecycle)
            },
        )
        .await
        .unwrap();
    let seq_for = |turn_id, event_type| {
        let target = std::mem::discriminant(&event_type);
        events
            .iter()
            .find(|event| {
                event.turn_id == Some(turn_id)
                    && std::mem::discriminant(&event.event_type) == target
            })
            .map(|event| event.seq)
            .expect("expected event seq")
    };

    let user_seq = seq_for(turn_id_two, SessionEventType::UserMessage);
    let input_seq = seq_for(turn_id_two, SessionEventType::InputQueued);
    let queue_seq = seq_for(turn_id_two, SessionEventType::MessageQueueAdded);
    let turn_seq = seq_for(turn_id_two, SessionEventType::TurnQueued);
    assert!(user_seq < input_seq && input_seq < queue_seq && queue_seq < turn_seq);

    let started_seq = seq_for(turn_id_one, SessionEventType::ToolCall);
    let interrupted_seq = seq_for(turn_id_one, SessionEventType::TurnInterrupted);
    let finished_seq = seq_for(turn_id_one, SessionEventType::TurnFinished);
    assert!(started_seq < interrupted_seq && interrupted_seq < finished_seq);

    let finished = events
        .iter()
        .find(|event| {
            event.turn_id == Some(turn_id_one)
                && matches!(event.event_type, SessionEventType::TurnFinished)
        })
        .expect("expected finished event");
    assert_eq!(
        finished
            .payload_json
            .get("status")
            .and_then(|value| value.as_str()),
        Some("interrupted")
    );
}

#[tokio::test]
async fn cancel_promotes_next_queued_turn_after_interrupted_finish() {
    enable_queued_messages_for_test_binary();
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let data_dir = tempfile::tempdir().unwrap();
    if storage_guard_would_trip(repo.path(), data_dir.path()) {
        eprintln!("skipping: storage guard would trip on low-disk test host");
        return;
    }
    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        common::fake_providers(),
        "http://127.0.0.1:0",
    )
    .await;
    let daemon = &fixture.daemon;
    let app = fixture.router();

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "t1", "fake", "fake-model").await;

    let (status, msg1): (StatusCode, ctx_core::models::Message) = common::json_request(
        &app,
        Method::POST,
        format!("/api/sessions/{}/messages", session.id.0),
        Some(json!({"content":"first slow-diff-test"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, msg2): (StatusCode, ctx_core::models::Message) = common::json_request(
        &app,
        Method::POST,
        format!("/api/sessions/{}/messages", session.id.0),
        Some(json!({"content":"second","delivery":"queued"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let turn_id_one = msg1.turn_id.expect("first turn id");
    let turn_id_two = msg2.turn_id.expect("second turn id");

    daemon
        .wait_for_scheduler_runtime_events_for_test(
            session.id,
            LIFECYCLE_EVENT_TIMEOUT,
            "turn start + queue events",
            |events| {
                let saw_started = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_one)
                        && matches!(event.event_type, SessionEventType::ToolCall)
                });
                let saw_queued = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_two)
                        && matches!(event.event_type, SessionEventType::TurnQueued)
                });
                Ok(saw_started && saw_queued)
            },
        )
        .await
        .unwrap();

    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/sessions/{}/cancel", session.id.0))
        .body(Body::empty())
        .unwrap();
    let (status, _) = common::oneshot_bytes(&app, req).await;
    assert_eq!(status, StatusCode::OK);

    let events = daemon
        .wait_for_scheduler_runtime_events_for_test(
            session.id,
            LIFECYCLE_EVENT_TIMEOUT,
            "cancel promotion",
            |events| {
                let saw_finished = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_one)
                        && matches!(event.event_type, SessionEventType::TurnFinished)
                });
                let saw_promoted = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_two)
                        && matches!(event.event_type, SessionEventType::MessageQueuePromoted)
                });
                let saw_next_started = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_two)
                        && matches!(event.event_type, SessionEventType::ToolCall)
                });
                Ok(saw_finished && saw_promoted && saw_next_started)
            },
        )
        .await
        .unwrap();
    let seq_for = |turn_id, event_type| {
        let target = std::mem::discriminant(&event_type);
        events
            .iter()
            .find(|event| {
                event.turn_id == Some(turn_id)
                    && std::mem::discriminant(&event.event_type) == target
            })
            .map(|event| event.seq)
            .expect("expected event seq")
    };

    let interrupted_seq = seq_for(turn_id_one, SessionEventType::TurnInterrupted);
    let finished_seq = seq_for(turn_id_one, SessionEventType::TurnFinished);
    let promoted_seq = seq_for(turn_id_two, SessionEventType::MessageQueuePromoted);
    let started_seq = seq_for(turn_id_two, SessionEventType::ToolCall);

    assert!(interrupted_seq < finished_seq);
    assert!(finished_seq < promoted_seq);
    assert!(promoted_seq < started_seq);
}

#[tokio::test]
async fn cancel_promotes_queued_turns_in_fifo_order_across_multiple_cancels() {
    enable_queued_messages_for_test_binary();
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let data_dir = tempfile::tempdir().unwrap();
    if storage_guard_would_trip(repo.path(), data_dir.path()) {
        eprintln!("skipping: storage guard would trip on low-disk test host");
        return;
    }
    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        common::fake_providers(),
        "http://127.0.0.1:0",
    )
    .await;
    let daemon = &fixture.daemon;
    let app = fixture.router();

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "t1", "fake", "fake-model").await;

    let (status, msg1): (StatusCode, ctx_core::models::Message) = common::json_request(
        &app,
        Method::POST,
        format!("/api/sessions/{}/messages", session.id.0),
        Some(json!({"content":"first slow-diff-test"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, msg2): (StatusCode, ctx_core::models::Message) = common::json_request(
        &app,
        Method::POST,
        format!("/api/sessions/{}/messages", session.id.0),
        Some(json!({"content":"second slow-diff-test","delivery":"queued"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, msg3): (StatusCode, ctx_core::models::Message) = common::json_request(
        &app,
        Method::POST,
        format!("/api/sessions/{}/messages", session.id.0),
        Some(json!({"content":"third","delivery":"queued"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let turn_id_one = msg1.turn_id.expect("first turn id");
    let turn_id_two = msg2.turn_id.expect("second turn id");
    let turn_id_three = msg3.turn_id.expect("third turn id");

    let events = daemon
        .wait_for_scheduler_runtime_events_for_test(
            session.id,
            LIFECYCLE_EVENT_TIMEOUT,
            "queue chain",
            |events| {
                let saw_started = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_one)
                        && matches!(event.event_type, SessionEventType::ToolCall)
                });
                let saw_second_queued = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_two)
                        && matches!(event.event_type, SessionEventType::TurnQueued)
                });
                let saw_third_queued = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_three)
                        && matches!(event.event_type, SessionEventType::TurnQueued)
                });
                Ok(saw_started && saw_second_queued && saw_third_queued)
            },
        )
        .await
        .unwrap();
    let payload_i64_for = |turn_id, event_type, key| {
        let target = std::mem::discriminant(&event_type);
        events
            .iter()
            .find(|event| {
                event.turn_id == Some(turn_id)
                    && std::mem::discriminant(&event.event_type) == target
            })
            .and_then(|event| event.payload_json.get(key))
            .and_then(|value| value.as_i64())
    };
    assert_eq!(
        payload_i64_for(
            turn_id_two,
            SessionEventType::MessageQueueAdded,
            "queue_position"
        ),
        Some(0)
    );
    assert_eq!(
        payload_i64_for(
            turn_id_three,
            SessionEventType::MessageQueueAdded,
            "queue_position"
        ),
        Some(1)
    );
    assert_eq!(
        payload_i64_for(turn_id_two, SessionEventType::TurnQueued, "queue_position"),
        Some(0)
    );
    assert_eq!(
        payload_i64_for(
            turn_id_three,
            SessionEventType::TurnQueued,
            "queue_position"
        ),
        Some(1)
    );

    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/sessions/{}/cancel", session.id.0))
        .body(Body::empty())
        .unwrap();
    let (status, _) = common::oneshot_bytes(&app, req).await;
    assert_eq!(status, StatusCode::OK);

    daemon
        .wait_for_scheduler_runtime_events_for_test(
            session.id,
            LIFECYCLE_EVENT_TIMEOUT,
            "first cancel promotion",
            |events| {
                let first_finished = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_one)
                        && matches!(event.event_type, SessionEventType::TurnFinished)
                });
                let second_promoted = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_two)
                        && matches!(event.event_type, SessionEventType::MessageQueuePromoted)
                });
                let second_started = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_two)
                        && matches!(event.event_type, SessionEventType::ToolCall)
                });
                Ok(first_finished && second_promoted && second_started)
            },
        )
        .await
        .unwrap();

    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/sessions/{}/cancel", session.id.0))
        .body(Body::empty())
        .unwrap();
    let (status, _) = common::oneshot_bytes(&app, req).await;
    assert_eq!(status, StatusCode::OK);

    let events = daemon
        .wait_for_scheduler_runtime_events_for_test(
            session.id,
            LIFECYCLE_EVENT_TIMEOUT,
            "second cancel promotion",
            |events| {
                let second_finished = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_two)
                        && matches!(event.event_type, SessionEventType::TurnFinished)
                });
                let third_promoted = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_three)
                        && matches!(event.event_type, SessionEventType::MessageQueuePromoted)
                });
                let third_started = events.iter().any(|event| {
                    event.turn_id == Some(turn_id_three)
                        && matches!(event.event_type, SessionEventType::ToolCall)
                });
                Ok(second_finished && third_promoted && third_started)
            },
        )
        .await
        .unwrap();
    let seq_for = |turn_id, event_type| {
        let target = std::mem::discriminant(&event_type);
        events
            .iter()
            .find(|event| {
                event.turn_id == Some(turn_id)
                    && std::mem::discriminant(&event.event_type) == target
            })
            .map(|event| event.seq)
            .expect("expected event seq")
    };
    let payload_i64_for = |turn_id, event_type, key| {
        let target = std::mem::discriminant(&event_type);
        events
            .iter()
            .find(|event| {
                event.turn_id == Some(turn_id)
                    && std::mem::discriminant(&event.event_type) == target
            })
            .and_then(|event| event.payload_json.get(key))
            .and_then(|value| value.as_i64())
    };

    let first_interrupted_seq = seq_for(turn_id_one, SessionEventType::TurnInterrupted);
    let first_finished_seq = seq_for(turn_id_one, SessionEventType::TurnFinished);
    let second_promoted_seq = seq_for(turn_id_two, SessionEventType::MessageQueuePromoted);
    let second_started_seq = seq_for(turn_id_two, SessionEventType::ToolCall);
    let second_interrupted_seq = seq_for(turn_id_two, SessionEventType::TurnInterrupted);
    let second_finished_seq = seq_for(turn_id_two, SessionEventType::TurnFinished);
    let third_promoted_seq = seq_for(turn_id_three, SessionEventType::MessageQueuePromoted);
    let third_started_seq = seq_for(turn_id_three, SessionEventType::ToolCall);

    assert!(first_interrupted_seq < first_finished_seq);
    assert!(first_finished_seq < second_promoted_seq);
    assert!(second_promoted_seq < second_started_seq);

    assert!(second_interrupted_seq < second_finished_seq);
    assert!(second_finished_seq < third_promoted_seq);
    assert!(third_promoted_seq < third_started_seq);
    assert!(second_promoted_seq < third_promoted_seq);

    let promoted_turns: Vec<_> = events
        .iter()
        .filter(|event| matches!(event.event_type, SessionEventType::MessageQueuePromoted))
        .map(|event| event.turn_id)
        .collect();
    assert_eq!(promoted_turns, vec![Some(turn_id_two), Some(turn_id_three)]);
    assert_eq!(
        payload_i64_for(
            turn_id_two,
            SessionEventType::MessageQueuePromoted,
            "previous_position"
        ),
        Some(0)
    );
    assert_eq!(
        payload_i64_for(
            turn_id_three,
            SessionEventType::MessageQueuePromoted,
            "previous_position",
        ),
        Some(0)
    );
}
