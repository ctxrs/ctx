use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message as WsMessage;

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

fn big_content(bytes: usize) -> String {
    // Deterministic and compresses poorly enough to stay large over JSON.
    // (Some repeated patterns compress well in OS stacks, but JSON encoding is still big.)
    let mut out = String::with_capacity(bytes);
    let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789";
    while out.len() < bytes {
        let idx = out.len() % alphabet.len();
        out.push(alphabet[idx] as char);
    }
    out
}

#[tokio::test]
async fn workspace_stream_does_not_reset_during_hydration_when_active_heads_are_large() {
    // Always-on regression test for hydration lag/overflow behavior.
    // Expected behavior:
    // - With compact active heads (fixed code), the initial snapshot should be small enough that
    //   hydration completes quickly and buffered head deltas do not overflow.
    // - On older baselines (without compaction), hydration can take long enough that the head
    //   batch buffer overflows, causing reset_required.

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

    // Create many sessions to inflate active_heads total payload.
    // This test is a regression guard against huge `active_heads` payloads:
    // we want the initial workspace snapshot to remain bounded even with many
    // active sessions that each have large head windows.
    //
    // We explicitly raise the WS client max message size so older baselines can
    // still be observed (and fail due to payload size / backpressure), rather
    // than failing client-side with `MessageTooLong`.
    // Keep this large enough to overflow old, non-compacted baselines while
    // staying lightweight enough for always-on execution in the full package suite.
    let session_count: usize = 8;
    let turns_per_session: i64 = 24;
    let message_bytes: usize = 16 * 1024;
    let max_snapshot_bytes: usize = 8_000_000;

    let mut sessions: Vec<ctx_core::models::Session> = Vec::with_capacity(session_count);
    for i in 0..session_count {
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

    // Hydrate active snapshot so update_session_head knows which sessions are primary.
    daemon
        .ensure_workspace_active_snapshot_hydrated(ws.id)
        .await
        .unwrap();

    let content = big_content(message_bytes);

    // Seed turns+messages directly in the store to create large session heads.
    for session in &sessions {
        daemon
            .seed_workspace_stream_stress_session_head_for_test(
                session,
                turns_per_session,
                &content,
                200,
            )
            .await
            .unwrap();
    }

    let ws_url = format!("{base}/api/workspaces/{}/stream", ws.id.0).replace("http://", "ws://");
    let mut ws_cfg = WebSocketConfig::default();
    ws_cfg.max_message_size = Some(64 << 20);
    ws_cfg.max_frame_size = Some(64 << 20);
    let (mut socket, _) =
        tokio_tungstenite::connect_async_with_config(&ws_url, Some(ws_cfg), false)
            .await
            .unwrap();

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

    // Publish head deltas until we observe the snapshot. The stream does not flush heads batches
    // while hydrating, so on a slow/large snapshot this should eventually overflow and reset.
    let stop = Arc::new(AtomicBool::new(false));
    let stop_pub = stop.clone();
    let daemon_pub = daemon.clone();
    let ws_id = ws.id;
    let sessions_pub = sessions.clone();

    let publisher = tokio::spawn(async move {
        let mut seq: i64 = 1;
        let mut idx: usize = 0;
        let mut throttle: u32 = 0;
        while !stop_pub.load(Ordering::Relaxed) {
            let session = &sessions_pub[idx % sessions_pub.len()];
            idx += 1;

            daemon_pub
                .publish_workspace_stream_stress_delta_for_test(ws_id, session, seq)
                .await;
            seq += 1;

            // Throttle just enough for the sender task to progress on small snapshots.
            // On baselines with large active_heads, hydration should still take long enough to
            // overflow the head batch buffer.
            throttle = throttle.wrapping_add(1);
            if throttle.is_multiple_of(2) {
                tokio::task::yield_now().await;
            }
        }
    });

    // Wait for the snapshot and fail fast if the server resets while hydrating.
    let mut saw_snapshot = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    while tokio::time::Instant::now() < deadline {
        let next = tokio::time::timeout(Duration::from_millis(250), socket.next()).await;
        match next {
            Ok(Some(Ok(WsMessage::Text(txt)))) => {
                let snapshot_bytes = txt.len();
                let value: Value = serde_json::from_str(&txt).unwrap();
                let msg_type = value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match msg_type {
                    "reset_required" => {
                        panic!("unexpected reset_required while hydrating with active_heads");
                    }
                    "snapshot" => {
                        if snapshot_bytes > max_snapshot_bytes {
                            panic!(
                                "snapshot too large: {snapshot_bytes} bytes (max {max_snapshot_bytes})"
                            );
                        }
                        saw_snapshot = true;
                        break;
                    }
                    _ => {}
                }
            }
            Ok(Some(Ok(WsMessage::Close(_)))) => {
                panic!("workspace stream closed before snapshot");
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(err))) => panic!("workspace stream error: {err:?}"),
            Ok(None) => panic!("workspace stream ended before snapshot"),
            Err(_) => {}
        }
    }

    if !saw_snapshot {
        panic!("timed out waiting for snapshot");
    }

    stop.store(true, Ordering::Relaxed);
    publisher.await.unwrap();

    // Post-snapshot, we should remain live without reset_required.
    let post_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < post_deadline {
        let next = tokio::time::timeout(Duration::from_millis(200), socket.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            let value: Value = serde_json::from_str(&txt).unwrap();
            let msg_type = value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if msg_type == "reset_required" {
                panic!("unexpected reset_required after snapshot");
            }
        }
    }
}
