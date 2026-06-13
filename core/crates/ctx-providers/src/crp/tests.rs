use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use base64::Engine as _;
use ctx_core::provider_policy::{
    CTX_CRP_LAUNCH_POLICY_ENV, CTX_CRP_LAUNCH_POLICY_FULL, FULL_YOLO_SANDBOX_MODE,
};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Duration;

use super::normalize::{map_crp_event, unknown_event_observation, CachedToolInput};
use super::protocol::CrpEvent;
use super::runtime::apply_outer_process_env;
use super::*;
use crate::adapters::ProviderTurnStatus;

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct ScopedEnvVar {
    key: &'static str,
    previous: Option<String>,
}

impl ScopedEnvVar {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        if let Some(value) = &self.previous {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn crp_test_env() -> HashMap<String, String> {
    HashMap::from([("CTX_MCP_DISABLED".to_string(), "1".to_string())])
}

fn immediate_sweep_config() -> ProviderSessionSweepConfig {
    ProviderSessionSweepConfig {
        idle_ttl: Duration::ZERO,
        max_idle_sessions: 0,
        interval: Duration::from_secs(60),
    }
}

fn write_session_status_runtime(
    workdir: &std::path::Path,
    script_name: &str,
    quiescent: bool,
) -> Result<std::path::PathBuf> {
    let script_path = workdir.join(script_name);
    fs::write(
        &script_path,
        format!(
            "#!/bin/sh\nwhile IFS= read -r line; do\n  if printf '%s' \"$line\" | grep -q '\"type\":\"session.status\"'; then\n    session_id=$(printf '%s' \"$line\" | sed -n 's/.*\"session_id\":\"\\([^\"]*\\)\".*/\\1/p')\n    printf '{{\"v\":1,\"seq\":1,\"channel\":\"control\",\"type\":\"session.notice\",\"session_id\":\"%s\",\"code\":\"session_status\",\"severity\":\"info\",\"message\":\"status\",\"details\":{{\"quiescent\":{quiescent}}}}}\\n' \"$session_id\"\n  fi\ndone\n",
            quiescent = if quiescent { "true" } else { "false" },
        ),
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;
    Ok(script_path)
}

#[test]
fn apply_outer_process_env_removes_inherited_ctx_auth_token() {
    let _guard = ScopedEnvVar::set("CTX_AUTH_TOKEN", "host-token");
    let mut cmd = tokio::process::Command::new("/usr/bin/env");
    apply_outer_process_env(&mut cmd, &HashMap::new());
    let envs: HashMap<_, _> = cmd.as_std().get_envs().collect();
    assert_eq!(
        envs.get(std::ffi::OsStr::new("CTX_AUTH_TOKEN"))
            .and_then(|value| value.as_deref()),
        None
    );
}

#[test]
fn apply_outer_process_env_removes_explicit_ctx_auth_token_override() {
    let _guard = ScopedEnvVar::set("CTX_AUTH_TOKEN", "host-token");
    let mut cmd = tokio::process::Command::new("/usr/bin/env");
    let env = HashMap::from([("CTX_AUTH_TOKEN".to_string(), "explicit-token".to_string())]);
    apply_outer_process_env(&mut cmd, &env);
    let envs: HashMap<_, _> = cmd.as_std().get_envs().collect();
    assert_eq!(
        envs.get(std::ffi::OsStr::new("CTX_AUTH_TOKEN"))
            .and_then(|value| value.as_deref()),
        None
    );
}

#[test]
fn apply_outer_process_env_removes_explicit_ctx_mcp_token_override() {
    let _guard = ScopedEnvVar::set("CTX_MCP_TOKEN", "host-mcp-token");
    let mut cmd = tokio::process::Command::new("/usr/bin/env");
    let env = HashMap::from([(
        "CTX_MCP_TOKEN".to_string(),
        "explicit-mcp-token".to_string(),
    )]);
    apply_outer_process_env(&mut cmd, &env);
    let envs: HashMap<_, _> = cmd.as_std().get_envs().collect();
    assert_eq!(
        envs.get(std::ffi::OsStr::new("CTX_MCP_TOKEN"))
            .and_then(|value| value.as_deref()),
        None
    );
}

#[test]
fn apply_outer_process_env_removes_local_shutdown_token() {
    let _guard = ScopedEnvVar::set("CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN", "host-shutdown-token");
    let mut cmd = tokio::process::Command::new("/usr/bin/env");
    let env = HashMap::from([(
        "CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN".to_string(),
        "explicit-shutdown-token".to_string(),
    )]);
    apply_outer_process_env(&mut cmd, &env);
    let envs: HashMap<_, _> = cmd.as_std().get_envs().collect();
    assert_eq!(
        envs.get(std::ffi::OsStr::new("CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN"))
            .and_then(|value| value.as_deref()),
        None
    );
}

#[tokio::test]
async fn set_session_model_writes_crp_command_for_live_session() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("capture.sh");
    let log_path = workdir.join("stdin.log");

    fs::write(
        &script_path,
        "#!/bin/sh\nwhile IFS= read -r line; do\n  printf '%s\\n' \"$line\" >> \"$LOG_FILE\"\n  if printf '%s' \"$line\" | grep -q '\"type\":\"session.set_model\"'; then\n    printf '{\"v\":1,\"seq\":1,\"channel\":\"control\",\"type\":\"session.notice\",\"session_id\":\"session-set-model\",\"code\":\"session_model_updated\",\"severity\":\"info\",\"message\":\"session model updated to amp-medium\",\"details\":{\"model_id\":\"amp-medium\"}}\\n'\n  fi\ndone\n",
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "session-set-model";
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );

    let session = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;
    session.opened.store(true, Ordering::SeqCst);

    adapter
        .set_session_model(session_key.to_string(), "amp-medium".to_string())
        .await?;

    let started = Instant::now();
    let contents = loop {
        if let Ok(contents) = fs::read_to_string(&log_path) {
            if !contents.trim().is_empty() {
                break contents;
            }
        }
        if started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for CRP command capture");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    let first_line = contents
        .lines()
        .find(|line| !line.trim().is_empty())
        .expect("captured command line");
    let payload: serde_json::Value = serde_json::from_str(first_line)?;
    assert_eq!(payload.get("v"), Some(&json!(1)));
    assert_eq!(payload.get("type"), Some(&json!("session.set_model")));
    assert_eq!(payload.get("session_id"), Some(&json!(session_key)));
    assert_eq!(payload.get("model_id"), Some(&json!("amp-medium")));

    session.process.shutdown("test complete").await;
    Ok(())
}

#[test]
fn unknown_control_crp_event_maps_to_diagnostic_only_observation() {
    let mut tool_output_cache: HashMap<String, String> = HashMap::new();
    let mut tool_input_cache: HashMap<String, CachedToolInput> = HashMap::new();

    let event = CrpEvent::Unknown {
        event_type: "tool.progress".to_string(),
        session_id: Some("session-1".to_string()),
        turn_id: Some("turn-1".to_string()),
        parse_error: "unknown variant `tool.progress`".to_string(),
        raw: json!({
            "type": "tool.progress",
            "session_id": "session-1",
            "turn_id": "turn-1",
            "message": "Scanning files",
            "percent": 50
        }),
    };

    let observation = unknown_event_observation(&event, protocol::CrpChannel::Control, 11)
        .expect("unknown control event should produce diagnostic observation");
    let mapped = map_crp_event(
        event,
        protocol::CrpChannel::Control,
        11,
        &mut tool_output_cache,
        &mut tool_input_cache,
    );

    assert!(mapped.events.is_empty());
    assert!(!mapped.done);
    assert_eq!(observation.protocol, "crp");
    assert_eq!(observation.event_type, "tool.progress");
    assert!(observation.crp_channel.is_none());
    assert_eq!(observation.crp_seq, 11);
    assert!(!observation.timeline_notice_emitted);
    assert_eq!(observation.raw.pointer("/percent"), Some(&json!(50)));
}

#[test]
fn unknown_control_tool_event_keeps_raw_details_in_diagnostic_observation() {
    let mut tool_output_cache: HashMap<String, String> = HashMap::new();
    let mut tool_input_cache: HashMap<String, CachedToolInput> = HashMap::new();

    let event = CrpEvent::Unknown {
        event_type: "tool.progress".to_string(),
        session_id: Some("session-1".to_string()),
        turn_id: Some("turn-1".to_string()),
        parse_error: "unknown variant `tool.progress`".to_string(),
        raw: json!({
            "type": "tool.progress",
            "session_id": "session-1",
            "turn_id": "turn-1",
            "tool_name": "Bash",
            "command": ["find", ".ctx/ctx-pack/agent-basics", "-type", "f"]
        }),
    };

    let observation = unknown_event_observation(&event, protocol::CrpChannel::Control, 12)
        .expect("unknown control event should produce diagnostic observation");
    let mapped = map_crp_event(
        event,
        protocol::CrpChannel::Control,
        12,
        &mut tool_output_cache,
        &mut tool_input_cache,
    );

    assert!(mapped.events.is_empty());
    assert_eq!(observation.raw.pointer("/command/0"), Some(&json!("find")));
}

#[test]
fn unknown_data_crp_event_maps_to_diagnostic_only_observation() {
    let mut tool_output_cache: HashMap<String, String> = HashMap::new();
    let mut tool_input_cache: HashMap<String, CachedToolInput> = HashMap::new();
    let event = CrpEvent::Unknown {
        event_type: "tool.progress".to_string(),
        session_id: Some("session-1".to_string()),
        turn_id: Some("turn-1".to_string()),
        parse_error: "unknown variant `tool.progress`".to_string(),
        raw: json!({
            "type": "tool.progress",
            "session_id": "session-1",
            "turn_id": "turn-1",
            "message": "Scanning files",
            "percent": 50
        }),
    };

    let observation = unknown_event_observation(&event, protocol::CrpChannel::Data, 13)
        .expect("unknown event should produce diagnostic observation");
    let mapped = map_crp_event(
        event,
        protocol::CrpChannel::Data,
        13,
        &mut tool_output_cache,
        &mut tool_input_cache,
    );

    assert!(mapped.events.is_empty());
    assert!(!mapped.done);
    assert_eq!(observation.protocol, "crp");
    assert_eq!(observation.event_type, "tool.progress");
    assert_eq!(observation.crp_channel.as_deref(), Some("data"));
    assert_eq!(observation.crp_seq, 13);
    assert!(!observation.timeline_notice_emitted);
    assert_eq!(observation.raw.pointer("/percent"), Some(&json!(50)));
}

#[tokio::test]
async fn set_session_model_rejects_missing_live_session() {
    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec!["-c".into(), "cat >/dev/null".into()],
    );
    let err = adapter
        .set_session_model("missing".to_string(), "amp-medium".to_string())
        .await
        .expect_err("missing session should fail");
    assert!(err
        .to_string()
        .contains("provider session missing is not live"));
}

#[tokio::test]
async fn prompt_drains_terminal_interrupted_event_after_cancel() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("fake-crp.sh");
    let log_path = workdir.join("stdin.log");

