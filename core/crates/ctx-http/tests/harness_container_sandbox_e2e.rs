use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::json;
use tokio::process::Command;

use ctx_providers::crp::Tier1CrpAdapter;

use ctx_managed_installs::{save_agent_server_config, AgentServerCommand, AgentServerConfigFile};
use ctx_settings_model::{
    ContainerExecutionSettings, ContainerMountMode, ContainerNetworkMode, ExecutionMode,
    ExecutionSettings, Settings,
};

mod common;

fn sandbox_cli_binary_for_tests() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("CTX_HARNESS_SANDBOX_CLI_PATH") {
        let path = PathBuf::from(raw);
        if path.exists() {
            return Some(path);
        }
    }
    which::which("nerdctl").ok()
}

async fn setup_git_repo() -> tempfile::TempDir {
    common::init_git_repo(&[("note.txt", "hello\n")]).await
}

fn write_fake_crp_script(root: &Path) -> PathBuf {
    let script_dir = root
        .join("providers")
        .join("agent-servers")
        .join("codex")
        .join("fake");
    std::fs::create_dir_all(&script_dir).unwrap();
    let script_path = script_dir.join("fake_crp.py");
    let script = r#"
import json
import sys

next_session = 1
seq = 1

def send(msg):
    global seq
    msg["seq"] = seq
    seq += 1
    msg.setdefault("channel", "control")
    sys.stdout.write(json.dumps(msg))
    sys.stdout.write("\n")
    sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except Exception:
        continue
    command_type = msg.get("type")
    if command_type == "session.open":
        session_id = msg.get("session_id")
        if not session_id:
            session_id = "sess_%d" % next_session
            next_session += 1
        send({
            "type": "session.opened",
            "session_id": session_id,
            "provider_session_id": session_id,
        })
    elif command_type == "session.prompt":
        session_id = msg.get("session_id") or "sess_1"
        turn_id = msg.get("turn_id") or "turn_1"
        send({
            "type": "turn.started",
            "session_id": session_id,
            "turn_id": turn_id,
        })
        send({
            "type": "message.final",
            "session_id": session_id,
            "turn_id": turn_id,
            "message_id": "msg_1",
            "content": "done",
        })
        send({
            "type": "turn.completed",
            "session_id": session_id,
            "turn_id": turn_id,
            "status": "success",
        })
    elif command_type == "models.list":
        send({
            "type": "models.list",
            "models": [{"id": "fake-model"}],
            "current_model_id": "fake-model",
        })
"#;
    std::fs::write(&script_path, script.trim_start()).unwrap();
    script_path
}

async fn configure_fake_provider(data_root: &Path, script_path: &Path) {
    let mut cfg = AgentServerConfigFile::default();
    cfg.providers.insert(
        "codex".to_string(),
        AgentServerCommand {
            command: "python3".to_string(),
            args: vec![script_path.to_string_lossy().to_string()],
            dependencies: Vec::new(),
            managed: None,
        },
    );
    save_agent_server_config(data_root, &cfg).await.unwrap();
}

async fn configure_container_settings(
    data_root: &Path,
    mount_mode: ContainerMountMode,
    image: &str,
) -> ExecutionSettings {
    configure_container_network_settings(
        data_root,
        mount_mode,
        image,
        ContainerNetworkMode::All,
        Vec::new(),
    )
    .await
}

async fn configure_container_network_settings(
    data_root: &Path,
    mount_mode: ContainerMountMode,
    image: &str,
    network_mode: ContainerNetworkMode,
    allowlist: Vec<String>,
) -> ExecutionSettings {
    let execution = ExecutionSettings {
        mode: ExecutionMode::Sandbox,
        container: ContainerExecutionSettings {
            mount_mode,
            network_mode,
            allowlist,
            image: Some(image.to_string()),
            ..Default::default()
        },
    };
    let settings = Settings {
        execution: Some(execution.clone()),
        ..Default::default()
    };
    ctx_daemon::test_support::TestDaemon::preseed_settings_for_data_root_for_test(
        data_root, &settings,
    )
    .await
    .unwrap();
    execution
}

fn fake_crp_providers(
    script_path: &Path,
) -> HashMap<String, Arc<dyn ctx_providers::adapters::ProviderAdapter>> {
    let mut providers: HashMap<String, Arc<dyn ctx_providers::adapters::ProviderAdapter>> =
        HashMap::new();
    providers.insert(
        "codex".into(),
        Arc::new(Tier1CrpAdapter::from_raw(
            "codex",
            "python3".to_string(),
            vec![script_path.to_string_lossy().to_string()],
        )),
    );
    providers
}

