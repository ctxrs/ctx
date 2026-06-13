use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

use ctx_daemon::test_support::TestDaemon;

mod common;

async fn setup() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    TestDaemon,
    common::TestServer,
) {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;

    (repo, fixture.data_dir, fixture.daemon, server)
}

fn deltas_from_stream_message(value: &Value) -> Vec<Value> {
    match value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "event" => {
            let Some(event) = value.get("event") else {
                return Vec::new();
            };
            if event.get("type").and_then(Value::as_str) != Some("session_head_delta") {
                return Vec::new();
            }
            event.get("delta").cloned().into_iter().collect()
        }
        "heads_batch" => value
            .get("deltas")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn assert_canonical_context_window(delta: &Value) {
    let turn = delta
        .get("turn")
        .and_then(Value::as_object)
        .expect("expected turn payload on terminal delta");
    let metrics = turn
        .get("metrics_json")
        .and_then(Value::as_object)
        .expect("expected canonical metrics_json on streamed turn");
    assert_eq!(
        metrics.get("context_window_tokens").and_then(Value::as_u64),
        Some(100)
    );
    assert_eq!(
        metrics
            .get("context_tokens_estimate")
            .and_then(Value::as_u64),
        Some(7)
    );
    assert_eq!(
        metrics
            .get("remaining_tokens_estimate")
            .and_then(Value::as_u64),
        Some(93)
    );
    let remaining_fraction = metrics
        .get("remaining_fraction")
        .and_then(Value::as_f64)
        .expect("expected remaining_fraction");
    assert!(
        (remaining_fraction - 0.93).abs() < 1e-9,
        "unexpected remaining_fraction: {remaining_fraction}"
    );
}

fn assert_live_context_window(delta: &Value) {
    let turn = delta
        .get("turn")
        .and_then(Value::as_object)
        .expect("expected turn payload on live delta");
    let metrics = turn
        .get("metrics_json")
        .and_then(Value::as_object)
        .expect("expected metrics_json on live delta");
    assert_eq!(
        metrics.get("context_window_tokens").and_then(Value::as_u64),
        Some(100)
    );
    assert_eq!(
        metrics
            .get("context_tokens_estimate")
            .and_then(Value::as_u64),
        Some(25)
    );
    assert_eq!(
        metrics
            .get("remaining_tokens_estimate")
            .and_then(Value::as_u64),
        Some(75)
    );
    let remaining_fraction = metrics
        .get("remaining_fraction")
        .and_then(Value::as_f64)
        .expect("expected remaining_fraction");
    assert!(
        (remaining_fraction - 0.75).abs() < 1e-9,
        "unexpected remaining_fraction: {remaining_fraction}"
    );
}

fn delta_has_live_context_window(delta: &Value) -> bool {
    let Some(metrics) = delta
        .get("turn")
        .and_then(Value::as_object)
        .and_then(|turn| turn.get("metrics_json"))
        .and_then(Value::as_object)
    else {
        return false;
    };
    metrics.get("context_window_tokens").and_then(Value::as_u64) == Some(100)
        && metrics
            .get("context_tokens_estimate")
            .and_then(Value::as_u64)
            == Some(25)
        && metrics
            .get("remaining_tokens_estimate")
            .and_then(Value::as_u64)
            == Some(75)
}

fn task_request_body() -> Value {
    json!({
        "title": "task",
        "default_session": {
            "provider_id": "fake",
            "model_id": "fake-model",
        },
    })
}

#[tokio::test]
async fn workspace_stream_done_delta_carries_context_window_metrics() {
    let (repo, _data_dir, _state, server) = setup().await;
    let base = &server.base_url;
    let client = &server.client;

    let ws: ctx_core::models::Workspace = client
        .post(format!("{base}/api/workspaces"))
        .json(&json!({"root_path": repo.path(), "name": "ws"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let task: ctx_core::models::Task = client
        .post(format!("{base}/api/workspaces/{}/tasks", ws.id.0))
        .json(&task_request_body())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let session = common::load_primary_session_http(client, base, &task).await;

    let ws_url = format!("{base}/api/workspaces/{}/stream", ws.id.0).replace("http://", "ws://");
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();

    let _ = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let subscribe = json!({
        "type": "subscribe",
        "scope": "active",
        "include_active_heads": true,
        "foreground_session_id": session.id.0,
        "sessions": [{
            "session_id": session.id.0,
            "replay": {
                "mode": "resume",
                "after_seq": 0,
            },
        }],
    })
    .to_string();
    socket
        .send(WsMessage::Text(subscribe.into()))
        .await
        .unwrap();

    let message: ctx_core::models::Message = client
        .post(format!("{base}/api/sessions/{}/messages", session.id.0))
        .json(&json!({"content":"slow-diff-test 0123456789"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let turn_id = message.turn_id.expect("expected turn id").0.to_string();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut saw_done = false;
    let mut saw_turn_finished = false;

    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let next =
            tokio::time::timeout(remaining.min(Duration::from_millis(250)), socket.next()).await;
        match next {
            Ok(Some(Ok(WsMessage::Text(txt)))) => {
                let value: Value = serde_json::from_str(&txt).unwrap();
                for delta in deltas_from_stream_message(&value) {
                    let session_id = delta
                        .get("session_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if session_id != session.id.0.to_string() {
                        continue;
                    }
                    let Some(event) = delta.get("event").and_then(Value::as_object) else {
                        continue;
                    };
                    let event_turn_id = event
                        .get("turn_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if event_turn_id != turn_id {
                        continue;
                    }
                    match event
                        .get("event_type")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                    {
                        "done" => {
                            assert_canonical_context_window(&delta);
                            saw_done = true;
                        }
                        "turn_finished" => {
                            assert_canonical_context_window(&delta);
                            saw_turn_finished = true;
                        }
                        _ => {}
                    }
                }
                if saw_done && saw_turn_finished {
                    break;
                }
            }
            Ok(Some(Ok(WsMessage::Close(_)))) => {
                panic!("workspace stream closed unexpectedly");
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(err))) => panic!("workspace stream error: {err:?}"),
            Ok(None) => panic!("workspace stream ended unexpectedly"),
            Err(_) => {}
        }
    }

    assert!(
        saw_done,
        "expected done delta with canonical context-window metrics"
    );
    assert!(
        saw_turn_finished,
        "expected turn_finished delta with canonical context-window metrics"
    );
}

#[tokio::test]
async fn workspace_stream_live_context_window_delta_arrives_before_done() {
    let (repo, _data_dir, _state, server) = setup().await;
    let base = &server.base_url;
    let client = &server.client;

    let ws: ctx_core::models::Workspace = client
        .post(format!("{base}/api/workspaces"))
        .json(&json!({"root_path": repo.path(), "name": "ws"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let task: ctx_core::models::Task = client
        .post(format!("{base}/api/workspaces/{}/tasks", ws.id.0))
        .json(&task_request_body())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let session = common::load_primary_session_http(client, base, &task).await;

    let ws_url = format!("{base}/api/workspaces/{}/stream", ws.id.0).replace("http://", "ws://");
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();

    let _ = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let subscribe = json!({
        "type": "subscribe",
        "scope": "active",
        "include_active_heads": true,
        "foreground_session_id": session.id.0,
        "sessions": [{
            "session_id": session.id.0,
            "replay": {
                "mode": "resume",
                "after_seq": 0,
            },
        }],
    })
    .to_string();
    socket
        .send(WsMessage::Text(subscribe.into()))
        .await
        .unwrap();

    let message: ctx_core::models::Message = client
        .post(format!("{base}/api/sessions/{}/messages", session.id.0))
        .json(&json!({"content":"slow-diff-test emit-live-context-window"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let turn_id = message.turn_id.expect("expected turn id").0.to_string();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut saw_live = false;
    let mut saw_done = false;

    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let next =
            tokio::time::timeout(remaining.min(Duration::from_millis(250)), socket.next()).await;
        match next {
            Ok(Some(Ok(WsMessage::Text(txt)))) => {
                let value: Value = serde_json::from_str(&txt).unwrap();
                for delta in deltas_from_stream_message(&value) {
                    let session_id = delta
                        .get("session_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if session_id != session.id.0.to_string() {
                        continue;
                    }
                    let Some(event) = delta.get("event").and_then(Value::as_object) else {
                        continue;
                    };
                    let event_turn_id = event
                        .get("turn_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if event_turn_id != turn_id {
                        continue;
                    }
                    if !saw_done && delta_has_live_context_window(&delta) {
                        assert_live_context_window(&delta);
                        saw_live = true;
                    }
                    match event
                        .get("event_type")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                    {
                        "context_window_update" => {
                            assert!(
                                !saw_done,
                                "live context-window delta arrived after terminal done"
                            );
                        }
                        "done" => {
                            saw_done = true;
                        }
                        _ => {}
                    }
                }
                if saw_live && saw_done {
                    break;
                }
            }
            Ok(Some(Ok(WsMessage::Close(_)))) => {
                panic!("workspace stream closed unexpectedly");
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(err))) => panic!("workspace stream error: {err:?}"),
            Ok(None) => panic!("workspace stream ended unexpectedly"),
            Err(_) => {}
        }
    }

    assert!(
        saw_live,
        "expected live context-window delta before terminal completion"
    );
    assert!(saw_done, "expected terminal done delta");
}