    fs::write(
        &script_path,
        r#"#!/bin/sh
turn_id=""
session_id=""
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  case "$line" in
    *'"type":"session.prompt"'*)
      turn_id=$(printf '%s' "$line" | sed -n 's/.*"turn_id":"\([^"]*\)".*/\1/p')
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      ;;
    *'"type":"session.cancel"'*)
      sleep 0.1
      printf '{"v":1,"seq":1,"channel":"control","type":"turn.completed","session_id":"%s","turn_id":"%s","status":"interrupted"}\n' "$session_id" "$turn_id"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "cancel-drain";
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );

    let session = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;
    session.opened.store(true, Ordering::SeqCst);

    let (event_tx, mut event_rx) = mpsc::channel(16);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let request = CrpPromptRequest {
        session_key: session_key.to_string(),
        input: TurnInput {
            content: "investigate".to_string(),
            attachments: Vec::new(),
            context_blocks: Vec::new(),
            model_id: None,
        },
        workdir: workdir.clone(),
        env: env.clone(),
        event_sink: event_tx,
        provider_unknown_event: None,
        provider_session_ref_claim: None,
        cancel_rx,
    };

    let pool = Arc::clone(&adapter.pool);
    let prompt_task = tokio::spawn(async move { pool.prompt(request).await });

    let started = Instant::now();
    loop {
        if let Ok(contents) = fs::read_to_string(&log_path) {
            if contents.contains(r#""type":"session.prompt""#) {
                break;
            }
        }
        if started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for session.prompt");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    cancel_tx.send(()).expect("cancel signal should send");

    let mut saw_turn_interrupted = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), event_rx.recv()).await {
            Ok(Some(event)) => {
                if matches!(event.event_type, SessionEventType::TurnInterrupted) {
                    saw_turn_interrupted = true;
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => {}
        }
    }

    prompt_task.await??;
    assert!(
        saw_turn_interrupted,
        "expected interrupted terminal event after cancel"
    );

    session.process.shutdown("test complete").await;
    Ok(())
}

#[tokio::test]
async fn prompt_fails_fast_on_fatal_startup_stderr_and_shuts_down_runtime() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("fatal_startup.sh");

    fs::write(
        &script_path,
        "#!/bin/sh\nprintf 'time=\"2026-04-02T22:10:57Z\" level=fatal msg=\"failed to create temp dir\"\\n' >&2\nsleep 30\n",
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "opencode",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "session-fatal-startup";
    let mut env = crp_test_env();
    env.insert("CTX_SESSION_ID".to_string(), session_key.to_string());

    let (event_sink, _event_rx) = tokio::sync::mpsc::channel(8);
    let handle = adapter
        .run(
            TurnInput {
                content: "user".to_string(),
                attachments: vec![],
                context_blocks: vec![],
                model_id: None,
            },
            workdir,
            env,
            event_sink,
            crate::adapters::ProviderRunHooks::default(),
        )
        .await?;

    let crate::adapters::RunHandle {
        done,
        outcome,
        cancel: _cancel,
        ..
    } = handle;
    tokio::time::timeout(Duration::from_secs(5), done)
        .await
        .expect("fatal startup run should finish promptly")?;

    let outcome = tokio::time::timeout(Duration::from_secs(5), outcome)
        .await
        .expect("fatal startup outcome should finish promptly")?;
    assert_eq!(outcome.status, ProviderTurnStatus::Failed);
    assert!(
        outcome
            .message
            .as_deref()
            .is_some_and(|message| message.contains("level=fatal")),
        "expected fatal stderr to surface through the failed outcome"
    );
    assert!(
        !adapter.has_live_session(session_key).await,
        "fatal startup stderr should shut down the unusable runtime session"
    );

    Ok(())
}

#[tokio::test]
async fn opencode_flattens_prompt_items_into_single_prompt_field() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("capture_prompt.sh");
    let log_path = workdir.join("stdin.log");

    fs::write(
        &script_path,
        "#!/bin/sh\nwhile IFS= read -r line; do\n  printf '%s\\n' \"$line\" >> \"$LOG_FILE\"\n  if printf '%s' \"$line\" | grep -q '\"type\":\"session.open\"'; then\n    printf '{\"v\":1,\"seq\":1,\"channel\":\"control\",\"type\":\"session.opened\",\"session_id\":\"session-prompt\"}\\n'\n  fi\n  if printf '%s' \"$line\" | grep -q '\"type\":\"session.prompt\"'; then\n    turn_id=$(printf '%s' \"$line\" | sed -n 's/.*\"turn_id\":\"\\([^\"]*\\)\".*/\\1/p')\n    printf '{\"v\":1,\"seq\":2,\"channel\":\"control\",\"type\":\"turn.completed\",\"session_id\":\"session-prompt\",\"turn_id\":\"%s\",\"status\":\"success\"}\\n' \"$turn_id\"\n  fi\ndone\n",
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "opencode",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );
    env.insert("CTX_SESSION_ID".to_string(), "session-prompt".to_string());

    let (event_sink, mut event_rx) = tokio::sync::mpsc::channel(8);
    let handle = adapter
        .run(
            TurnInput {
                content: "user".to_string(),
                attachments: vec![],
                context_blocks: vec![json!({"type":"text","text":"system"})],
                model_id: Some("openai/gpt-4.1-mini".to_string()),
            },
            workdir.clone(),
            env,
            event_sink,
            crate::adapters::ProviderRunHooks::default(),
        )
        .await?;

    tokio::time::timeout(Duration::from_secs(5), handle.done)
        .await
        .expect("prompt run should finish")?;
    while event_rx.recv().await.is_some() {}

    let contents = fs::read_to_string(&log_path)?;
    let prompt_line = contents
        .lines()
        .find(|line| line.contains("\"type\":\"session.prompt\""))
        .expect("captured prompt line");
    let payload: serde_json::Value = serde_json::from_str(prompt_line)?;
    assert_eq!(payload.get("items"), None);
    assert_eq!(payload.get("prompt"), Some(&json!("system\n\nuser")));

    Ok(())
}

#[tokio::test]
async fn prompt_model_override_can_be_disabled_via_env() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("capture_prompt_model.sh");
    let log_path = workdir.join("stdin.log");

    fs::write(
        &script_path,
        "#!/bin/sh\nwhile IFS= read -r line; do\n  printf '%s\\n' \"$line\" >> \"$LOG_FILE\"\n  if printf '%s' \"$line\" | grep -q '\"type\":\"session.open\"'; then\n    printf '{\"v\":1,\"seq\":1,\"channel\":\"control\",\"type\":\"session.opened\",\"session_id\":\"session-no-model\"}\\n'\n  fi\n  if printf '%s' \"$line\" | grep -q '\"type\":\"session.prompt\"'; then\n    turn_id=$(printf '%s' \"$line\" | sed -n 's/.*\"turn_id\":\"\\([^\"]*\\)\".*/\\1/p')\n    printf '{\"v\":1,\"seq\":2,\"channel\":\"control\",\"type\":\"turn.completed\",\"session_id\":\"session-no-model\",\"turn_id\":\"%s\",\"status\":\"success\"}\\n' \"$turn_id\"\n  fi\ndone\n",
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "kimi",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );
    env.insert("CTX_SESSION_ID".to_string(), "session-no-model".to_string());
    env.insert(
        "CTX_CRP_DISABLE_MODEL_OVERRIDE".to_string(),
        "1".to_string(),
    );

    let (event_sink, mut event_rx) = tokio::sync::mpsc::channel(8);
    let handle = adapter
        .run(
            TurnInput {
                content: "user".to_string(),
                attachments: vec![],
                context_blocks: vec![],
                model_id: Some("openai/gpt-4.1-mini".to_string()),
            },
            workdir.clone(),
            env,
            event_sink,
            crate::adapters::ProviderRunHooks::default(),
        )
        .await?;

    tokio::time::timeout(Duration::from_secs(5), handle.done)
        .await
        .expect("prompt run should finish")?;
    while event_rx.recv().await.is_some() {}

    let contents = fs::read_to_string(&log_path)?;
    let prompt_line = contents
        .lines()
        .find(|line| line.contains("\"type\":\"session.prompt\""))
        .expect("captured prompt line");
    let payload: serde_json::Value = serde_json::from_str(prompt_line)?;
    assert_eq!(payload.get("model"), None);

    Ok(())
}

#[tokio::test]
async fn shutdown_cached_session_is_not_live_and_gets_replaced() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec!["-c".into(), "cat >/dev/null".into()],
    );
    let env = crp_test_env();
    let session_key = "session-restart-after-shutdown";

    let first = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;
    assert!(adapter.has_live_session(session_key).await);

    first.process.shutdown("simulated runtime exit").await;

    assert!(!adapter.has_live_session(session_key).await);

    let second = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;
    assert!(
        !Arc::ptr_eq(&first, &second),
        "expected a dead cached session to be replaced"
    );
    assert_eq!(session_shutdown_reason(&second), None);

    second.process.shutdown("test complete").await;
    Ok(())
}

#[tokio::test]
async fn get_or_create_session_replaces_live_session_when_launch_env_changes() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec!["-c".into(), "while IFS= read -r _line; do :; done".into()],
    );
    let session_key = "session-refresh-after-env-change";
    let mut env = crp_test_env();
    env.insert(
        "CODEX_HOME".to_string(),
        workdir.join("codex-home-one").to_string_lossy().to_string(),
    );

    let first = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;
    let same = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;
    assert!(
        Arc::ptr_eq(&first, &same),
        "identical launch env should keep reusing the live session"
    );

    let mut changed_env = env.clone();
    changed_env.insert(
        "CODEX_HOME".to_string(),
        workdir.join("codex-home-two").to_string_lossy().to_string(),
    );
    let second = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &changed_env)
        .await?;

    assert!(
        !Arc::ptr_eq(&first, &second),
        "changed launch env must replace the stale live process"
    );
    assert_eq!(adapter.pool.session_count_for_test().await, 1);
    assert!(
        session_shutdown_reason(&first)
            .as_deref()
            .unwrap_or_default()
            .contains("CRP launch environment refresh"),
        "stale process should record why it was replaced"
    );
    assert_eq!(session_shutdown_reason(&second), None);

    second.process.shutdown("test complete").await;
    Ok(())
}

