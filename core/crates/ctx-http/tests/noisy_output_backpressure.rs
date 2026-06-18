use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

use ctx_core::models::{
    SessionEventType, WorkspaceActiveSnapshotEvent, WorkspaceActiveSnapshotStreamMessage,
};

mod common;

const STORAGE_GUARD_EMERGENCY_FREE_BYTES: u64 = 1024 * 1024 * 1024;
const CRP_FIXTURE_FIRST_EVENT_TIMEOUT_MS: &str = "60000";
const STREAM_TAIL_DRAIN: Duration = Duration::from_millis(250);

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.prev.take() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn noisy_tool_output_stays_bounded_end_to_end() {
    let _env_lock = lock_env();

    let Some(python) = common::crp_fixture_runtime::python_binary() else {
        eprintln!("skipping: python3/python not found");
        return;
    };

    let fixtures_dir = common::resolve_manifest_dir()
        .join("tests")
        .join("fixtures")
        .join("provider_scenarios");
    let _guard_fixtures = EnvGuard::set("CTX_TEST_FIXTURES_DIR", &fixtures_dir.to_string_lossy());
    let _guard_scenario = EnvGuard::set("CTX_TEST_SCENARIO", "noisy_tool_output");
    let _guard_first_event_timeout = EnvGuard::set(
        "CTX_CRP_FIRST_EVENT_TIMEOUT_MS",
        CRP_FIXTURE_FIRST_EVENT_TIMEOUT_MS,
    );
    let Some(_guard_ctx_mcp) = common::set_ctx_mcp_command_env_for_test() else {
        eprintln!("skipping: ctx-mcp test binary not found");
        return;
    };

    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let data_dir = tempfile::tempdir().unwrap();
    let min_free_bytes = [
        fs2::available_space(repo.path()).ok(),
        fs2::available_space(data_dir.path()).ok(),
        fs2::available_space(std::env::temp_dir()).ok(),
    ]
    .into_iter()
    .flatten()
    .min()
    .unwrap_or(u64::MAX);
    if min_free_bytes <= STORAGE_GUARD_EMERGENCY_FREE_BYTES {
        eprintln!("skipping: storage guard would trip on low-disk test host");
        return;
    }
    let codex_home = tempfile::tempdir().unwrap();
    tokio::fs::write(
        codex_home.path().join("auth.json"),
        br#"{"OPENAI_API_KEY":"test-key"}"#,
    )
    .await
    .unwrap();
    let _guard_codex_home = EnvGuard::set("CTX_CODEX_HOME", &codex_home.path().to_string_lossy());
    let _guard_mcp_disabled = EnvGuard::set("CTX_MCP_DISABLED", "1");

    let script_path = common::crp_fixture_runtime::write_crp_fixture_runtime(data_dir.path());
    let (runtime_command, runtime_args) =
        common::crp_fixture_runtime::fixture_runtime_invocation(&python, &script_path);
    common::seed_managed_codex_cli_host_runtime_with_args(
        data_dir.path(),
        &runtime_command,
        runtime_args,
    )
    .await;
    let providers =
        common::crp_fixture_runtime::build_crp_fixture_providers(&["codex"], &python, &script_path);
    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        providers,
        "http://127.0.0.1:0",
    )
    .await;
    let daemon = &fixture.daemon;
    let app = fixture.router();
    let server = common::spawn_http_server(app.clone()).await;

    let workspace = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) = common::create_task_with_session(
        &app,
        workspace.id.0,
        "noisy-output",
        "codex",
        "fake-model",
    )
    .await;

    let ws_url = format!(
        "{}/api/workspaces/{}/stream",
        server.base_url, workspace.id.0
    )
    .replace("http://", "ws://");
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
        "sessions": [{
            "session_id": session.id.0,
            "replay": {
                "mode": "resume",
                "after_seq": 0
            }
        }]
    })
    .to_string();
    socket
        .send(WsMessage::Text(subscribe.into()))
        .await
        .unwrap();

    let response = server
        .client
        .post(format!(
            "{}/api/sessions/{}/messages",
            server.base_url, session.id.0
        ))
        .json(&json!({"content": "run a noisy command"}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());

    let persistence_wait = daemon
        .wait_for_noisy_output_persistence_snapshot_for_test(session.id, Duration::from_secs(30));
    tokio::pin!(persistence_wait);
    let mut persistence_result = None;
    let snapshot = loop {
        tokio::select! {
            result = &mut persistence_wait => {
                persistence_result = Some(result.unwrap());
            }
            next = tokio::time::timeout(Duration::from_millis(250), socket.next()) => {
                match next {
                    Ok(Some(Ok(WsMessage::Text(text)))) => {
                        let Ok(message) =
                            serde_json::from_str::<WorkspaceActiveSnapshotStreamMessage>(&text)
                        else {
                            continue;
                        };
                        match message {
                            WorkspaceActiveSnapshotStreamMessage::ResetRequired { .. } => {
                                panic!("unexpected reset_required during noisy command");
                            }
                            WorkspaceActiveSnapshotStreamMessage::Event { event, .. } => {
                                if let WorkspaceActiveSnapshotEvent::SessionGap {
                                    session_id,
                                    ..
                                } = event.as_ref()
                                {
                                    assert_ne!(
                                        *session_id, session.id,
                                        "unexpected session_gap during noisy command"
                                    );
                                }
                            }
                            WorkspaceActiveSnapshotStreamMessage::HeadsBatch { .. }
                            | WorkspaceActiveSnapshotStreamMessage::Snapshot { .. } => {}
                        }
                    }
                    Ok(Some(Ok(WsMessage::Close(_)))) => {
                        panic!("workspace stream closed during noisy command");
                    }
                    Ok(Some(Ok(_))) | Err(_) => {}
                    Ok(Some(Err(err))) => panic!("workspace stream error: {err:?}"),
                    Ok(None) => panic!("workspace stream ended unexpectedly"),
                }
            }
        }
        if let Some(snapshot) = persistence_result.take() {
            let drain_until = tokio::time::Instant::now() + STREAM_TAIL_DRAIN;
            while tokio::time::Instant::now() < drain_until {
                match tokio::time::timeout(Duration::from_millis(10), socket.next()).await {
                    Ok(Some(Ok(WsMessage::Text(text)))) => {
                        let Ok(message) =
                            serde_json::from_str::<WorkspaceActiveSnapshotStreamMessage>(&text)
                        else {
                            continue;
                        };
                        match message {
                            WorkspaceActiveSnapshotStreamMessage::ResetRequired { .. } => {
                                panic!("unexpected reset_required during noisy command");
                            }
                            WorkspaceActiveSnapshotStreamMessage::Event { event, .. } => {
                                if let WorkspaceActiveSnapshotEvent::SessionGap {
                                    session_id, ..
                                } = event.as_ref()
                                {
                                    assert_ne!(
                                        *session_id, session.id,
                                        "unexpected session_gap during noisy command"
                                    );
                                }
                            }
                            WorkspaceActiveSnapshotStreamMessage::HeadsBatch { .. }
                            | WorkspaceActiveSnapshotStreamMessage::Snapshot { .. } => {}
                        }
                    }
                    Ok(Some(Ok(WsMessage::Close(_)))) => {
                        panic!("workspace stream closed during noisy command");
                    }
                    Ok(Some(Ok(_))) | Err(_) => {}
                    Ok(Some(Err(err))) => panic!("workspace stream error: {err:?}"),
                    Ok(None) => panic!("workspace stream ended unexpectedly"),
                }
            }
            break snapshot;
        }
    };

    let events = snapshot.events;
    let messages = snapshot.messages;
    assert!(
        !events.iter().any(|event| {
            matches!(event.event_type, SessionEventType::Notice)
                && event
                    .payload_json
                    .get("kind")
                    .and_then(|value| value.as_str())
                    == Some("session_gap")
        }),
        "unexpected session_gap persisted in noisy scenario: {events:#?}"
    );
    assert!(
        !events.iter().any(|event| {
            matches!(event.event_type, SessionEventType::Notice)
                && event
                    .payload_json
                    .get("kind")
                    .and_then(|value| value.as_str())
                    == Some("crp_unknown_event")
        }),
        "tool output deltas must not degrade into unknown notices: {events:#?}"
    );

    let tool_result = events
        .iter()
        .find(|event| matches!(event.event_type, SessionEventType::ToolResult))
        .unwrap_or_else(|| panic!("missing tool result event: {events:#?}"));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.event_type, SessionEventType::ToolResult))
            .count(),
        1,
        "expected exactly one tool result event: {events:#?}"
    );
    assert_eq!(
        tool_result
            .payload_json
            .get("status")
            .and_then(|value| value.as_str()),
        Some("completed")
    );
    assert!(
        messages.iter().any(|message| {
            matches!(message.role, ctx_core::models::MessageRole::Assistant)
                && message
                    .content
                    .contains("Noisy command completed with bounded output handling.")
        }),
        "expected final assistant message to persist: {messages:#?}"
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| matches!(message.role, ctx_core::models::MessageRole::Assistant))
            .count(),
        1,
        "expected exactly one assistant message: {messages:#?}"
    );
}