async fn run_container_python(container_name: &str, script: &str) -> std::process::Output {
    let sandbox_cli = sandbox_cli_binary_for_tests().expect("sandbox CLI required for e2e");
    Command::new(sandbox_cli)
        .arg("exec")
        .arg(container_name)
        .arg("python3")
        .arg("-c")
        .arg(script)
        .output()
        .await
        .unwrap()
}

async fn create_session_with_provider(
    app: &axum::Router,
    git_repo_root: &Path,
    provider_id: &str,
) -> ctx_core::models::Session {
    let workspace = common::create_workspace(app, git_repo_root, "ws").await;
    let (_task, session) =
        common::create_task_with_session(app, workspace.id.0, "t1", provider_id, "fake-model")
            .await;
    session
}

async fn post_message(app: &axum::Router, session_id: &str, content: &str) {
    let (status, _message): (StatusCode, serde_json::Value) = common::json_request(
        app,
        Method::POST,
        format!("/api/sessions/{session_id}/messages"),
        Some(json!({ "content": content })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

async fn ensure_workspace_harness_container(
    app: &axum::Router,
    workspace_id: ctx_core::ids::WorkspaceId,
) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/api/workspaces/{}/harness_container/ensure",
            workspace_id.0
        ))
        .body(Body::empty())
        .unwrap();
    let (status, body) = common::oneshot_bytes(app, req).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_empty(), "ensure response should be empty");
}

async fn workspace_harness_egress_guard(
    app: &axum::Router,
    workspace_id: ctx_core::ids::WorkspaceId,
) -> Option<bool> {
    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        app,
        Method::GET,
        format!("/api/workspaces/{}/harness_container", workspace_id.0),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body.get("egress_guard")
        .and_then(serde_json::Value::as_bool)
}

async fn wait_for_done(
    daemon: &ctx_daemon::test_support::TestDaemon,
    session_id: ctx_core::ids::SessionId,
) {
    daemon
        .wait_for_session_done_event_count_for_test(session_id, 1, Duration::from_secs(60))
        .await
        .expect("timed out waiting for Done event");
}

const PROMPT: &str = "Reply with the exact text: done";

#[tokio::test]
#[ignore]
async fn harness_container_sandbox_fake_acp() {
    let _sandbox_env_lock = ctx_daemon::test_support::sandbox_cli_env_test_lock()
        .lock()
        .await;
    if std::env::var("CTX_E2E_SANDBOX").ok().as_deref() != Some("1") {
        eprintln!("skipping: CTX_E2E_SANDBOX not set");
        return;
    }
    if sandbox_cli_binary_for_tests().is_none() {
        eprintln!("skipping: sandbox CLI not found");
        return;
    }
    let sandbox_cli = sandbox_cli_binary_for_tests().expect("sandbox CLI not found");
    let _guard = common::TestEnvGuard::set("CTX_HARNESS_SANDBOX_CLI_PATH", sandbox_cli.as_os_str());

    let git_repo = setup_git_repo().await;
    let data_dir = tempfile::tempdir().unwrap();

    let script_path = write_fake_crp_script(data_dir.path());
    configure_fake_provider(data_dir.path(), &script_path).await;
    let _execution_settings = configure_container_settings(
        data_dir.path(),
        ContainerMountMode::DiskIsolated,
        "python:3.11",
    )
    .await;

    let providers = fake_crp_providers(&script_path);
    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        providers,
        "http://127.0.0.1:4399",
    )
    .await;
    let app = fixture.router();
    let session = create_session_with_provider(&app, git_repo.path(), "codex").await;

    let session_id = session.id.0.to_string();
    post_message(&app, &session_id, PROMPT).await;
    wait_for_done(&fixture.daemon, session.id).await;
}