#[tokio::test]
async fn get_or_create_session_reuses_live_session_when_only_live_fields_change() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec!["-c".into(), "while IFS= read -r _line; do :; done".into()],
    );
    let session_key = "session-retained-after-live-env-change";
    let mut env = crp_test_env();
    env.insert(
        "CODEX_HOME".to_string(),
        workdir.join("codex-home").to_string_lossy().to_string(),
    );
    env.insert("CTX_MODEL_ID".to_string(), "gpt-5.4".to_string());

    let first = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;

    let mut changed_env = env.clone();
    changed_env.insert(
        "CTX_PROVIDER_SESSION_REF".to_string(),
        "provider-session-ref".to_string(),
    );
    changed_env.insert("CTX_MODEL_ID".to_string(), "gpt-5.5".to_string());
    changed_env.insert(
        "CTX_SYSTEM_PROMPT_APPEND".to_string(),
        "updated prompt".to_string(),
    );
    changed_env.insert("CTX_RUN_GRANT_ID".to_string(), "grant-two".to_string());
    changed_env.insert("CTX_POLICY_VERSION".to_string(), "policy-two".to_string());

    let second = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &changed_env)
        .await?;

    assert!(
        Arc::ptr_eq(&first, &second),
        "live-session env fields must not force an already-open runtime to restart"
    );
    assert_eq!(session_shutdown_reason(&first), None);

    second.process.shutdown("test complete").await;
    Ok(())
}

#[tokio::test]
async fn reap_idle_sessions_reaps_quiescent_live_session() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = write_session_status_runtime(&workdir, "quiescent-status.sh", true)?;
    let adapter = Tier1CrpAdapter::from_raw(
        "codex",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "quiescent-reap";
    let env = crp_test_env();

    let _session = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;

    let stats = adapter
        .pool
        .reap_idle_sessions(immediate_sweep_config())
        .await;

    assert_eq!(
        stats,
        ProviderSessionSweepStats {
            reaped: 1,
            ..ProviderSessionSweepStats::default()
        }
    );
    assert!(!adapter.has_live_session(session_key).await);
    assert!(adapter.pool.list_processes().await.is_empty());
    Ok(())
}

#[tokio::test]
async fn provider_runtime_sessions_skip_status_probe_when_opened_metadata_omits_capability(
) -> Result<()> {
    let _env_lock = ENV_LOCK.lock().await;
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("provider-runtime-status-default.sh");
    let log_path = workdir.join("provider-runtime-status-default.log");

    fs::write(
        &script_path,
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  case "$line" in
    *'"type":"session.open"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":1,"channel":"control","type":"session.opened","session_id":"%s"}\n' "$session_id"
      ;;
    *'"type":"session.prompt"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      turn_id=$(printf '%s' "$line" | sed -n 's/.*"turn_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":2,"channel":"control","type":"turn.completed","session_id":"%s","turn_id":"%s","status":"success"}\n' "$session_id" "$turn_id"
      ;;
    *'"type":"session.status"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":3,"channel":"control","type":"session.notice","session_id":"%s","code":"session_status","severity":"info","details":{"quiescent":true}}\n' "$session_id"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_provider_runtime(
        "codex",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let mut env = crp_test_env();
    env.insert(
        "CTX_SESSION_ID".to_string(),
        "runtime-status-default".to_string(),
    );
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );

    let (event_sink, _event_rx) = mpsc::channel(8);
    let handle = adapter
        .run(
            TurnInput {
                content: "ping".to_string(),
                attachments: Vec::new(),
                context_blocks: Vec::new(),
                model_id: None,
            },
            workdir.clone(),
            env,
            event_sink,
            crate::adapters::ProviderRunHooks::default(),
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(5), handle.done)
        .await
        .context("prompt run should finish")??;

    let stats = adapter
        .pool
        .reap_idle_sessions(immediate_sweep_config())
        .await;
    assert_eq!(stats, ProviderSessionSweepStats::default());
    assert!(adapter.has_live_session("runtime-status-default").await);
    let log_contents = fs::read_to_string(&log_path)?;
    assert!(
        !log_contents.contains(r#""type":"session.status""#),
        "provider runtimes must opt in before ctx probes session.status"
    );
    let session = adapter
        .pool
        .require_open_session("runtime-status-default")
        .await?;
    session.process.shutdown("test complete").await;
    Ok(())
}

#[tokio::test]
async fn crp_rejects_unsupported_launch_policy_before_spawning_runtime() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("unsupported-launch-policy.sh");
    let log_path = workdir.join("unsupported-launch-policy.log");

    fs::write(
        &script_path,
        r#"#!/bin/sh
printf 'spawned\n' > "$LOG_FILE"
while IFS= read -r _line; do
  :
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_provider_runtime(
        "codex",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "unsupported-launch-policy-session";
    let mut env = crp_test_env();
    env.insert("CTX_SESSION_ID".to_string(), session_key.to_string());
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );
    env.insert(
        CTX_CRP_LAUNCH_POLICY_ENV.to_string(),
        "danger-full-access".to_string(),
    );
    let (event_sink, _event_rx) = mpsc::channel(8);

    let handle = adapter
        .run(
            TurnInput {
                content: "ping".to_string(),
                attachments: Vec::new(),
                context_blocks: Vec::new(),
                model_id: None,
            },
            workdir,
            env,
            event_sink,
            crate::adapters::ProviderRunHooks::default(),
        )
        .await?;

    let outcome = tokio::time::timeout(Duration::from_secs(5), handle.outcome)
        .await
        .context("unsupported launch policy should finish promptly")??;
    assert_eq!(outcome.status, ProviderTurnStatus::Failed);
    assert!(outcome
        .message
        .as_deref()
        .unwrap_or_default()
        .contains("unsupported CTX_CRP_LAUNCH_POLICY"));
    tokio::time::timeout(Duration::from_secs(5), handle.done)
        .await
        .context("unsupported launch policy done signal should finish promptly")??;
    assert!(
        !log_path.exists(),
        "unsupported launch policy must fail before spawning CRP runtime"
    );
    Ok(())
}

#[tokio::test]
async fn crp_launch_policy_change_reopens_pooled_session() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("launch-policy-refresh.sh");
    let log_path = workdir.join("launch-policy-refresh.log");

    fs::write(
        &script_path,
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  case "$line" in
    *'"type":"session.open"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":1,"channel":"control","type":"session.opened","session_id":"%s"}\n' "$session_id"
      ;;
    *'"type":"session.prompt"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      turn_id=$(printf '%s' "$line" | sed -n 's/.*"turn_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":2,"channel":"control","type":"turn.completed","session_id":"%s","turn_id":"%s","status":"success"}\n' "$session_id" "$turn_id"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_provider_runtime(
        "codex",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "launch-policy-refresh-session";

    for launch_policy in [
        Some(CTX_CRP_LAUNCH_POLICY_FULL),
        None,
        Some(CTX_CRP_LAUNCH_POLICY_FULL),
    ] {
        let mut env = crp_test_env();
        env.insert("CTX_SESSION_ID".to_string(), session_key.to_string());
        env.insert(
            "LOG_FILE".to_string(),
            log_path.to_string_lossy().to_string(),
        );
        if let Some(launch_policy) = launch_policy {
            env.insert(
                CTX_CRP_LAUNCH_POLICY_ENV.to_string(),
                launch_policy.to_string(),
            );
        }
        let (event_sink, _event_rx) = mpsc::channel(8);
        let handle = adapter
            .run(
                TurnInput {
                    content: "ping".to_string(),
                    attachments: Vec::new(),
                    context_blocks: Vec::new(),
                    model_id: None,
                },
                workdir.clone(),
                env,
                event_sink,
                crate::adapters::ProviderRunHooks::default(),
            )
            .await?;
        tokio::time::timeout(Duration::from_secs(5), handle.done)
            .await
            .context("prompt run should finish")??;
        assert!(
            adapter.has_live_session(session_key).await,
            "launch-policy refresh should leave the replacement session reusable"
        );
    }

    let log_contents = fs::read_to_string(&log_path)?;
    let open_lines = log_contents
        .lines()
        .filter(|line| line.contains(r#""type":"session.open""#))
        .collect::<Vec<_>>();
    assert_eq!(
        open_lines.len(),
        3,
        "CRP must reopen whenever launch policy changes: {log_contents}"
    );
    assert!(open_lines[0].contains(FULL_YOLO_SANDBOX_MODE));
    assert!(!open_lines[1].contains(FULL_YOLO_SANDBOX_MODE));
    assert!(open_lines[2].contains(FULL_YOLO_SANDBOX_MODE));
    Ok(())
}

#[tokio::test]
async fn scoped_mcp_prompt_drains_crp_session_so_next_turn_reopens_with_fresh_bootstrap(
) -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("mcp-refresh.sh");
    let log_path = workdir.join("mcp-refresh.log");

    fs::write(
        &script_path,
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  case "$line" in
    *'"type":"session.open"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":1,"channel":"control","type":"session.opened","session_id":"%s"}\n' "$session_id"
      ;;
    *'"type":"session.prompt"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      turn_id=$(printf '%s' "$line" | sed -n 's/.*"turn_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":2,"channel":"control","type":"turn.completed","session_id":"%s","turn_id":"%s","status":"success"}\n' "$session_id" "$turn_id"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_provider_runtime(
        "codex",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "mcp-refresh-session";

    let mut unscoped_env = HashMap::new();
    unscoped_env.insert("CTX_SESSION_ID".to_string(), session_key.to_string());
    unscoped_env.insert(
        "CTX_DAEMON_URL".to_string(),
        "http://127.0.0.1:4399".to_string(),
    );
    unscoped_env.insert("CTX_MCP_DISABLED".to_string(), "1".to_string());
    unscoped_env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );
    let (event_sink, _event_rx) = mpsc::channel(8);
    let handle = adapter
        .run(
            TurnInput {
                content: "ping".to_string(),
                attachments: Vec::new(),
                context_blocks: Vec::new(),
                model_id: None,
            },
            workdir.clone(),
            unscoped_env,
            event_sink,
            crate::adapters::ProviderRunHooks::default(),
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(5), handle.done)
        .await
        .context("unscoped prompt run should finish")??;
    assert!(
        adapter.has_live_session(session_key).await,
        "unscoped CRP prompt should leave the reusable session pooled"
    );

    for token in ["token-one", "token-two"] {
        let mut env = HashMap::new();
        env.insert("CTX_SESSION_ID".to_string(), session_key.to_string());
        env.insert(
            "CTX_DAEMON_URL".to_string(),
            "http://127.0.0.1:4399".to_string(),
        );
        env.insert(
            "CTX_MCP_COMMAND".to_string(),
            script_path.to_string_lossy().to_string(),
        );
        env.insert("CTX_MCP_TOKEN".to_string(), token.to_string());
        env.insert(
            "LOG_FILE".to_string(),
            log_path.to_string_lossy().to_string(),
        );
        let (event_sink, _event_rx) = mpsc::channel(8);
        let handle = adapter
            .run(
                TurnInput {
                    content: "ping".to_string(),
                    attachments: Vec::new(),
                    context_blocks: Vec::new(),
                    model_id: None,
                },
                workdir.clone(),
                env,
                event_sink,
                crate::adapters::ProviderRunHooks::default(),
            )
            .await?;
        tokio::time::timeout(Duration::from_secs(5), handle.done)
            .await
            .context("prompt run should finish")??;
        assert!(
            !adapter.has_live_session(session_key).await,
            "scoped MCP runs must not keep a CRP session with a stale token"
        );
    }

    let log_contents = fs::read_to_string(&log_path)?;
    let open_count = log_contents
        .lines()
        .filter(|line| line.contains(r#""type":"session.open""#))
        .count();
    let prompt_count = log_contents
        .lines()
        .filter(|line| line.contains(r#""type":"session.prompt""#))
        .count();
    assert_eq!(
        open_count, 3,
        "each scoped MCP turn must reopen CRP so session.open carries the fresh token: {log_contents}"
    );
    assert_eq!(prompt_count, 3);
    assert!(log_contents.contains("token-one"));
    assert!(log_contents.contains("token-two"));
    Ok(())
}

#[tokio::test]
async fn reap_idle_sessions_reaps_unopened_session_without_status_probe() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("capture-unopened.sh");
    let log_path = workdir.join("capture-unopened.log");

    fs::write(
        &script_path,
        "#!/bin/sh\nwhile IFS= read -r line; do\n  printf '%s\\n' \"$line\" >> \"$LOG_FILE\"\ndone\n",
    )?;
    fs::write(&log_path, "")?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "opencode",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "unopened-reap";
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );

    let _session = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;

    let stats = adapter
        .pool
        .reap_idle_sessions(immediate_sweep_config())
        .await;

    assert_eq!(
        stats,
        ProviderSessionSweepStats {
            reaped: 1,
            ..ProviderSessionSweepStats::default()
        }
    );
    assert!(!adapter.has_live_session(session_key).await);
    let log_contents = fs::read_to_string(&log_path)?;
    assert!(
        !log_contents.contains(r#""type":"session.status""#),
        "unopened sessions must be reaped without a session.status probe"
    );
    Ok(())
}

#[tokio::test]
async fn reap_idle_sessions_keeps_busy_session() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = write_session_status_runtime(&workdir, "busy-status.sh", false)?;
    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "busy-reap";
    let env = crp_test_env();

    let session = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;
    session.opened.store(true, Ordering::SeqCst);

    let stats = adapter
        .pool
        .reap_idle_sessions(immediate_sweep_config())
        .await;

    assert_eq!(
        stats,
        ProviderSessionSweepStats {
            skipped_busy: 1,
            ..ProviderSessionSweepStats::default()
        }
    );
    assert!(adapter.has_live_session(session_key).await);

    session.process.shutdown("test complete").await;
    Ok(())
}

#[tokio::test]
async fn reap_idle_sessions_keeps_pinned_session() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = write_session_status_runtime(&workdir, "pinned-status.sh", true)?;
    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "pinned-reap";
    let env = crp_test_env();

    let session = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;
    session.opened.store(true, Ordering::SeqCst);
    adapter
        .pool
        .set_session_pinned(session_key.to_string(), true);

    let stats = adapter
        .pool
        .reap_idle_sessions(immediate_sweep_config())
        .await;

    assert_eq!(stats, ProviderSessionSweepStats::default());
    assert!(adapter.has_live_session(session_key).await);

    adapter
        .pool
        .set_session_pinned(session_key.to_string(), false);
    session.process.shutdown("test complete").await;
    Ok(())
}

#[tokio::test]
async fn reap_idle_sessions_removes_dead_sessions_without_status_probe() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec!["-c".into(), "cat >/dev/null".into()],
    );
    let session_key = "dead-reap";
    let env = crp_test_env();

    let session = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;
    session.process.shutdown("simulated runtime exit").await;

    let stats = adapter
        .pool
        .reap_idle_sessions(immediate_sweep_config())
        .await;

    assert_eq!(
        stats,
        ProviderSessionSweepStats {
            dead_removed: 1,
            ..ProviderSessionSweepStats::default()
        }
    );
    assert!(!adapter.has_live_session(session_key).await);
    Ok(())
}

