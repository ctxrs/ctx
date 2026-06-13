use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use serde_json::Value;

use ctx_daemon::test_support::TestDaemon;
use ctx_managed_installs::{save_agent_server_config, AgentServerCommand, AgentServerConfigFile};
use ctx_providers::adapters::{ProviderAdapter, ProviderHealth, ProviderStatus};
use ctx_providers::crp::Tier1CrpAdapter;

mod common;

struct LiveCanaryHeadSnapshot {
    body: Value,
    events: Vec<Value>,
    assistant_messages: Vec<String>,
}

async fn post_message(app: &axum::Router, session_id: uuid::Uuid, content: &str) {
    let (status, body): (StatusCode, Value) = common::json_request(
        app,
        Method::POST,
        format!("/api/sessions/{session_id}/messages"),
        Some(serde_json::json!({ "content": content })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "failed to post live canary message: {body:#?}"
    );
}

async fn wait_for_terminal_head(
    app: &axum::Router,
    session_id: uuid::Uuid,
) -> LiveCanaryHeadSnapshot {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(240);
    loop {
        let (status, body): (StatusCode, Value) = common::json_request(
            app,
            Method::GET,
            format!("/api/sessions/{session_id}/head?include_events=true&limit=120"),
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "failed to load live canary session head: {body:#?}"
        );
        let events = head_events(&body);
        if events.iter().any(|event| event_type(event) == Some("done")) {
            return LiveCanaryHeadSnapshot {
                assistant_messages: assistant_messages_from_head(&body),
                body,
                events,
            };
        }
        if events.iter().any(is_terminal_failure_event) {
            panic!(
                "live canary saw terminal failure/auth-required events: {events:#?}; head={body:#?}"
            );
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for Done event: {events:#?}; head={body:#?}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn head_events(body: &Value) -> Vec<Value> {
    body.get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn event_type(event: &Value) -> Option<&str> {
    event.get("event_type").and_then(Value::as_str)
}

fn event_payload(event: &Value) -> Option<&Value> {
    event.get("payload_json").or_else(|| event.get("payload"))
}

fn is_terminal_failure_event(event: &Value) -> bool {
    match event_type(event) {
        Some("auth_required" | "error") => true,
        Some("turn_finished") => matches!(
            event_payload(event)
                .and_then(|payload| payload.get("status"))
                .and_then(Value::as_str),
            Some("failed" | "interrupted")
        ),
        _ => false,
    }
}

fn assistant_messages_from_head(body: &Value) -> Vec<String> {
    body.get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .filter_map(|message| {
            message
                .get("content")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect()
}

fn assistant_message_text_from_head(body: &Value) -> String {
    body.get("messages")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .rev()
                .find(|row| row.get("role").and_then(Value::as_str) == Some("assistant"))
        })
        .and_then(|row| row.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn resolve_live_claude_crp_command() -> Option<String> {
    if let Ok(raw) = std::env::var("CTX_LIVE_CLAUDE_CRP_COMMAND") {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            eprintln!(
                "skipping: CTX_LIVE_CLAUDE_CRP_COMMAND is set but empty; provide an absolute path"
            );
            return None;
        }
        let path = std::path::PathBuf::from(trimmed);
        if !path.is_absolute() || !path.exists() {
            eprintln!(
                "skipping: CTX_LIVE_CLAUDE_CRP_COMMAND must be an existing absolute path: {trimmed}"
            );
            return None;
        }
        return Some(path.to_string_lossy().to_string());
    }

    let output = Command::new("which").arg("claude-crp").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let detected = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if detected.is_empty() {
        return None;
    }
    let path = std::path::PathBuf::from(&detected);
    if !path.is_absolute() || !path.exists() {
        return None;
    }
    Some(path.to_string_lossy().to_string())
}

fn has_claude_cli() -> bool {
    Command::new("which")
        .arg("claude")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

async fn seed_claude_runtime_config(data_root: &Path, command_abs_path: &str) {
    let mut cfg = AgentServerConfigFile::default();
    let command = AgentServerCommand {
        command: command_abs_path.to_string(),
        args: Vec::new(),
        dependencies: Vec::new(),
        managed: None,
    };
    cfg.providers
        .insert("claude-crp".to_string(), command.clone());
    cfg.providers.insert("claude".to_string(), command);
    save_agent_server_config(data_root, &cfg)
        .await
        .expect("write agent server config");
}

async fn seed_provider_status_ok(daemon: &TestDaemon, provider_id: &str) {
    daemon
        .upsert_provider_status(
            provider_id.to_string(),
            ProviderStatus {
                provider_id: provider_id.to_string(),
                installed: true,
                detected_path: None,
                version: None,
                capabilities: None,
                health: ProviderHealth::Ok,
                diagnostics: Vec::new(),
                details: HashMap::new(),
                usability: ctx_providers::adapters::ProviderUsability::default(),
            },
        )
        .await;
}

#[tokio::test]
#[ignore]
async fn live_provider_canary_turn_invariants() {
    let provider_id = std::env::var("CTX_LIVE_PROVIDER_ID").ok();
    let model_id = std::env::var("CTX_LIVE_MODEL_ID").ok();
    if provider_id.is_none() || model_id.is_none() {
        eprintln!("skipping: set CTX_LIVE_PROVIDER_ID and CTX_LIVE_MODEL_ID to run live canaries");
        return;
    }
    let provider_id = provider_id.unwrap();
    let model_id = model_id.unwrap();

    let adapter: Arc<dyn ProviderAdapter> = match provider_id.as_str() {
        "codex" => Arc::new(Tier1CrpAdapter::codex()),
        "claude" | "claude-crp" => Arc::new(Tier1CrpAdapter::claude()),
        _ => {
            eprintln!(
                "skipping: CTX_LIVE_PROVIDER_ID={provider_id} not supported by this canary yet"
            );
            return;
        }
    };

    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let data_dir = tempfile::tempdir().unwrap();

    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert(provider_id.clone(), adapter);

    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        providers,
        "http://127.0.0.1:0",
    )
    .await;
    let app = fixture.router();
    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "t1", &provider_id, &model_id).await;

    let expected_token = format!("CTX_LIVE_PROVIDER_CANARY_OK_{}", uuid::Uuid::new_v4());
    post_message(
        &app,
        session.id.0,
        &format!("Reply with exactly this token: {expected_token}"),
    )
    .await;
    let snapshot = wait_for_terminal_head(&app, session.id.0).await;

    let assistant_messages = snapshot.assistant_messages;
    assert!(
        assistant_messages
            .iter()
            .any(|message| message.contains(&expected_token)),
        "expected assistant message containing {expected_token}; saw {assistant_messages:#?} in head {:#?} and events {:#?}",
        snapshot.body,
        snapshot.events
    );
}

#[tokio::test]
#[ignore]
async fn live_codex_canary_can_edit_workspace_file() {
    let provider_id = std::env::var("CTX_LIVE_PROVIDER_ID")
        .ok()
        .filter(|value| matches!(value.as_str(), "codex"))
        .unwrap_or_else(|| "codex".to_string());
    let model_id = std::env::var("CTX_LIVE_MODEL_ID").ok();
    if model_id.is_none() {
        eprintln!("skipping: set CTX_LIVE_MODEL_ID to run the live Codex file-edit canary");
        return;
    }
    let model_id = model_id.unwrap();

    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let data_dir = tempfile::tempdir().unwrap();

    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert(provider_id.clone(), Arc::new(Tier1CrpAdapter::codex()));

    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        providers,
        "http://127.0.0.1:0",
    )
    .await;
    let app = fixture.router();
    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "codex-write", &provider_id, &model_id)
            .await;

    let expected_token = format!("CTX_LIVE_CODEX_WRITE_OK_{}", uuid::Uuid::new_v4());
    let relative_path = "live-codex-write-proof.txt";
    let prompt = format!(
        "Create or overwrite the workspace file {relative_path}. Write exactly this content and nothing else: {expected_token}. The file must contain exactly those characters with no trailing newline or extra whitespace. If you use a shell command to write the file, use printf rather than echo -n, because echo -n is not portable and may write the literal text -n. After writing the file, reply with exactly this token: {expected_token}"
    );
    post_message(&app, session.id.0, &prompt).await;
    let snapshot = wait_for_terminal_head(&app, session.id.0).await;

    let actual = tokio::fs::read_to_string(repo.path().join(relative_path))
        .await
        .expect("live Codex canary should create proof file");
    assert_eq!(
        actual.trim_end(),
        expected_token,
        "live Codex canary wrote unexpected file contents"
    );

    let assistant_messages = snapshot.assistant_messages;
    assert!(
        assistant_messages
            .iter()
            .any(|message| message.contains(&expected_token)),
        "expected assistant message containing {expected_token}; saw {assistant_messages:#?} in head {:#?} and events {:#?}",
        snapshot.body,
        snapshot.events
    );
}

#[tokio::test]
#[ignore]
async fn live_claude_endpoint_profile_api_key_round_trip() {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    if api_key.is_none() {
        eprintln!("skipping: set ANTHROPIC_API_KEY to run live Claude endpoint canary");
        return;
    }
    let api_key = api_key.unwrap();

    let claude_crp_command = resolve_live_claude_crp_command();
    if claude_crp_command.is_none() {
        eprintln!(
            "skipping: set CTX_LIVE_CLAUDE_CRP_COMMAND to an absolute claude-crp path (or install claude-crp in PATH)"
        );
        return;
    }
    if !has_claude_cli() {
        eprintln!("skipping: Claude CLI is not installed (missing `claude` command)");
        return;
    }
    let claude_crp_command = claude_crp_command.unwrap();

    let base_url = std::env::var("CTX_LIVE_ANTHROPIC_BASE_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "https://api.anthropic.com".to_string());

    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let data_dir = tempfile::tempdir().unwrap();
    seed_claude_runtime_config(data_dir.path(), &claude_crp_command).await;

    let claude_adapter: Arc<dyn ProviderAdapter> = Arc::new(Tier1CrpAdapter::from_raw(
        "claude-crp",
        claude_crp_command.clone(),
        vec![],
    ));
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("claude-crp".to_string(), Arc::clone(&claude_adapter));
    providers.insert("claude".to_string(), claude_adapter);

    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        providers,
        "http://127.0.0.1:0",
    )
    .await;
    let app = fixture.router();
    let provider_id = "claude-crp".to_string();
    seed_provider_status_ok(&fixture.daemon, &provider_id).await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let endpoint_name = format!("live-claude-endpoint-{}", uuid::Uuid::new_v4());

    let (upsert_status, upsert_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/providers/{provider_id}/harness_config/endpoints"),
        Some(serde_json::json!({
            "name": endpoint_name,
            "base_url": base_url,
            "api_shape": "anthropic_messages",
            "api_key": api_key,
        })),
    )
    .await;
    assert_eq!(
        upsert_status,
        StatusCode::OK,
        "failed to create claude endpoint profile: {upsert_body:#?}"
    );
    let endpoint_id = upsert_body
        .get("endpoints")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter().find_map(|row| {
                let is_match = row
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name == endpoint_name);
                if !is_match {
                    return None;
                }
                row.get("id").and_then(Value::as_str).map(|s| s.to_string())
            })
        })
        .expect("endpoint id for created claude endpoint");

    let (select_status, select_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/providers/{provider_id}/harness_config/select"),
        Some(serde_json::json!({
            "source_kind": "endpoint",
            "endpoint_id": endpoint_id,
        })),
    )
    .await;
    assert_eq!(
        select_status,
        StatusCode::OK,
        "failed to select claude endpoint source: {select_body:#?}"
    );

    let (verify_status, verify_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/providers/{provider_id}/verify", ws.id.0),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(
        verify_status,
        StatusCode::OK,
        "provider verify API call failed: {verify_body:#?}"
    );
    assert_eq!(
        verify_body.get("status").and_then(Value::as_str),
        Some("ok"),
        "provider verify did not return ok: {verify_body:#?}"
    );

    let (options_status, options_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!(
            "/api/workspaces/{}/providers/{provider_id}/options",
            ws.id.0
        ),
        None,
    )
    .await;
    assert_eq!(
        options_status,
        StatusCode::OK,
        "failed to load claude provider options: {options_body:#?}"
    );

    let model_id = std::env::var("CTX_LIVE_MODEL_ID")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| {
            options_body
                .get("models")
                .and_then(|m| m.get("current_model_id"))
                .and_then(Value::as_str)
                .map(|s| s.to_string())
        })
        .or_else(|| {
            options_body
                .get("models")
                .and_then(|m| m.get("models"))
                .and_then(Value::as_array)
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("id"))
                .and_then(Value::as_str)
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "sonnet".to_string());

    let (_task, session) = common::create_task_with_session(
        &app,
        ws.id.0,
        "live-claude-endpoint-task",
        &provider_id,
        &model_id,
    )
    .await;

    post_message(
        &app,
        session.id.0,
        "Reply with exactly this token: CLAUDE_ENDPOINT_E2E_OK",
    )
    .await;
    let snapshot = wait_for_terminal_head(&app, session.id.0).await;
    let assistant_messages = snapshot.assistant_messages;
    assert!(
        assistant_messages
            .iter()
            .any(|message| message.contains("CLAUDE_ENDPOINT_E2E_OK")),
        "expected assistant message containing CLAUDE_ENDPOINT_E2E_OK; saw {assistant_messages:#?} in head {:#?} and events {:#?}",
        snapshot.body,
        snapshot.events
    );
}