#[tokio::test]
#[ignore]
async fn harness_container_sandbox_egress_allowlist() {
    let _ = tracing_subscriber::fmt::try_init();
    let _sandbox_env_lock = ctx_daemon::test_support::sandbox_cli_env_test_lock()
        .lock()
        .await;
    if std::env::var("CTX_E2E_SANDBOX").ok().as_deref() != Some("1") {
        eprintln!("skipping: CTX_E2E_SANDBOX not set");
        return;
    }
    if sandbox_cli_binary_for_tests().is_none() {
        eprintln!("skipping: sandbox CLI not found");
        return;
    }
    if std::env::var("CTX_EGRESS_PROXY_PATH").ok().is_none() {
        eprintln!("skipping: CTX_EGRESS_PROXY_PATH not set");
        return;
    }
    let sandbox_cli = sandbox_cli_binary_for_tests().expect("sandbox CLI not found");
    let _guard = common::TestEnvGuard::set("CTX_HARNESS_SANDBOX_CLI_PATH", sandbox_cli.as_os_str());

    let image =
        std::env::var("CTX_E2E_SANDBOX_IMAGE").unwrap_or_else(|_| "python:3.11".to_string());
    let allow_host = "example.com";

    let git_repo = setup_git_repo().await;
    let data_dir = tempfile::tempdir().unwrap();

    let script_path = write_fake_crp_script(data_dir.path());
    configure_fake_provider(data_dir.path(), &script_path).await;
    let execution_settings = configure_container_network_settings(
        data_dir.path(),
        ContainerMountMode::DiskIsolated,
        &image,
        ContainerNetworkMode::Allowlist,
        vec![allow_host.to_string()],
    )
    .await;
    assert_eq!(
        execution_settings.container.network_mode,
        ContainerNetworkMode::Allowlist
    );

    let providers = fake_crp_providers(&script_path);
    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        providers,
        "http://127.0.0.1:4399",
    )
    .await;
    let app = fixture.router();
    let session = create_session_with_provider(&app, git_repo.path(), "codex").await;
    ensure_workspace_harness_container(&app, session.workspace_id).await;

    let session_id = session.id.0.to_string();
    post_message(&app, &session_id, PROMPT).await;
    wait_for_done(&fixture.daemon, session.id).await;
    let egress_guard = workspace_harness_egress_guard(&app, session.workspace_id).await;
    assert!(
        egress_guard.unwrap_or(false),
        "egress guard was not configured"
    );

    let container_name = format!("ctx-harness-{}", session.workspace_id.0);
    let allow_script = r#"
import socket, ssl
host = "example.com"
ctx = ssl.create_default_context()
sock = ctx.wrap_socket(socket.socket(), server_hostname=host)
sock.settimeout(5)
sock.connect((host, 443))
sock.sendall(b"GET / HTTP/1.1\r\nHost: " + host.encode() + b"\r\nConnection: close\r\n\r\n")
sock.recv(4)
"#;
    let allow_output = run_container_python(&container_name, allow_script).await;
    assert!(
        allow_output.status.success(),
        "allowlist host failed: {}",
        String::from_utf8_lossy(&allow_output.stderr)
    );

    let deny_script = r#"
import socket, ssl, sys
host = "example.net"
ctx = ssl.create_default_context()
sock = ctx.wrap_socket(socket.socket(), server_hostname=host)
sock.settimeout(5)
try:
    sock.connect((host, 443))
    sock.sendall(b"GET / HTTP/1.1\r\nHost: " + host.encode() + b"\r\nConnection: close\r\n\r\n")
    sock.recv(4)
    sys.exit(0)
except Exception:
    sys.exit(2)
"#;
    let deny_output = run_container_python(&container_name, deny_script).await;
    assert_eq!(deny_output.status.code(), Some(2));
}