#[tokio::test]
async fn reap_idle_sessions_skips_probe_for_runtimes_without_session_status() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("capture-idle.sh");
    let log_path = workdir.join("capture-idle.log");

    fs::write(
        &script_path,
        "#!/bin/sh\nwhile IFS= read -r line; do\n  printf '%s\\n' \"$line\" >> \"$LOG_FILE\"\ndone\n",
    )?;
    fs::write(&log_path, "")?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw_with_session_status(
        "unsupported-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
        false,
    );
    let session_key = "unsupported-probe";
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );

    let session = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;
    session.opened.store(true, Ordering::SeqCst);

    let stats = adapter
        .pool
        .reap_idle_sessions(immediate_sweep_config())
        .await;

    assert_eq!(stats, ProviderSessionSweepStats::default());
    assert!(adapter.has_live_session(session_key).await);
    let log_contents = fs::read_to_string(&log_path)?;
    assert!(
        !log_contents.contains(r#""type":"session.status""#),
        "unsupported runtimes must not be probed for session.status"
    );

    session.process.shutdown("test complete").await;
    Ok(())
}

#[tokio::test]
async fn get_or_create_session_over_cap_does_not_probe_status_inline() -> Result<()> {
    let _env_lock = ENV_LOCK.lock().await;
    let _max_idle_guard = ScopedEnvVar::set("CTX_PROVIDER_WORKER_MAX_IDLE_SESSIONS", "1");

    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("capture-over-cap.sh");
    let log_path = workdir.join("capture-over-cap.log");

    fs::write(
        &script_path,
        "#!/bin/sh\nwhile IFS= read -r line; do\n  printf '%s\\n' \"$line\" >> \"$LOG_FILE\"\ndone\n",
    )?;
    fs::write(&log_path, "")?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_provider_runtime(
        "codex",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );

    let first = adapter
        .pool
        .get_or_create_session("first-session", &workdir, &env)
        .await?;
    first.opened.store(true, Ordering::SeqCst);

    let second = tokio::time::timeout(
        Duration::from_secs(1),
        adapter
            .pool
            .get_or_create_session("second-session", &workdir, &env),
    )
    .await
    .context("timed out creating second session over idle cap")??;
    second.opened.store(true, Ordering::SeqCst);

    let log_contents = fs::read_to_string(&log_path)?;
    assert!(
        !log_contents.contains(r#""type":"session.status""#),
        "over-cap session creation must not synchronously probe session.status"
    );

    first.process.shutdown("test complete").await;
    second.process.shutdown("test complete").await;
    Ok(())
}

#[tokio::test]
async fn reap_idle_sessions_never_kills_in_flight_model_update() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("set-model-busy.sh");
    let log_path = workdir.join("set-model-busy.log");
    let model_seen_path = workdir.join("model-seen");
    let allow_model_path = workdir.join("allow-model");

    fs::write(
        &script_path,
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  case "$line" in
    *'"type":"session.set_model"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      : > "$MODEL_SEEN_FILE"
      while [ ! -f "$ALLOW_MODEL_FILE" ]; do
        sleep 0.02
      done
      printf '{"v":1,"seq":1,"channel":"control","type":"session.notice","session_id":"%s","code":"session_model_updated","severity":"info","details":{"model_id":"amp-medium"}}\n' "$session_id"
      ;;
    *'"type":"session.status"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":2,"channel":"control","type":"session.notice","session_id":"%s","code":"session_status","severity":"info","details":{"quiescent":true}}\n' "$session_id"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "opencode",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "busy-model-update";
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );
    env.insert(
        "MODEL_SEEN_FILE".to_string(),
        model_seen_path.to_string_lossy().to_string(),
    );
    env.insert(
        "ALLOW_MODEL_FILE".to_string(),
        allow_model_path.to_string_lossy().to_string(),
    );

    let session = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;
    session.opened.store(true, Ordering::SeqCst);

    let adapter_for_task = adapter.clone();
    let set_model = tokio::spawn(async move {
        adapter_for_task
            .set_session_model(session_key.to_string(), "amp-medium".to_string())
            .await
    });

    let started = Instant::now();
    while !model_seen_path.exists() {
        if started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for session.set_model");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let stats = adapter
        .pool
        .reap_idle_sessions(immediate_sweep_config())
        .await;
    assert_eq!(stats, ProviderSessionSweepStats::default());
    assert!(adapter.has_live_session(session_key).await);

    let log_contents = fs::read_to_string(&log_path)?;
    assert!(
        !log_contents.contains(r#""type":"session.status""#),
        "busy session.set_model must not be status-probed"
    );

    fs::write(&allow_model_path, "")?;
    set_model.await??;

    session.process.shutdown("test complete").await;
    Ok(())
}

#[tokio::test]
async fn draining_model_update_session_shuts_down_after_completion() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("set-model-drain.sh");
    let log_path = workdir.join("set-model-drain.log");
    let model_seen_path = workdir.join("model-seen");
    let allow_model_path = workdir.join("allow-model");

    fs::write(
        &script_path,
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  case "$line" in
    *'"type":"session.set_model"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      : > "$MODEL_SEEN_FILE"
      while [ ! -f "$ALLOW_MODEL_FILE" ]; do
        sleep 0.02
      done
      printf '{"v":1,"seq":1,"channel":"control","type":"session.notice","session_id":"%s","code":"session_model_updated","severity":"info","details":{"model_id":"amp-medium"}}\n' "$session_id"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "codex",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "draining-model-update";
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );
    env.insert(
        "MODEL_SEEN_FILE".to_string(),
        model_seen_path.to_string_lossy().to_string(),
    );
    env.insert(
        "ALLOW_MODEL_FILE".to_string(),
        allow_model_path.to_string_lossy().to_string(),
    );

    let session = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;
    session.opened.store(true, Ordering::SeqCst);

    let adapter_for_task = adapter.clone();
    let set_model = tokio::spawn(async move {
        adapter_for_task
            .set_session_model(session_key.to_string(), "amp-medium".to_string())
            .await
    });

    let started = Instant::now();
    while !model_seen_path.exists() {
        if started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for session.set_model");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    adapter.pool.restart_drain("test drain").await;
    assert_eq!(adapter.pool.list_processes().await.len(), 1);

    fs::write(&allow_model_path, "")?;
    set_model.await??;

    let shutdown_started = Instant::now();
    while adapter.has_live_session(session_key).await {
        if shutdown_started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for draining model-update session shutdown");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert!(adapter.pool.list_processes().await.is_empty());
    Ok(())
}

#[tokio::test]
async fn draining_model_update_send_failure_shuts_down_session() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("set-model-closed-stdin.sh");

    fs::write(&script_path, "#!/bin/sh\nexec <&-\nsleep 30\n")?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "draining-model-send-failure";
    let env = crp_test_env();

    let session = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;
    session.opened.store(true, Ordering::SeqCst);
    session.draining.store(true, Ordering::SeqCst);

    let err = adapter
        .set_session_model(session_key.to_string(), "amp-medium".to_string())
        .await
        .expect_err("closed stdin should fail session.set_model");
    assert!(!err.to_string().trim().is_empty());
    assert!(!adapter.has_live_session(session_key).await);
    Ok(())
}

#[tokio::test]
async fn canceled_prompt_clears_opening_and_reaps_unopened_session() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("opening-cancel.sh");
    let log_path = workdir.join("opening-cancel.log");

    fs::write(
        &script_path,
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "opening-cancel";
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );

    let (event_tx, _event_rx) = mpsc::channel(8);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let request = CrpPromptRequest {
        session_key: session_key.to_string(),
        input: TurnInput {
            content: "work".to_string(),
            attachments: Vec::new(),
            context_blocks: Vec::new(),
            model_id: None,
        },
        workdir: workdir.clone(),
        env: env.clone(),
        event_sink: event_tx,
        provider_unknown_event: None,
        provider_session_ref_claim: None,
        cancel_rx,
    };

    let pool = Arc::clone(&adapter.pool);
    let prompt_task = tokio::spawn(async move { pool.prompt(request).await });

    let started = Instant::now();
    loop {
        if let Ok(contents) = fs::read_to_string(&log_path) {
            if contents.contains(r#""type":"session.open""#) {
                break;
            }
        }
        if started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for session.open");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    cancel_tx.send(()).expect("cancel signal should send");
    let _ = tokio::time::timeout(Duration::from_secs(5), prompt_task)
        .await
        .context("timed out waiting for canceled prompt")??;

    let stats = adapter
        .pool
        .reap_idle_sessions(immediate_sweep_config())
        .await;

    assert_eq!(
        stats,
        ProviderSessionSweepStats {
            reaped: 1,
            ..ProviderSessionSweepStats::default()
        }
    );
    assert!(!adapter.has_live_session(session_key).await);
    Ok(())
}

#[tokio::test]
async fn prompt_setup_error_clears_opening_and_reaps_unopened_session() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("opening-setup-error.sh");
    let log_path = workdir.join("opening-setup-error.log");

    fs::write(
        &script_path,
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  if printf '%s' "$line" | grep -q '"type":"session.open"'; then
    session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
    printf '{"v":1,"seq":1,"channel":"control","type":"session.opened","session_id":"%s"}\n' "$session_id"
  fi
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "opencode",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "opening-setup-error";
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );

    let (event_tx, _event_rx) = mpsc::channel(8);
    let (_cancel_tx, cancel_rx) = oneshot::channel();
    let request = CrpPromptRequest {
        session_key: session_key.to_string(),
        input: TurnInput {
            content: "please inspect this image".to_string(),
            attachments: vec![ctx_core::models::MessageAttachment::Image {
                mime_type: "image/png".to_string(),
                data_base64: base64::engine::general_purpose::STANDARD.encode(b"png"),
                name: Some("fixture.png".to_string()),
            }],
            context_blocks: Vec::new(),
            model_id: None,
        },
        workdir: workdir.clone(),
        env,
        event_sink: event_tx,
        provider_unknown_event: None,
        provider_session_ref_claim: None,
        cancel_rx,
    };

    let err = tokio::time::timeout(Duration::from_secs(5), adapter.pool.prompt(request))
        .await
        .context("timed out waiting for prompt setup error")?
        .expect_err("non-text prompt items should fail prompt setup");
    assert!(err
        .to_string()
        .contains("provider requires text-only ACP prompt items"));

    let started = Instant::now();
    loop {
        if let Ok(contents) = fs::read_to_string(&log_path) {
            if contents.contains(r#""type":"session.open""#) {
                assert!(
                    !contents.contains(r#""type":"session.prompt""#),
                    "prompt send should not happen after setup validation fails"
                );
                break;
            }
        }
        if started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for session.open capture");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let stats = adapter
        .pool
        .reap_idle_sessions(immediate_sweep_config())
        .await;

    assert_eq!(stats, ProviderSessionSweepStats::default());
    assert!(!adapter.has_live_session(session_key).await);
    Ok(())
}

#[tokio::test]
async fn prompt_times_out_when_runtime_never_emits_first_event() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("silent-runtime.sh");
    let log_path = workdir.join("silent-runtime.log");

    fs::write(
        &script_path,
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "silent-runtime";
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );
    env.insert(
        "CTX_CRP_FIRST_EVENT_TIMEOUT_MS".to_string(),
        "50".to_string(),
    );

    let (event_tx, mut event_rx) = mpsc::channel(8);
    let (_cancel_tx, cancel_rx) = oneshot::channel();
    let request = CrpPromptRequest {
        session_key: session_key.to_string(),
        input: TurnInput {
            content: "work".to_string(),
            attachments: Vec::new(),
            context_blocks: Vec::new(),
            model_id: None,
        },
        workdir: workdir.clone(),
        env,
        event_sink: event_tx,
        provider_unknown_event: None,
        provider_session_ref_claim: None,
        cancel_rx,
    };

    let outcome = tokio::time::timeout(Duration::from_secs(5), adapter.pool.prompt(request))
        .await
        .context("timed out waiting for first-event timeout")??;
    assert_eq!(outcome.status, ProviderTurnStatus::Failed);
    assert_eq!(outcome.reason.as_deref(), Some("provider_startup_timeout"));
    assert_eq!(outcome.kind, Some(json!("provider_startup_timeout")));
    assert!(event_rx.try_recv().is_err());

    let stats = adapter
        .pool
        .reap_idle_sessions(immediate_sweep_config())
        .await;
    assert_eq!(
        stats,
        ProviderSessionSweepStats {
            dead_removed: 1,
            ..ProviderSessionSweepStats::default()
        }
    );
    assert!(!adapter.has_live_session(session_key).await);
    Ok(())
}

#[tokio::test]
async fn prompt_scrubs_ambient_provider_mode_when_request_env_omits_it() -> Result<()> {
    let _env_guard = ENV_LOCK.lock().await;
    let _provider_mode = ScopedEnvVar::set("CTX_PROVIDER_MODE", "full-access");
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("ambient-provider-mode-runtime.sh");
    let log_path = workdir.join("ambient-provider-mode.log");

    fs::write(
        &script_path,
        r#"#!/bin/sh
printf 'provider_mode=%s\n' "${CTX_PROVIDER_MODE-}" >> "$LOG_FILE"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  if printf '%s' "$line" | grep -q '"type":"session.open"'; then
    printf '{"v":1,"seq":1,"channel":"control","type":"session.opened","session_id":"ambient-provider-mode"}\n'
  fi
  if printf '%s' "$line" | grep -q '"type":"session.prompt"'; then
    turn_id=$(printf '%s' "$line" | sed -n 's/.*"turn_id":"\([^"]*\)".*/\1/p')
    printf '{"v":1,"seq":2,"channel":"control","type":"turn.completed","session_id":"ambient-provider-mode","turn_id":"%s","status":"success"}\n' "$turn_id"
  fi
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );
    let (event_tx, _event_rx) = mpsc::channel(8);
    let (_cancel_tx, cancel_rx) = oneshot::channel();
    let request = CrpPromptRequest {
        session_key: "ambient-provider-mode".to_string(),
        input: TurnInput {
            content: "work".to_string(),
            attachments: Vec::new(),
            context_blocks: Vec::new(),
            model_id: None,
        },
        workdir: workdir.clone(),
        env,
        event_sink: event_tx,
        provider_unknown_event: None,
        provider_session_ref_claim: None,
        cancel_rx,
    };

    let outcome = tokio::time::timeout(Duration::from_secs(5), adapter.pool.prompt(request))
        .await
        .context("timed out waiting for prompt outcome")??;
    assert_eq!(outcome.status, ProviderTurnStatus::Completed);

    let log = fs::read_to_string(&log_path)?;
    assert!(
        log.lines().any(|line| line == "provider_mode="),
        "expected runtime to observe a scrubbed CTX_PROVIDER_MODE, got log:\n{log}"
    );
    assert!(
        !log.contains("provider_mode=full-access"),
        "ambient CTX_PROVIDER_MODE leaked into child runtime:\n{log}"
    );
    Ok(())
}

#[tokio::test]
async fn prompt_interrupts_for_auth_required_stderr_before_first_event() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("auth-required-runtime.sh");

    fs::write(
        &script_path,
        r#"#!/bin/sh
printf 'Authentication required: https://auth.openai.com/oauth/authorize?token=secret\n' >&2
while IFS= read -r _line; do
  :
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "codex",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "auth-required-before-first-event";
    let mut env = crp_test_env();
    env.insert(
        "CTX_CRP_FIRST_EVENT_TIMEOUT_MS".to_string(),
        "250".to_string(),
    );

    let (event_tx, mut event_rx) = mpsc::channel(8);
    let (_cancel_tx, cancel_rx) = oneshot::channel();
    let request = CrpPromptRequest {
        session_key: session_key.to_string(),
        input: TurnInput {
            content: "work".to_string(),
            attachments: Vec::new(),
            context_blocks: Vec::new(),
            model_id: None,
        },
        workdir: workdir.clone(),
        env,
        event_sink: event_tx,
        provider_unknown_event: None,
        cancel_rx,
        provider_session_ref_claim: None,
    };

    let outcome = tokio::time::timeout(Duration::from_secs(5), adapter.pool.prompt(request))
        .await
        .context("timed out waiting for auth-required startup interruption")??;
    assert_eq!(outcome.status, ProviderTurnStatus::Interrupted);
    assert_eq!(outcome.reason.as_deref(), Some("auth_required"));

    let notice_event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
        .await
        .context("timed out waiting for auth-required notice")?
        .context("missing auth-required notice event")?;
    assert!(matches!(notice_event.event_type, SessionEventType::Notice));
    assert_eq!(
        notice_event.payload_json.get("kind"),
        Some(&json!("auth_required"))
    );
    assert_eq!(
        notice_event.payload_json.get("code"),
        Some(&json!("auth_required"))
    );
    assert_eq!(
        notice_event.payload_json.get("message"),
        Some(&json!("Authentication required."))
    );
    assert_eq!(
        notice_event.payload_json.get("source"),
        Some(&json!("crp_stderr"))
    );
    assert_eq!(notice_event.payload_json.get("auth_url"), None);

    let interrupted_event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
        .await
        .context("timed out waiting for auth-required terminal event")?
        .context("missing auth-required terminal event")?;
    assert!(matches!(
        interrupted_event.event_type,
        SessionEventType::TurnInterrupted
    ));
    assert_eq!(
        interrupted_event.payload_json.get("reason"),
        Some(&json!("auth_required"))
    );

    let stats = adapter
        .pool
        .reap_idle_sessions(immediate_sweep_config())
        .await;
    assert_eq!(
        stats,
        ProviderSessionSweepStats {
            dead_removed: 1,
            ..ProviderSessionSweepStats::default()
        }
    );
    assert!(!adapter.has_live_session(session_key).await);
    Ok(())
}

#[tokio::test]
async fn prompt_surfaces_auth_error_stderr_before_first_event() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("auth-error-runtime.sh");

    fs::write(
        &script_path,
        r#"#!/bin/sh
printf "message: 'Interactive consent could not be obtained.'\n" >&2
while IFS= read -r _line; do
  :
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "gemini",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "auth-error-before-first-event";
    let mut env = crp_test_env();
    env.insert(
        "CTX_CRP_FIRST_EVENT_TIMEOUT_MS".to_string(),
        "250".to_string(),
    );

    let (event_tx, mut event_rx) = mpsc::channel(8);
    let (_cancel_tx, cancel_rx) = oneshot::channel();
    let request = CrpPromptRequest {
        session_key: session_key.to_string(),
        input: TurnInput {
            content: "work".to_string(),
            attachments: Vec::new(),
            context_blocks: Vec::new(),
            model_id: None,
        },
        workdir: workdir.clone(),
        env,
        event_sink: event_tx,
        provider_unknown_event: None,
        cancel_rx,
        provider_session_ref_claim: None,
    };

    let outcome = tokio::time::timeout(Duration::from_secs(5), adapter.pool.prompt(request))
        .await
        .context("timed out waiting for auth-error startup failure")??;
    assert_eq!(outcome.status, ProviderTurnStatus::Failed);
    assert_eq!(
        outcome.message.as_deref(),
        Some("Gemini CLI could not obtain interactive OAuth consent in this environment.")
    );
    assert_ne!(outcome.reason.as_deref(), Some("provider_startup_timeout"));
    assert_eq!(outcome.details, Some(json!({ "source": "crp_stderr" })));
    assert_eq!(outcome.kind, Some(json!("auth_error")));
    assert!(event_rx.try_recv().is_err());

    let stats = adapter
        .pool
        .reap_idle_sessions(immediate_sweep_config())
        .await;
    assert_eq!(
        stats,
        ProviderSessionSweepStats {
            dead_removed: 1,
            ..ProviderSessionSweepStats::default()
        }
    );
    assert!(!adapter.has_live_session(session_key).await);
    Ok(())
}

#[tokio::test]
async fn completed_prompt_reaps_oldest_idle_session_in_background() -> Result<()> {
    let _env_lock = ENV_LOCK.lock().await;
    let _max_idle_guard = ScopedEnvVar::set("CTX_PROVIDER_WORKER_MAX_IDLE_SESSIONS", "1");

    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("background-reap.sh");

    fs::write(
        &script_path,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"session.open"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":1,"channel":"control","type":"session.opened","session_id":"%s","supports_session_status":true}\n' "$session_id"
      ;;
    *'"type":"session.prompt"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      turn_id=$(printf '%s' "$line" | sed -n 's/.*"turn_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":2,"channel":"control","type":"turn.completed","session_id":"%s","turn_id":"%s","status":"success"}\n' "$session_id" "$turn_id"
      ;;
    *'"type":"session.status"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":3,"channel":"control","type":"session.notice","session_id":"%s","code":"session_status","severity":"info","details":{"quiescent":true}}\n' "$session_id"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_provider_runtime(
        "codex",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );

    for session_key in ["first-idle", "second-idle"] {
        let mut env = crp_test_env();
        env.insert("CTX_SESSION_ID".to_string(), session_key.to_string());
        let (event_sink, _event_rx) = mpsc::channel(8);
        let handle = adapter
            .run(
                TurnInput {
                    content: "ping".to_string(),
                    attachments: Vec::new(),
                    context_blocks: Vec::new(),
                    model_id: None,
                },
                workdir.clone(),
                env,
                event_sink,
                crate::adapters::ProviderRunHooks::default(),
            )
            .await?;
        tokio::time::timeout(Duration::from_secs(5), handle.done)
            .await
            .context("prompt run should finish")??;
    }

    let started = Instant::now();
    loop {
        if adapter.pool.list_processes().await.len() == 1 {
            break;
        }
        if started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for background idle reap");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert!(!adapter.has_live_session("first-idle").await);
    assert!(adapter.has_live_session("second-idle").await);

    adapter
        .restart("test complete", ProviderRestartMode::Immediate)
        .await?;
    Ok(())
}

#[tokio::test]
async fn reap_idle_sessions_scans_past_busy_oldest_session_to_enforce_cap() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("status-cap-scan.sh");

    fs::write(
        &script_path,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"session.status"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      if [ "$session_id" = "oldest-busy" ]; then
        quiescent=false
      else
        quiescent=true
      fi
      printf '{"v":1,"seq":1,"channel":"control","type":"session.notice","session_id":"%s","code":"session_status","severity":"info","details":{"quiescent":%s}}\n' "$session_id" "$quiescent"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "codex",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let env = crp_test_env();

    let oldest = adapter
        .pool
        .get_or_create_session("oldest-busy", &workdir, &env)
        .await?;
    oldest.opened.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let newest = adapter
        .pool
        .get_or_create_session("newest-quiescent", &workdir, &env)
        .await?;
    newest.opened.store(true, Ordering::SeqCst);

    let stats = adapter
        .pool
        .reap_idle_sessions(ProviderSessionSweepConfig {
            idle_ttl: Duration::from_secs(3600),
            max_idle_sessions: 1,
            interval: Duration::from_secs(60),
        })
        .await;

    assert_eq!(
        stats,
        ProviderSessionSweepStats {
            reaped: 1,
            skipped_busy: 1,
            ..ProviderSessionSweepStats::default()
        }
    );
    assert!(adapter.has_live_session("oldest-busy").await);
    assert!(!adapter.has_live_session("newest-quiescent").await);

    oldest.process.shutdown("test complete").await;
    Ok(())
}

#[tokio::test]
async fn background_reap_runs_followup_sweep_for_sessions_that_finish_mid_sweep() -> Result<()> {
    let _env_lock = ENV_LOCK.lock().await;
    let _max_idle_guard = ScopedEnvVar::set("CTX_PROVIDER_WORKER_MAX_IDLE_SESSIONS", "0");

    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("status-followup.sh");
    let log_path = workdir.join("status-followup.log");
    let first_status_seen_path = workdir.join("first-status-seen");
    let allow_first_status_path = workdir.join("allow-first-status");

    fs::write(
        &script_path,
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  case "$line" in
    *'"type":"session.status"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      if [ "$session_id" = "first-idle" ]; then
        : > "$FIRST_STATUS_SEEN_FILE"
        while [ ! -f "$ALLOW_FIRST_STATUS_FILE" ]; do
          sleep 0.02
        done
      fi
      printf '{"v":1,"seq":1,"channel":"control","type":"session.notice","session_id":"%s","code":"session_status","severity":"info","details":{"quiescent":true}}\n' "$session_id"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "codex",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );
    env.insert(
        "FIRST_STATUS_SEEN_FILE".to_string(),
        first_status_seen_path.to_string_lossy().to_string(),
    );
    env.insert(
        "ALLOW_FIRST_STATUS_FILE".to_string(),
        allow_first_status_path.to_string_lossy().to_string(),
    );

    let first = adapter
        .pool
        .get_or_create_session("first-idle", &workdir, &env)
        .await?;
    first.opened.store(true, Ordering::SeqCst);

    adapter.pool.trigger_background_reap();

    let started = Instant::now();
    while !first_status_seen_path.exists() {
        if started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for first session.status probe");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let second = adapter
        .pool
        .get_or_create_session("second-idle", &workdir, &env)
        .await?;
    second.opened.store(true, Ordering::SeqCst);

    adapter.pool.trigger_background_reap();
    fs::write(&allow_first_status_path, "")?;

    let drain_started = Instant::now();
    while !adapter.pool.list_processes().await.is_empty() {
        if drain_started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for queued background reaps");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert!(!adapter.has_live_session("first-idle").await);
    assert!(!adapter.has_live_session("second-idle").await);
    Ok(())
}

#[tokio::test]
async fn reap_idle_sessions_never_kills_active_prompt() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("active-prompt.sh");
    let log_path = workdir.join("active-prompt.log");

    fs::write(
        &script_path,
        r#"#!/bin/sh
turn_id=""
session_id=""
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  case "$line" in
    *'"type":"session.prompt"'*)
      turn_id=$(printf '%s' "$line" | sed -n 's/.*"turn_id":"\([^"]*\)".*/\1/p')
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      ;;
    *'"type":"session.cancel"'*)
      printf '{"v":1,"seq":1,"channel":"control","type":"turn.completed","session_id":"%s","turn_id":"%s","status":"interrupted"}\n' "$session_id" "$turn_id"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "active-reap";
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );

    let session = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;
    session.opened.store(true, Ordering::SeqCst);

    let (event_tx, _event_rx) = mpsc::channel(8);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let request = CrpPromptRequest {
        session_key: session_key.to_string(),
        input: TurnInput {
            content: "work".to_string(),
            attachments: Vec::new(),
            context_blocks: Vec::new(),
            model_id: None,
        },
        workdir: workdir.clone(),
        env: env.clone(),
        event_sink: event_tx,
        provider_unknown_event: None,
        provider_session_ref_claim: None,
        cancel_rx,
    };

    let pool = Arc::clone(&adapter.pool);
    let prompt_task = tokio::spawn(async move { pool.prompt(request).await });

    let started = Instant::now();
    loop {
        if let Ok(contents) = fs::read_to_string(&log_path) {
            if contents.contains(r#""type":"session.prompt""#) {
                break;
            }
        }
        if started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for active session.prompt");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let stats = adapter
        .pool
        .reap_idle_sessions(immediate_sweep_config())
        .await;
    assert_eq!(stats, ProviderSessionSweepStats::default());
    assert!(adapter.has_live_session(session_key).await);

    let log_contents = fs::read_to_string(&log_path)?;
    assert!(
        !log_contents.contains(r#""type":"session.status""#),
        "active prompt session should not be status-probed"
    );

    cancel_tx.send(()).expect("cancel signal should send");
    prompt_task.await??;
    session.process.shutdown("test complete").await;
    Ok(())
}

#[tokio::test]
async fn reap_idle_sessions_skips_session_that_becomes_active_during_status_probe() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("status-race.sh");
    let log_path = workdir.join("status-race.log");
    let status_seen_path = workdir.join("status-seen");
    let allow_status_path = workdir.join("allow-status");

    fs::write(
        &script_path,
        r#"#!/bin/sh
turn_id=""
session_id=""
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  case "$line" in
    *'"type":"session.status"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      : > "$STATUS_SEEN_FILE"
      while [ ! -f "$ALLOW_STATUS_FILE" ]; do
        sleep 0.02
      done
      printf '{"v":1,"seq":1,"channel":"control","type":"session.notice","session_id":"%s","code":"session_status","severity":"info","message":"status","details":{"quiescent":true}}\n' "$session_id"
      ;;
    *'"type":"session.prompt"'*)
      turn_id=$(printf '%s' "$line" | sed -n 's/.*"turn_id":"\([^"]*\)".*/\1/p')
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      ;;
    *'"type":"session.cancel"'*)
      printf '{"v":1,"seq":2,"channel":"control","type":"turn.completed","session_id":"%s","turn_id":"%s","status":"interrupted"}\n' "$session_id" "$turn_id"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "status-race-reap";
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );
    env.insert(
        "STATUS_SEEN_FILE".to_string(),
        status_seen_path.to_string_lossy().to_string(),
    );
    env.insert(
        "ALLOW_STATUS_FILE".to_string(),
        allow_status_path.to_string_lossy().to_string(),
    );

    let session = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;
    session.opened.store(true, Ordering::SeqCst);
    let initial_last_used = session.last_used();

    let pool = Arc::clone(&adapter.pool);
    let reap_task =
        tokio::spawn(async move { pool.reap_idle_sessions(immediate_sweep_config()).await });

    let started = Instant::now();
    while !status_seen_path.exists() {
        if started.elapsed() > Duration::from_secs(15) {
            anyhow::bail!("timed out waiting for session.status probe");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let (event_tx, mut event_rx) = mpsc::channel(8);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let request = CrpPromptRequest {
        session_key: session_key.to_string(),
        input: TurnInput {
            content: "work".to_string(),
            attachments: Vec::new(),
            context_blocks: Vec::new(),
            model_id: None,
        },
        workdir: workdir.clone(),
        env: env.clone(),
        event_sink: event_tx,
        provider_unknown_event: None,
        provider_session_ref_claim: None,
        cancel_rx,
    };

    let pool = Arc::clone(&adapter.pool);
    let prompt_task = tokio::spawn(async move { pool.prompt(request).await });

    let started = Instant::now();
    loop {
        if session.last_used() != initial_last_used {
            break;
        }
        if started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for overlapping prompt reuse");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    fs::write(&allow_status_path, "")?;

    let stats = reap_task.await?;
    assert_eq!(stats, ProviderSessionSweepStats::default());
    assert!(adapter.has_live_session(session_key).await);

    cancel_tx.send(()).expect("cancel signal should send");
    prompt_task.await??;
    while let Ok(event) = event_rx.try_recv() {
        assert_ne!(
            event.payload_json.get("code"),
            Some(&json!("session_status")),
            "sweep-only session_status notices must not leak into prompt streams"
        );
    }

    let session = adapter.pool.require_open_session(session_key).await?;
    session.process.shutdown("test complete").await;
    Ok(())
}

#[tokio::test]
async fn reap_idle_sessions_preserves_draining_sessions_until_prompt_completion() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("draining-prompt.sh");
    let log_path = workdir.join("draining-prompt.log");

    fs::write(
        &script_path,
        r#"#!/bin/sh
turn_id=""
session_id=""
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  case "$line" in
    *'"type":"session.prompt"'*)
      turn_id=$(printf '%s' "$line" | sed -n 's/.*"turn_id":"\([^"]*\)".*/\1/p')
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      ;;
    *'"type":"session.cancel"'*)
      printf '{"v":1,"seq":1,"channel":"control","type":"turn.completed","session_id":"%s","turn_id":"%s","status":"interrupted"}\n' "$session_id" "$turn_id"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "draining-reap";
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );

    let session = adapter
        .pool
        .get_or_create_session(session_key, &workdir, &env)
        .await?;
    session.opened.store(true, Ordering::SeqCst);

    let (event_tx, _event_rx) = mpsc::channel(8);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let request = CrpPromptRequest {
        session_key: session_key.to_string(),
        input: TurnInput {
            content: "work".to_string(),
            attachments: Vec::new(),
            context_blocks: Vec::new(),
            model_id: None,
        },
        workdir: workdir.clone(),
        env: env.clone(),
        event_sink: event_tx,
        provider_unknown_event: None,
        provider_session_ref_claim: None,
        cancel_rx,
    };

    let pool = Arc::clone(&adapter.pool);
    let prompt_task = tokio::spawn(async move { pool.prompt(request).await });

    let started = Instant::now();
    loop {
        if let Ok(contents) = fs::read_to_string(&log_path) {
            if contents.contains(r#""type":"session.prompt""#) {
                break;
            }
        }
        if started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for draining session.prompt");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    adapter.pool.restart_drain("test drain").await;

    let stats = adapter
        .pool
        .reap_idle_sessions(immediate_sweep_config())
        .await;
    assert_eq!(stats, ProviderSessionSweepStats::default());
    assert_eq!(adapter.pool.list_processes().await.len(), 1);

    cancel_tx.send(()).expect("cancel signal should send");
    prompt_task.await??;

    assert!(adapter.pool.list_processes().await.is_empty());
    Ok(())
}

#[tokio::test]
async fn completed_prompt_refreshes_idle_timestamp_before_reap() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("touch-after-prompt.sh");

    fs::write(
        &script_path,
        r#"#!/bin/sh
turn_id=""
session_id=""
while IFS= read -r line; do
  case "$line" in
    *'"type":"session.open"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":1,"channel":"control","type":"session.opened","session_id":"%s","provider_session_id":"provider-touch-after-prompt"}\n' "$session_id"
      ;;
    *'"type":"session.prompt"'*)
      turn_id=$(printf '%s' "$line" | sed -n 's/.*"turn_id":"\([^"]*\)".*/\1/p')
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      sleep 0.2
      printf '{"v":1,"seq":2,"channel":"control","type":"turn.completed","session_id":"%s","turn_id":"%s","status":"success"}\n' "$session_id" "$turn_id"
      ;;
    *'"type":"session.status"'*)
      printf '{"v":1,"seq":3,"channel":"control","type":"session.notice","session_id":"%s","code":"session_status","severity":"info","message":"status","details":{"quiescent":true}}\n' "$session_id"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "touch-after-prompt";
    let env = crp_test_env();

    let (event_tx, _event_rx) = mpsc::channel(8);
    let (_cancel_tx, cancel_rx) = oneshot::channel();
    let request = CrpPromptRequest {
        session_key: session_key.to_string(),
        input: TurnInput {
            content: "work".to_string(),
            attachments: Vec::new(),
            context_blocks: Vec::new(),
            model_id: None,
        },
        workdir: workdir.clone(),
        env: env.clone(),
        event_sink: event_tx,
        provider_unknown_event: None,
        provider_session_ref_claim: None,
        cancel_rx,
    };

    adapter.pool.prompt(request).await?;

    let stats = adapter
        .pool
        .reap_idle_sessions(ProviderSessionSweepConfig {
            idle_ttl: Duration::from_secs(1),
            max_idle_sessions: usize::MAX,
            interval: Duration::from_secs(60),
        })
        .await;
    assert_eq!(stats, ProviderSessionSweepStats::default());
    assert!(adapter.has_live_session(session_key).await);

    let session = adapter.pool.require_open_session(session_key).await?;
    session.process.shutdown("test complete").await;
    Ok(())
}