#[tokio::test]
#[ignore]
async fn live_claude_openrouter_opus_v1_base_url_normalization_round_trip() {
    let api_key = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    if api_key.is_none() {
        eprintln!("skipping: set OPENROUTER_API_KEY to run live OpenRouter Claude endpoint canary");
        return;
    }
    let api_key = api_key.unwrap();

    let claude_crp_command = resolve_live_claude_crp_command();
    if claude_crp_command.is_none() {
        eprintln!(
            "skipping: set CTX_LIVE_CLAUDE_CRP_COMMAND to an absolute claude-crp path (or install claude-crp in PATH)"
        );
        return;
    }
    if !has_claude_cli() {
        eprintln!("skipping: Claude CLI is not installed (missing `claude` command)");
        return;
    }
    let claude_crp_command = claude_crp_command.unwrap();

    let openrouter_base = std::env::var("OPENROUTER_BASE_URL")
        .ok()
        .or_else(|| std::env::var("CTX_LIVE_OPENROUTER_BASE_URL").ok())
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "https://openrouter.ai/api/v1".to_string());
    let input_base_with_v1 = if openrouter_base.to_ascii_lowercase().ends_with("/v1") {
        openrouter_base
    } else {
        format!("{openrouter_base}/v1")
    };
    let expected_normalized_base = input_base_with_v1
        .strip_suffix("/v1")
        .expect("input base URL has /v1 suffix")
        .to_string();

    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let data_dir = tempfile::tempdir().unwrap();
    seed_claude_runtime_config(data_dir.path(), &claude_crp_command).await;

    let claude_adapter: Arc<dyn ProviderAdapter> = Arc::new(Tier1CrpAdapter::from_raw(
        "claude-crp",
        claude_crp_command.clone(),
        vec![],
    ));
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("claude-crp".to_string(), Arc::clone(&claude_adapter));
    providers.insert("claude".to_string(), claude_adapter);

    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        providers,
        "http://127.0.0.1:0",
    )
    .await;
    let app = fixture.router();
    let provider_id = "claude-crp".to_string();
    seed_provider_status_ok(&fixture.daemon, &provider_id).await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let endpoint_name = format!("live-claude-openrouter-{}", uuid::Uuid::new_v4());
    let requested_model = "anthropic/claude-opus-4.6";

    let (upsert_status, upsert_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/providers/{provider_id}/harness_config/endpoints"),
        Some(serde_json::json!({
            "name": endpoint_name,
            "base_url": input_base_with_v1,
            "api_shape": "anthropic_messages",
            "api_key": api_key,
            "model_override": requested_model,
        })),
    )
    .await;
    assert_eq!(
        upsert_status,
        StatusCode::OK,
        "failed to create OpenRouter endpoint profile: {upsert_body:#?}"
    );
    let endpoint = upsert_body
        .get("endpoints")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter().find(|row| {
                row.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name == endpoint_name)
            })
        })
        .expect("created endpoint in response");
    let endpoint_id = endpoint
        .get("id")
        .and_then(Value::as_str)
        .expect("created endpoint id");
    let endpoint_base = endpoint
        .get("base_url")
        .and_then(Value::as_str)
        .expect("created endpoint base_url");
    assert_eq!(
        endpoint_base, expected_normalized_base,
        "claude endpoint base URL should strip trailing /v1 for anthropic_messages"
    );

    let (select_status, select_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/providers/{provider_id}/harness_config/select"),
        Some(serde_json::json!({
            "source_kind": "endpoint",
            "endpoint_id": endpoint_id,
        })),
    )
    .await;
    assert_eq!(
        select_status,
        StatusCode::OK,
        "failed to select OpenRouter endpoint source: {select_body:#?}"
    );

    let (verify_status, verify_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/providers/{provider_id}/verify", ws.id.0),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(
        verify_status,
        StatusCode::OK,
        "provider verify API call failed: {verify_body:#?}"
    );
    assert_eq!(
        verify_body.get("status").and_then(Value::as_str),
        Some("ok"),
        "provider verify did not return ok: {verify_body:#?}"
    );

    let (_task, session) = common::create_task_with_session(
        &app,
        ws.id.0,
        "live-claude-openrouter-opus",
        &provider_id,
        requested_model,
    )
    .await;

    post_message(
        &app,
        session.id.0,
        "Reply with exactly this token: OPENROUTER_CLAUDE_OPUS_46_OK",
    )
    .await;
    let snapshot = wait_for_terminal_head(&app, session.id.0).await;

    let assistant_text = assistant_message_text_from_head(&snapshot.body);
    assert!(
        assistant_text.contains("OPENROUTER_CLAUDE_OPUS_46_OK"),
        "assistant response did not include expected marker: {assistant_text}; events={:#?}",
        snapshot.events
    );
}