#[tokio::test]
#[ignore]
async fn harness_container_sandbox_egress_allow_all() {
    let _ = tracing_subscriber::fmt::try_init();
    let _sandbox_env_lock = ctx_daemon::test_support::sandbox_cli_env_test_lock()
        .lock()
        .await;
    if std::env::var("CTX_E2E_SANDBOX").ok().as_deref() != Some("1") {
        eprintln!("skipping: CTX_E2E_SANDBOX not set");
        return;
    }
    if sandbox_cli_binary_for_tests().is_none() {
        eprintln!("skipping: sandbox CLI not found");
        return;
    }
    if std::env::var("CTX_EGRESS_PROXY_PATH").ok().is_none() {
        eprintln!("skipping: CTX_EGRESS_PROXY_PATH not set");
        return;
    }
    let sandbox_cli = sandbox_cli_binary_for_tests().expect("sandbox CLI not found");
    let _guard = common::TestEnvGuard::set("CTX_HARNESS_SANDBOX_CLI_PATH", sandbox_cli.as_os_str());

    let image =
        std::env::var("CTX_E2E_SANDBOX_IMAGE").unwrap_or_else(|_| "python:3.11".to_string());

    let git_repo = setup_git_repo().await;
    let data_dir = tempfile::tempdir().unwrap();

    let script_path = write_fake_crp_script(data_dir.path());
    configure_fake_provider(data_dir.path(), &script_path).await;
    let _execution_settings = configure_container_network_settings(
        data_dir.path(),
        ContainerMountMode::DiskIsolated,
        &image,
        ContainerNetworkMode::All,
        Vec::new(),
    )
    .await;

    let providers = fake_crp_providers(&script_path);
    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        providers,
        "http://127.0.0.1:4399",
    )
    .await;
    let app = fixture.router();
    let session = create_session_with_provider(&app, git_repo.path(), "codex").await;
    ensure_workspace_harness_container(&app, session.workspace_id).await;

    let session_id = session.id.0.to_string();
    post_message(&app, &session_id, PROMPT).await;
    wait_for_done(&fixture.daemon, session.id).await;
    let egress_guard = workspace_harness_egress_guard(&app, session.workspace_id).await;
    assert_eq!(
        egress_guard,
        Some(false),
        "egress guard should be disabled for allow-all"
    );

    let container_name = format!("ctx-harness-{}", session.workspace_id.0);
    for host in ["example.com", "example.net"] {
        let allow_script = format!(
            r#"
import socket, ssl
host = "{host}"
ctx = ssl.create_default_context()
sock = ctx.wrap_socket(socket.socket(), server_hostname=host)
sock.settimeout(5)
sock.connect((host, 443))
sock.sendall(b"GET / HTTP/1.1\r\nHost: " + host.encode() + b"\r\nConnection: close\r\n\r\n")
sock.recv(4)
"#
        );
        let allow_output = run_container_python(&container_name, &allow_script).await;
        assert!(
            allow_output.status.success(),
            "allow-all host failed ({host}): {}",
            String::from_utf8_lossy(&allow_output.stderr)
        );
    }
}

#[tokio::test]
#[ignore]
async fn harness_container_sandbox_egress_deny_all() {
    let _ = tracing_subscriber::fmt::try_init();
    let _sandbox_env_lock = ctx_daemon::test_support::sandbox_cli_env_test_lock()
        .lock()
        .await;
    if std::env::var("CTX_E2E_SANDBOX").ok().as_deref() != Some("1") {
        eprintln!("skipping: CTX_E2E_SANDBOX not set");
        return;
    }
    if sandbox_cli_binary_for_tests().is_none() {
        eprintln!("skipping: sandbox CLI not found");
        return;
    }
    if std::env::var("CTX_EGRESS_PROXY_PATH").ok().is_none() {
        eprintln!("skipping: CTX_EGRESS_PROXY_PATH not set");
        return;
    }
    let sandbox_cli = sandbox_cli_binary_for_tests().expect("sandbox CLI not found");
    let _guard = common::TestEnvGuard::set("CTX_HARNESS_SANDBOX_CLI_PATH", sandbox_cli.as_os_str());

    let image =
        std::env::var("CTX_E2E_SANDBOX_IMAGE").unwrap_or_else(|_| "python:3.11".to_string());

    let git_repo = setup_git_repo().await;
    let data_dir = tempfile::tempdir().unwrap();

    let script_path = write_fake_crp_script(data_dir.path());
    configure_fake_provider(data_dir.path(), &script_path).await;
    let _execution_settings = configure_container_network_settings(
        data_dir.path(),
        ContainerMountMode::DiskIsolated,
        &image,
        ContainerNetworkMode::Allowlist,
        Vec::new(),
    )
    .await;

    let providers = fake_crp_providers(&script_path);
    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        providers,
        "http://127.0.0.1:4399",
    )
    .await;
    let app = fixture.router();
    let session = create_session_with_provider(&app, git_repo.path(), "codex").await;
    ensure_workspace_harness_container(&app, session.workspace_id).await;

    let session_id = session.id.0.to_string();
    post_message(&app, &session_id, PROMPT).await;
    wait_for_done(&fixture.daemon, session.id).await;
    let egress_guard = workspace_harness_egress_guard(&app, session.workspace_id).await;
    assert!(
        egress_guard.unwrap_or(false),
        "egress guard was not configured"
    );

    let container_name = format!("ctx-harness-{}", session.workspace_id.0);
    let deny_script = r#"
import socket, ssl, sys
host = "example.com"
ctx = ssl.create_default_context()
sock = ctx.wrap_socket(socket.socket(), server_hostname=host)
sock.settimeout(5)
try:
    sock.connect((host, 443))
    sock.sendall(b"GET / HTTP/1.1\r\nHost: " + host.encode() + b"\r\nConnection: close\r\n\r\n")
    sock.recv(4)
    sys.exit(0)
except Exception:
    sys.exit(2)
"#;
    let deny_output = run_container_python(&container_name, deny_script).await;
    assert_eq!(deny_output.status.code(), Some(2));
}