#[tokio::test]
async fn authenticate_session_filters_sweep_only_status_notices() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("auth-status-filter.sh");

    fs::write(
        &script_path,
        r#"#!/bin/sh
session_id=""
while IFS= read -r line; do
  case "$line" in
    *'"type":"session.open"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":1,"channel":"control","type":"session.opened","session_id":"%s","supports_session_status":true}\n' "$session_id"
      ;;
    *'"type":"session.authenticate"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":2,"channel":"control","type":"session.notice","session_id":"%s","code":"session_status","severity":"info","message":"status","details":{"quiescent":true}}\n' "$session_id"
      printf '{"v":1,"seq":3,"channel":"control","type":"session.notice","session_id":"%s","code":"authenticated","severity":"info","message":"authenticated"}\n' "$session_id"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "auth-status-filter";
    let (event_tx, mut event_rx) = mpsc::channel(16);

    adapter
        .authenticate_session(
            session_key.to_string(),
            workdir.clone(),
            crp_test_env(),
            None,
            event_tx,
            crate::adapters::ProviderRunHooks::default(),
        )
        .await?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut events = Vec::new();
    let mut saw_terminal = false;
    while Instant::now() < deadline {
        let recv = tokio::time::timeout(Duration::from_millis(250), event_rx.recv()).await;
        let Some(event) = (match recv {
            Ok(event) => event,
            Err(_) => continue,
        }) else {
            continue;
        };
        saw_terminal = event.payload_json.get("code") == Some(&json!("authenticated"));
        events.push(event);
        if saw_terminal {
            break;
        }
    }

    assert!(saw_terminal, "timed out waiting for auth terminal event");
    assert!(
        events
            .iter()
            .all(|event| event.payload_json.get("code") != Some(&json!("session_status"))),
        "sweep-only session_status notices must not leak into auth streams"
    );

    let session = adapter.pool.require_open_session(session_key).await?;
    session.process.shutdown("test complete").await;
    Ok(())
}

