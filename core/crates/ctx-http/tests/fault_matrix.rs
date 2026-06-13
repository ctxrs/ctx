#![cfg(feature = "fault_injection")]

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::process::Command;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

use ctx_providers::fake::FakeProviderAdapter;

static FAILPOINT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup_git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["init"])
        .output()
        .await
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "user.email", "test@example.com"])
        .output()
        .await
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "user.name", "Test"])
        .output()
        .await
        .unwrap();
    tokio::fs::write(root.join("file.txt"), "hello\n")
        .await
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["add", "."])
        .output()
        .await
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["commit", "-m", "init"])
        .output()
        .await
        .unwrap();
    dir
}

async fn setup_server() -> (
    common::FakeDaemonFixture,
    tokio::task::JoinHandle<()>,
    std::net::SocketAddr,
    ctx_core::models::Workspace,
    ctx_core::models::Session,
    i64,
) {
    let repo = setup_git_repo().await;
    let mut providers: HashMap<String, Arc<dyn ctx_providers::adapters::ProviderAdapter>> =
        HashMap::new();
    providers.insert("fake".into(), Arc::new(FakeProviderAdapter::new()));

    let fixture = common::fake_daemon_fixture_with_providers(providers, "http://127.0.0.1:0").await;
    let app = fixture.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

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
        .json(&json!({"title":"fault-matrix"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let session = common::load_primary_session_http(&client, &base, &task).await;
    let last = fixture
        .daemon
        .seed_fault_matrix_replay_notice_for_test(task.id, session.id)
        .await
        .unwrap();

    (fixture, server, addr, ws, session, last)
}

fn clear_all_failpoints() {
    ctx_http::fault_injection::clear_failpoints();
    ctx_store::fault_injection::clear_failpoints();
}

async fn connect_workspace_stream(
    addr: std::net::SocketAddr,
    workspace_id: ctx_core::ids::WorkspaceId,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let ws_url = format!("ws://{}/api/workspaces/{}/stream", addr, workspace_id.0);
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    socket
}

async fn subscribe_session(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    session_id: ctx_core::ids::SessionId,
    after_seq: i64,
) {
    let subscribe = json!({
        "type": "subscribe",
        "sessions": [{
            "session_id": session_id.0,
            "replay": {
                "mode": "resume",
                "after_seq": after_seq,
            },
        }],
    })
    .to_string();
    socket
        .send(WsMessage::Text(subscribe.clone().into()))
        .await
        .unwrap();
}

async fn wait_for_reset_required_or_disconnect(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    deadline: Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + deadline;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let next =
            tokio::time::timeout(remaining.min(Duration::from_millis(250)), socket.next()).await;
        match next {
            Ok(Some(Ok(WsMessage::Text(txt)))) => {
                let txt_string = txt.to_string();
                let value: serde_json::Value =
                    serde_json::from_str(&txt_string).map_err(|err| err.to_string())?;
                if value.get("type").and_then(|v| v.as_str()) == Some("reset_required") {
                    return Ok(());
                }
            }
            Ok(Some(Ok(WsMessage::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => return Ok(()),
            Ok(Some(Ok(_))) | Err(_) => {}
        }
    }

    Ok(())
}

async fn wait_for_disconnect_without_reset(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    deadline: Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + deadline;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let next =
            tokio::time::timeout(remaining.min(Duration::from_millis(250)), socket.next()).await;
        match next {
            Ok(Some(Ok(WsMessage::Text(txt)))) => {
                let txt_string = txt.to_string();
                let value: serde_json::Value =
                    serde_json::from_str(&txt_string).map_err(|err| err.to_string())?;
                if value.get("type").and_then(|v| v.as_str()) == Some("reset_required") {
                    return Err(format!(
                        "unexpected reset_required while disconnecting: {txt_string}"
                    ));
                }
            }
            Ok(Some(Ok(WsMessage::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => return Ok(()),
            Ok(Some(Ok(_))) | Err(_) => {}
        }
    }

    Ok(())
}

#[tokio::test]
async fn fault_matrix_replay_errors_become_gaps() {
    let _failpoint_guard = FAILPOINT_LOCK.lock().await;
    let (_fixture, server, addr, ws, session, _last_seq) = setup_server().await;

    struct Case {
        name: &'static str,
        setup: fn(),
    }

    let cases = [
        Case {
            name: "replay list fails",
            setup: || {
                ctx_http::fault_injection::clear_failpoints();
                ctx_store::fault_injection::clear_failpoints();
                ctx_http::fault_injection::set_failpoint(
                    "ctx_http.replay_session_events_active.list",
                    1,
                );
            },
        },
        Case {
            name: "replay send fails",
            setup: || {
                clear_all_failpoints();
                ctx_http::fault_injection::set_failpoint(
                    "ctx_http.replay_session_events_active.send",
                    1,
                );
            },
        },
    ];

    for case in cases {
        let mut socket = connect_workspace_stream(addr, ws.id).await;

        (case.setup)();
        subscribe_session(&mut socket, session.id, 0).await;

        if let Err(err) =
            wait_for_reset_required_or_disconnect(&mut socket, Duration::from_secs(10)).await
        {
            panic!("{}: {}", case.name, err);
        }

        clear_all_failpoints();
    }

    server.abort();
}

#[tokio::test]
async fn fault_matrix_snapshot_send_failure_reconnects_cleanly() {
    let _failpoint_guard = FAILPOINT_LOCK.lock().await;
    let (fixture, server, addr, ws, _session, _last_seq) = setup_server().await;
    fixture
        .daemon
        .ensure_workspace_active_snapshot_hydrated(ws.id)
        .await
        .unwrap();

    let ws_url = format!("ws://{}/api/workspaces/{}/stream", addr, ws.id.0);
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    ctx_http::fault_injection::clear_failpoints();
    ctx_store::fault_injection::clear_failpoints();
    ctx_http::fault_injection::set_failpoint("ctx_http.send_workspace_active_snapshot", 1);

    let subscribe = json!({
        "type": "subscribe",
        "scope": "active",
        "include_active_heads": true,
    })
    .to_string();
    socket
        .send(WsMessage::Text(subscribe.clone().into()))
        .await
        .unwrap();

    wait_for_disconnect_without_reset(&mut socket, Duration::from_secs(5))
        .await
        .unwrap();

    ctx_http::fault_injection::clear_failpoints();
    ctx_store::fault_injection::clear_failpoints();

    let (mut retry_socket, _) = connect_async(&ws_url).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), retry_socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    retry_socket
        .send(WsMessage::Text(subscribe.clone().into()))
        .await
        .unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let next = tokio::time::timeout(
            remaining.min(Duration::from_millis(250)),
            retry_socket.next(),
        )
        .await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            if let Ok(ctx_core::models::WorkspaceActiveSnapshotStreamMessage::Snapshot {
                active_snapshot,
                active_heads,
                ..
            }) =
                serde_json::from_str::<ctx_core::models::WorkspaceActiveSnapshotStreamMessage>(&txt)
            {
                assert_eq!(active_snapshot.workspace_id, ws.id);
                assert_eq!(active_snapshot.active.tasks.len(), 1);
                let Some(active_heads) = active_heads else {
                    panic!("expected active heads after reconnect");
                };
                assert_eq!(active_heads.workspace_id, ws.id);
                assert_eq!(active_heads.heads.len(), 1);
                server.abort();
                return;
            }
        }
    }

    server.abort();
    panic!("timed out waiting for snapshot payload after reconnect");
}

#[tokio::test]
async fn fault_matrix_reset_emit_failure_disconnects_stream() {
    let _failpoint_guard = FAILPOINT_LOCK.lock().await;
    let (_fixture, server, addr, ws, session, _last_seq) = setup_server().await;

    let mut socket = connect_workspace_stream(addr, ws.id).await;
    clear_all_failpoints();
    ctx_store::fault_injection::set_failpoint("ctx_store.list_session_events_page_by_seq", 1);
    ctx_http::fault_injection::set_failpoint("ctx_http.send_workspace_active_reset", 1);

    subscribe_session(&mut socket, session.id, 0).await;

    wait_for_disconnect_without_reset(&mut socket, Duration::from_secs(5))
        .await
        .unwrap();

    clear_all_failpoints();
    server.abort();
}
