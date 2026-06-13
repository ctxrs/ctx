use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

use ctx_core::models::SessionEventType;
use ctx_daemon::test_support::provider_scenarios::ProviderScenarioTurnSnapshot;

mod common;

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
const STORAGE_GUARD_EMERGENCY_FREE_BYTES: u64 = 1024 * 1024 * 1024;
const CRP_FIXTURE_FIRST_EVENT_TIMEOUT_MS: &str = "60000";

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

fn host_has_sufficient_free_space(paths: &[&std::path::Path]) -> bool {
    let min_free_bytes = paths
        .iter()
        .filter_map(|path| fs2::available_space(path).ok())
        .min()
        .unwrap_or(u64::MAX);
    min_free_bytes > STORAGE_GUARD_EMERGENCY_FREE_BYTES
}

async fn configure_hermetic_codex_home() -> (tempfile::TempDir, EnvGuard) {
    let codex_home = tempfile::tempdir().unwrap();
    tokio::fs::write(
        codex_home.path().join("auth.json"),
        br#"{"OPENAI_API_KEY":"test-key"}"#,
    )
    .await
    .unwrap();
    let guard = EnvGuard::set("CTX_CODEX_HOME", &codex_home.path().to_string_lossy());
    (codex_home, guard)
}

async fn fixture_model_id_for_provider(
    app: &axum::Router,
    workspace_id: uuid::Uuid,
    provider_id: &str,
) -> String {
    let fallback_model_id = match provider_id {
        "codex" => Some("gpt-5.4/medium"),
        "claude-crp" => Some("default/medium"),
        _ => None,
    };
    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        app,
        Method::GET,
        format!("/api/workspaces/{workspace_id}/providers/{provider_id}/options"),
        None,
    )
    .await;
    if status != StatusCode::OK {
        return fallback_model_id.unwrap_or("fake-model").to_string();
    }
    body.pointer("/models/current_model_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            body.pointer("/models/models")
                .and_then(serde_json::Value::as_array)
                .and_then(|models| models.first())
                .and_then(|model| model.get("id"))
                .and_then(serde_json::Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or(fallback_model_id)
        .unwrap_or("fake-model")
        .to_string()
}

async fn post_message(app: &axum::Router, session_id: uuid::Uuid, content: &str) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/sessions/{session_id}/messages"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "content": content }).to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

fn check_no_session_gap(events: &[ctx_core::models::SessionEvent]) -> Result<(), String> {
    let saw_gap = events.iter().any(|event| {
        matches!(event.event_type, SessionEventType::Notice)
            && event
                .payload_json
                .get("kind")
                .and_then(|value| value.as_str())
                == Some("session_gap")
    });
    if saw_gap {
        Err(format!("unexpected session_gap event: {events:#?}"))
    } else {
        Ok(())
    }
}

fn count_events(events: &[ctx_core::models::SessionEvent], event_type: SessionEventType) -> usize {
    let target = std::mem::discriminant(&event_type);
    events
        .iter()
        .filter(|e| std::mem::discriminant(&e.event_type) == target)
        .count()
}

fn check_event_count_at_least(
    events: &[ctx_core::models::SessionEvent],
    event_type: SessionEventType,
    min_count: usize,
) -> Result<(), String> {
    let count = count_events(events, event_type.clone());
    if count < min_count {
        Err(format!(
            "expected at least {min_count} {event_type:?} events; saw {count}: {events:#?}"
        ))
    } else {
        Ok(())
    }
}

fn check_assistant_message_inserted_contains(
    events: &[ctx_core::models::SessionEvent],
    expected: &str,
) -> Result<(), String> {
    let saw = events.iter().any(|e| {
        matches!(e.event_type, SessionEventType::AssistantMessageInserted)
            && e.payload_json
                .get("content")
                .and_then(|v| v.as_str())
                .is_some_and(|v| v.contains(expected))
    });
    if saw {
        Ok(())
    } else {
        Err(format!(
            "expected AssistantMessageInserted to contain {expected:?}; events: {events:#?}"
        ))
    }
}

fn check_event_count_exact(
    events: &[ctx_core::models::SessionEvent],
    event_type: SessionEventType,
    expected_count: usize,
) -> Result<(), String> {
    let count = count_events(events, event_type.clone());
    if count != expected_count {
        Err(format!(
            "expected exactly {expected_count} {event_type:?} events; saw {count}: {events:#?}"
        ))
    } else {
        Ok(())
    }
}

fn check_turn_thought_partial_contains(
    turns: &[ProviderScenarioTurnSnapshot],
    expected: &str,
) -> Result<(), String> {
    let Some(last) = turns.last() else {
        return Err("expected at least one turn; saw none".to_string());
    };
    let thought = last.thought_partial.as_deref().unwrap_or("");
    if thought.contains(expected) {
        Ok(())
    } else {
        Err(format!(
            "expected thought_partial to contain {expected:?}; saw {thought:?}; turns: {turns:#?}"
        ))
    }
}

fn read_event_order_seq(event: &ctx_core::models::SessionEvent) -> Option<i64> {
    event
        .payload_json
        .get("order_seq")
        .or_else(|| event.payload_json.get("orderSeq"))
        .and_then(|value| {
            value.as_i64().or_else(|| {
                value
                    .as_str()
                    .and_then(|text| text.trim().parse::<i64>().ok())
            })
        })
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn provider_scenarios_offline_crp_fixtures() {
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
    let _guard_scenario = EnvGuard::set("CTX_TEST_SCENARIO", "basic");
    let _guard_first_event_timeout = EnvGuard::set(
        "CTX_CRP_FIRST_EVENT_TIMEOUT_MS",
        CRP_FIXTURE_FIRST_EVENT_TIMEOUT_MS,
    );
    let _guard_ctx_mcp = common::set_ctx_mcp_command_env_for_test();

    let provider_ids: &[&str] = &[
        "codex",
        "claude-crp",
        "claude",
        // ACP bridge providers
        "gemini",
        "qwen",
        "cursor",
        "pi",
        "opencode",
        "mistral",
        "goose",
        "kimi",
        "auggie",
        "amp",
        "droid",
        "copilot",
        "continue",
        "cline",
        "openhands",
    ];

    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let data_dir = tempfile::tempdir().unwrap();
    if !host_has_sufficient_free_space(&[repo.path(), data_dir.path(), &std::env::temp_dir()]) {
        eprintln!("skipping: storage guard would trip on low-disk test host");
        return;
    }
    let (_codex_home, _guard_codex_home) = configure_hermetic_codex_home().await;
    let _guard_mcp_disabled = EnvGuard::set("CTX_MCP_DISABLED", "1");
    let fixture = common::provider_scenarios_offline_daemon_fixture(
        data_dir,
        &python,
        provider_ids,
        "http://127.0.0.1:0",
    )
    .await;
    let app = &fixture.app;

    let ws = common::create_workspace(app, repo.path(), "ws").await;

    let mut failures: HashMap<&str, String> = HashMap::new();
    for provider_id in provider_ids {
        let model_id = fixture_model_id_for_provider(app, ws.id.0, provider_id).await;
        let (_task, session) =
            common::create_task_with_session(app, ws.id.0, "t1", provider_id, &model_id).await;

        post_message(app, session.id.0, "hi").await;
        let snapshot = fixture
            .daemon
            .wait_for_provider_scenario_done_for_test(session.id)
            .await
            .unwrap();
        let events = &snapshot.events;
        let turns = &snapshot.turns;

        let mut errs: Vec<String> = Vec::new();
        if let Err(err) = check_no_session_gap(&events) {
            errs.push(err);
        }
        if let Err(err) = check_event_count_at_least(&events, SessionEventType::ToolCall, 1) {
            errs.push(err);
        }
        if let Err(err) = check_event_count_at_least(&events, SessionEventType::ToolResult, 1) {
            errs.push(err);
        }
        if let Err(err) = check_assistant_message_inserted_contains(&events, provider_id) {
            errs.push(err);
        }
        if let Err(err) = check_turn_thought_partial_contains(&turns, provider_id) {
            errs.push(err);
        }

        if !errs.is_empty() {
            failures.insert(*provider_id, errs.join("\n"));
        }
    }

    assert!(
        failures.is_empty(),
        "provider scenario assertion failures: {failures:#?}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn provider_scenarios_offline_interleaved_assistant_tools_do_not_fragment_messages() {
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
    let _guard_scenario = EnvGuard::set("CTX_TEST_SCENARIO", "interleaved_assistant_tools");
    let _guard_first_event_timeout = EnvGuard::set(
        "CTX_CRP_FIRST_EVENT_TIMEOUT_MS",
        CRP_FIXTURE_FIRST_EVENT_TIMEOUT_MS,
    );
    let _guard_ctx_mcp = common::set_ctx_mcp_command_env_for_test();

    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let data_dir = tempfile::tempdir().unwrap();
    if !host_has_sufficient_free_space(&[repo.path(), data_dir.path(), &std::env::temp_dir()]) {
        eprintln!("skipping: storage guard would trip on low-disk test host");
        return;
    }
    let (_codex_home, _guard_codex_home) = configure_hermetic_codex_home().await;
    let _guard_mcp_disabled = EnvGuard::set("CTX_MCP_DISABLED", "1");
    let provider_ids: &[&str] = &["codex"];
    let fixture = common::provider_scenarios_offline_daemon_fixture(
        data_dir,
        &python,
        provider_ids,
        "http://127.0.0.1:0",
    )
    .await;
    let app = &fixture.app;

    let ws = common::create_workspace(app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(app, ws.id.0, "t1", "codex", "fake-model").await;

    post_message(app, session.id.0, "hi").await;
    let snapshot = fixture
        .daemon
        .wait_for_provider_scenario_done_for_test(session.id)
        .await
        .unwrap();
    let events = &snapshot.events;
    let assistant_messages = &snapshot.assistant_messages;

    check_event_count_exact(&events, SessionEventType::AssistantMessageInserted, 1)
        .unwrap_or_else(|err| panic!("{err}"));

    assert_eq!(
        assistant_messages.len(),
        1,
        "expected one persisted assistant message, saw: {assistant_messages:#?}"
    );
    assert_eq!(
        assistant_messages[0].content,
        "The source patch is copied over. The Rust test runs are queued behind another Cargo lock. I am using that wait time to trace the tool subtitle path."
    );

    let assistant_inserted = events
        .iter()
        .find(|event| matches!(event.event_type, SessionEventType::AssistantMessageInserted))
        .unwrap_or_else(|| panic!("missing AssistantMessageInserted event: {events:#?}"));
    let first_tool_call = events
        .iter()
        .find(|event| matches!(event.event_type, SessionEventType::ToolCall))
        .unwrap_or_else(|| panic!("missing ToolCall event: {events:#?}"));

    let assistant_inserted_order = read_event_order_seq(assistant_inserted).unwrap_or_else(|| {
        panic!("AssistantMessageInserted missing order_seq: {assistant_inserted:#?}")
    });
    let tool_order = read_event_order_seq(first_tool_call)
        .unwrap_or_else(|| panic!("ToolCall missing order_seq: {first_tool_call:#?}"));
    let message_order = assistant_messages[0]
        .order_seq
        .unwrap_or_else(|| panic!("assistant message missing order_seq: {assistant_messages:#?}"));

    assert_eq!(
        message_order, assistant_inserted_order,
        "persisted assistant message should reuse assistant_message_inserted order_seq"
    );
    assert!(
        message_order < tool_order,
        "interleaved assistant message should stay anchored before later tool calls; message={message_order}, tool={tool_order}, events={events:#?}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn provider_scenarios_offline_crp_fixtures_persist_context_window_metrics() {
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
    let _guard_scenario = EnvGuard::set("CTX_TEST_SCENARIO", "context_window");
    let _guard_first_event_timeout = EnvGuard::set(
        "CTX_CRP_FIRST_EVENT_TIMEOUT_MS",
        CRP_FIXTURE_FIRST_EVENT_TIMEOUT_MS,
    );
    let _guard_ctx_mcp = common::set_ctx_mcp_command_env_for_test();

    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let data_dir = tempfile::tempdir().unwrap();
    if !host_has_sufficient_free_space(&[repo.path(), data_dir.path(), &std::env::temp_dir()]) {
        eprintln!("skipping: storage guard would trip on low-disk test host");
        return;
    }
    let (_codex_home, _guard_codex_home) = configure_hermetic_codex_home().await;
    let _guard_mcp_disabled = EnvGuard::set("CTX_MCP_DISABLED", "1");
    let provider_ids: &[&str] = &["codex", "claude-crp"];
    let fixture = common::provider_scenarios_offline_daemon_fixture(
        data_dir,
        &python,
        provider_ids,
        "http://127.0.0.1:0",
    )
    .await;
    let app = &fixture.app;

    let ws = common::create_workspace(app, repo.path(), "ws").await;

    for provider_id in provider_ids {
        let model_id = fixture_model_id_for_provider(app, ws.id.0, provider_id).await;
        let (_task, session) =
            common::create_task_with_session(app, ws.id.0, "t1", provider_id, &model_id).await;

        post_message(app, session.id.0, "hi").await;
        let snapshot = fixture
            .daemon
            .wait_for_provider_scenario_completed_turn_for_test(session.id)
            .await
            .unwrap();
        let turn = snapshot.turns.last().unwrap_or_else(|| {
            panic!("expected one completed turn for {provider_id}: {snapshot:#?}")
        });
        let metrics = turn.metrics_json.as_ref().unwrap_or_else(|| {
            panic!("expected metrics_json on final turn for {provider_id}: {turn:#?}")
        });

        assert_eq!(
            metrics
                .get("context_window_tokens")
                .and_then(serde_json::Value::as_u64),
            Some(200_000),
            "unexpected context_window_tokens for {provider_id}: {metrics:#?}"
        );
        assert_eq!(
            metrics
                .get("context_tokens_estimate")
                .and_then(serde_json::Value::as_u64),
            Some(50_000),
            "unexpected context_tokens_estimate for {provider_id}: {metrics:#?}"
        );
        assert_eq!(
            metrics
                .get("remaining_tokens_estimate")
                .and_then(serde_json::Value::as_u64),
            Some(150_000),
            "unexpected remaining_tokens_estimate for {provider_id}: {metrics:#?}"
        );
        let remaining_fraction = metrics
            .get("remaining_fraction")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or_else(|| {
                panic!("expected remaining_fraction for {provider_id}: {metrics:#?}")
            });
        assert!(
            (remaining_fraction - 0.75).abs() < 1e-9,
            "unexpected remaining_fraction for {provider_id}: {remaining_fraction}; metrics={metrics:#?}"
        );
    }
}