#[tokio::test]
async fn authenticate_session_runtime_exit_clears_unopened_session() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("auth-open-send-failure.sh");

    fs::write(&script_path, "#!/bin/sh\nexit 0\n")?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "auth-open-send-failure";
    let (event_tx, _event_rx) = mpsc::channel(8);

    let mut env = crp_test_env();
    env.insert(
        "CTX_CRP_FIRST_EVENT_TIMEOUT_MS".to_string(),
        "50".to_string(),
    );
    let auth_result = tokio::time::timeout(
        Duration::from_secs(5),
        adapter.authenticate_session(
            session_key.to_string(),
            workdir.clone(),
            env,
            None,
            event_tx,
            crate::adapters::ProviderRunHooks::default(),
        ),
    )
    .await
    .context("timed out waiting for authenticate_session cleanup path")?;
    if let Err(err) = auth_result {
        assert!(
            !err.to_string().trim().is_empty(),
            "authenticate_session should surface a useful error when auth startup fails early"
        );
    }

    let started = Instant::now();
    while adapter.pool.session_count_for_test().await != 0 {
        if started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for early-exit auth session to leave the pool");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert!(!adapter.has_live_session(session_key).await);
    Ok(())
}

#[tokio::test]
async fn authenticate_session_acp_auth_only_open_omits_mcp_servers_and_drains_session() -> Result<()>
{
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("acp-auth-only-open.sh");
    let log_path = workdir.join("stdin.log");

    fs::write(
        &script_path,
        r#"#!/bin/sh
seq=0
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  case "$line" in
    *'"type":"session.open"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      seq=$((seq + 1))
      printf '{"v":1,"seq":%s,"channel":"control","type":"session.opened","session_id":"%s","supports_session_status":true}\n' "$seq" "$session_id"
      ;;
    *'"type":"session.authenticate"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      seq=$((seq + 1))
      printf '{"v":1,"seq":%s,"channel":"control","type":"session.notice","session_id":"%s","code":"authenticated","severity":"info","message":"authenticated"}\n' "$seq" "$session_id"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_provider_runtime_acp_bridge(
        "pi",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "acp-auth-open";
    let mut env = HashMap::new();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );
    env.insert(
        "CTX_DAEMON_URL".to_string(),
        "http://127.0.0.1:4401".to_string(),
    );
    env.insert("CTX_AUTH_TOKEN".to_string(), "token-123".to_string());
    env.insert(
        "CTX_MCP_COMMAND".to_string(),
        script_path.to_string_lossy().to_string(),
    );
    let (event_tx, mut event_rx) = mpsc::channel(16);

    adapter
        .authenticate_session(
            session_key.to_string(),
            workdir.clone(),
            env,
            None,
            event_tx,
            crate::adapters::ProviderRunHooks::default(),
        )
        .await?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut events = Vec::new();
    let mut saw_terminal = false;
    while Instant::now() < deadline {
        let recv = tokio::time::timeout(Duration::from_millis(250), event_rx.recv()).await;
        let Some(event) = (match recv {
            Ok(event) => event,
            Err(_) => continue,
        }) else {
            continue;
        };
        saw_terminal = event.payload_json.get("code") == Some(&json!("authenticated"));
        events.push(event);
        if saw_terminal {
            break;
        }
    }

    assert!(saw_terminal, "timed out waiting for auth terminal event");
    assert!(
        events
            .iter()
            .all(|event| !matches!(&event.event_type, SessionEventType::Init)),
        "auth-only ACP opens must not forward session init/open events"
    );

    let started = Instant::now();
    while adapter.pool.session_count_for_test().await != 0 {
        if started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for auth-only ACP session to leave the pool");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert!(!adapter.has_live_session(session_key).await);

    let stdin_log = fs::read_to_string(&log_path)?;
    let open_line = stdin_log
        .lines()
        .find(|line| line.contains(r#""type":"session.open""#))
        .context("missing session.open line")?;
    assert!(
        !open_line.contains(r#""mcp_servers""#),
        "auth-only ACP session.open must omit MCP bootstrap: {open_line}"
    );
    assert!(
        stdin_log.contains(r#""type":"session.authenticate""#),
        "authenticate command should still be sent: {stdin_log}"
    );

    Ok(())
}

#[tokio::test]
async fn prompt_rejects_provider_session_open_mismatch_before_prompt() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("resume-mismatch.sh");
    let log_path = workdir.join("stdin.log");

    fs::write(
        &script_path,
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  case "$line" in
    *'"type":"session.open"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":1,"channel":"control","type":"session.opened","session_id":"%s","provider_session_id":"wrong-provider-ref"}\n' "$session_id"
      ;;
    *'"type":"session.prompt"'*)
      printf 'unexpected prompt\n' >> "$LOG_FILE"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );
    env.insert(
        "CTX_PROVIDER_SESSION_REF".to_string(),
        "expected-provider-ref".to_string(),
    );
    let (event_tx, _event_rx) = mpsc::channel(8);
    let (_cancel_tx, cancel_rx) = oneshot::channel();
    let err = adapter
        .pool
        .prompt(CrpPromptRequest {
            session_key: "resume-mismatch".to_string(),
            input: TurnInput {
                content: "user".to_string(),
                attachments: vec![],
                context_blocks: vec![],
                model_id: None,
            },
            workdir: workdir.clone(),
            env,
            event_sink: event_tx,
            provider_unknown_event: None,
            provider_session_ref_claim: None,
            cancel_rx,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("resume mismatch"), "{err:#}");

    let started = Instant::now();
    while adapter.pool.session_count_for_test().await != 0 {
        if started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for rejected prompt session to leave the pool");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!adapter.has_live_session("resume-mismatch").await);

    let stdin_log = fs::read_to_string(&log_path)?;
    assert!(
        !stdin_log.contains(r#""type":"session.prompt""#),
        "prompt must not be sent after open mismatch: {stdin_log}"
    );
    Ok(())
}

#[tokio::test]
async fn prompt_waits_for_provider_session_claim_before_prompt() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("claim-hook-blocks-prompt.sh");
    let log_path = workdir.join("stdin.log");

    fs::write(
        &script_path,
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  case "$line" in
    *'"type":"session.open"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":1,"channel":"control","type":"session.opened","session_id":"%s","provider_session_id":"expected-provider-ref"}\n' "$session_id"
      ;;
    *'"type":"session.prompt"'*)
      printf 'unexpected prompt\n' >> "$LOG_FILE"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );
    env.insert(
        "CTX_PROVIDER_SESSION_REF".to_string(),
        "expected-provider-ref".to_string(),
    );
    let (event_tx, _event_rx) = mpsc::channel(8);
    let (_cancel_tx, cancel_rx) = oneshot::channel();
    let err = adapter
        .pool
        .prompt(CrpPromptRequest {
            session_key: "claim-hook-blocks-prompt".to_string(),
            input: TurnInput {
                content: "user".to_string(),
                attachments: vec![],
                context_blocks: vec![],
                model_id: None,
            },
            workdir: workdir.clone(),
            env,
            event_sink: event_tx,
            provider_unknown_event: None,
            provider_session_ref_claim: Some(Arc::new(|_claim| {
                Box::pin(async { anyhow::bail!("claim rejected") })
            })),
            cancel_rx,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("claim rejected"), "{err:#}");

    let started = Instant::now();
    while adapter.pool.session_count_for_test().await != 0 {
        if started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for rejected claim-hook session to leave the pool");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!adapter.has_live_session("claim-hook-blocks-prompt").await);

    let stdin_log = fs::read_to_string(&log_path)?;
    assert!(
        !stdin_log.contains(r#""type":"session.prompt""#),
        "prompt must not be sent if provider-session claim hook fails: {stdin_log}"
    );
    Ok(())
}

#[tokio::test]
async fn authenticate_session_rejects_open_mismatch_before_authenticate() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let workdir = tempdir.path().to_path_buf();
    let script_path = workdir.join("auth-open-mismatch.sh");
    let log_path = workdir.join("stdin.log");

    fs::write(
        &script_path,
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  case "$line" in
    *'"type":"session.open"'*)
      session_id=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
      printf '{"v":1,"seq":1,"channel":"control","type":"session.opened","session_id":"%s","provider_session_id":"wrong-provider-ref"}\n' "$session_id"
      ;;
    *'"type":"session.authenticate"'*)
      printf 'unexpected authenticate\n' >> "$LOG_FILE"
      ;;
  esac
done
"#,
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let adapter = Tier1CrpAdapter::from_raw(
        "fake-crp",
        "/bin/sh".to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let mut env = crp_test_env();
    env.insert(
        "LOG_FILE".to_string(),
        log_path.to_string_lossy().to_string(),
    );
    env.insert(
        "CTX_PROVIDER_SESSION_REF".to_string(),
        "expected-provider-ref".to_string(),
    );
    let (event_tx, _event_rx) = mpsc::channel(8);

    let err = adapter
        .authenticate_session(
            "auth-open-mismatch".to_string(),
            workdir.clone(),
            env,
            None,
            event_tx,
            crate::adapters::ProviderRunHooks::default(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("resume mismatch"), "{err:#}");

    let started = Instant::now();
    while adapter.pool.session_count_for_test().await != 0 {
        if started.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for rejected auth-open session to leave the pool");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!adapter.has_live_session("auth-open-mismatch").await);

    let stdin_log = fs::read_to_string(&log_path)?;
    assert!(
        !stdin_log.contains(r#""type":"session.authenticate""#),
        "authenticate must not be sent after open mismatch: {stdin_log}"
    );
    Ok(())
}

#[test]
fn auth_required_stderr_notice_payload_is_redacted() {
    let payload =
        auth_required_notice_payload_from_stderr("https://auth.example.test/start?token=secret");

    assert_eq!(payload.get("kind"), Some(&json!("auth_required")));
    assert_eq!(payload.get("code"), Some(&json!("auth_required")));
    assert_eq!(
        payload.get("message"),
        Some(&json!("Authentication required."))
    );
    assert_eq!(payload.get("source"), Some(&json!("crp_stderr")));
    assert_eq!(payload.get("auth_url"), None);
}
