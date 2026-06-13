use std::time::Duration;

use futures::{future, SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use ctx_daemon::test_support::TestDaemon;

mod common;

const STREAM_TAIL_DRAIN: Duration = Duration::from_millis(250);

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

#[tokio::test]
async fn workspace_stream_stays_live_without_gaps_under_activity() {
    // This test is intentionally moderate: it should be stable in CI but still
    // exercise streaming with tool calls + thought chunks across multiple sessions.
    const SESSION_COUNT: usize = 2;
    const TURNS_PER_SESSION: usize = 1;

    let (repo, _data_dir, daemon, server) = setup().await;
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

    let mut sessions: Vec<ctx_core::models::Session> = Vec::new();
    for i in 0..SESSION_COUNT {
        let task: ctx_core::models::Task = client
            .post(format!("{base}/api/workspaces/{}/tasks", ws.id.0))
            .json(&json!({"title": format!("task-{i}")}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        let session = common::load_primary_session_http(client, base, &task).await;
        sessions.push(session);
    }

    let ws_url = format!("{base}/api/workspaces/{}/stream", ws.id.0).replace("http://", "ws://");
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();

    // Drain the initial server frame (ready).
    let _ = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let subscribe = json!({
        "type": "subscribe",
        "scope": "active",
        "include_active_heads": true,
        "sessions": sessions
            .iter()
            .map(|s| {
                json!({
                    "session_id": s.id.0,
                    "replay": {
                        "mode": "resume",
                        "after_seq": 0,
                    },
                })
            })
            .collect::<Vec<_>>(),
    })
    .to_string();
    socket
        .send(WsMessage::Text(subscribe.into()))
        .await
        .unwrap();

    // Fire activity: multiple turns per session. Fake provider emits assistant chunk,
    // thought chunks (opt-in marker), tool call/result, assistant complete, done.
    let mut senders = Vec::new();
    for (idx, session) in sessions.iter().enumerate() {
        let client = client.clone();
        let base = base.to_string();
        let session_id = session.id.0;
        senders.push(tokio::spawn(async move {
            for j in 0..TURNS_PER_SESSION {
                let content = format!("turn {idx}/{j} emit-thought");
                let _resp: ctx_core::models::Message = client
                    .post(format!("{base}/api/sessions/{session_id}/messages"))
                    .json(&json!({"content": content}))
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
            }
        }));
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let done_daemon = daemon.clone();
    let done_sessions = sessions.clone();
    let activity_done = async move {
        loop {
            if senders.iter().all(|h| h.is_finished()) {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for activity senders to finish");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        for sender in senders {
            sender.await?;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        future::try_join_all(done_sessions.iter().map(|session| {
            done_daemon.wait_for_session_done_event_count_for_test(
                session.id,
                TURNS_PER_SESSION,
                remaining,
            )
        }))
        .await?;
        Ok::<(), anyhow::Error>(())
    };
    tokio::pin!(activity_done);
    let mut activity_result = None;
    loop {
        tokio::select! {
            result = &mut activity_done => {
                activity_result = Some(result);
            }
            next = tokio::time::timeout(Duration::from_millis(250), socket.next()) => {
                match next {
                    Ok(Some(Ok(WsMessage::Text(txt)))) => {
                        let value: Value = serde_json::from_str(&txt).unwrap();
                        let msg_type = value
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        match msg_type {
                            "reset_required" => {
                                panic!("unexpected reset_required while running activity");
                            }
                            "event" => {
                                let Some(event) = value.get("event") else {
                                    continue;
                                };
                                let event_type = event
                                    .get("type")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                if event_type == "session_gap" {
                                    panic!("unexpected session_gap while running activity");
                                }

                                if event_type == "session_head_delta" {}
                            }
                            "heads_batch" => {}
                            "snapshot" => {}
                            _ => {}
                        }
                    }
                    Ok(Some(Ok(WsMessage::Close(_)))) => {
                        panic!("workspace stream closed unexpectedly while running activity");
                    }
                    Ok(Some(Ok(_))) => {}
                    Ok(Some(Err(err))) => {
                        panic!("workspace stream error: {err:?}");
                    }
                    Ok(None) => {
                        panic!("workspace stream ended unexpectedly");
                    }
                    Err(_) => {
                        // no frame in this interval; check completion progress
                    }
                }
            }
        }
        if let Some(result) = activity_result.take() {
            result.unwrap_or_else(|err| {
                panic!("timed out waiting for activity without gaps/reset_required: {err:#}")
            });
            let drain_until = tokio::time::Instant::now() + STREAM_TAIL_DRAIN;
            while tokio::time::Instant::now() < drain_until {
                match tokio::time::timeout(Duration::from_millis(10), socket.next()).await {
                    Ok(Some(Ok(WsMessage::Text(txt)))) => {
                        let value: Value = serde_json::from_str(&txt).unwrap();
                        let msg_type = value
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        match msg_type {
                            "reset_required" => {
                                panic!("unexpected reset_required while running activity");
                            }
                            "event" => {
                                let Some(event) = value.get("event") else {
                                    continue;
                                };
                                let event_type = event
                                    .get("type")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                if event_type == "session_gap" {
                                    panic!("unexpected session_gap while running activity");
                                }
                            }
                            "heads_batch" => {}
                            "snapshot" => {}
                            _ => {}
                        }
                    }
                    Ok(Some(Ok(WsMessage::Close(_)))) => {
                        panic!("workspace stream closed unexpectedly while running activity");
                    }
                    Ok(Some(Ok(_))) | Err(_) => {}
                    Ok(Some(Err(err))) => {
                        panic!("workspace stream error: {err:?}");
                    }
                    Ok(None) => {
                        panic!("workspace stream ended unexpectedly");
                    }
                }
            }
            break;
        }
    }
}
