use std::path::Path;
use std::{env, fs};

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::process::Command;

mod common;

struct EnvVarGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let prev = env::var_os(key);
        env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.as_ref() {
            env::set_var(self.key, prev);
        } else {
            env::remove_var(self.key);
        }
    }
}

#[derive(Debug, Deserialize)]
struct TaskWithSession {
    id: String,
    primary_session_id: Option<String>,
}

async fn run_git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn setup_git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    run_git(dir.path(), &["init"]).await;
    run_git(dir.path(), &["config", "user.email", "test@example.com"]).await;
    run_git(dir.path(), &["config", "user.name", "Test"]).await;
    fs::write(dir.path().join("file.txt"), "hello\n").unwrap();
    run_git(dir.path(), &["add", "."]).await;
    run_git(dir.path(), &["commit", "-m", "init"]).await;
    dir
}

fn should_run() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    matches!(
        env::var("CTX_E2E_SANDBOX").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

fn sandbox_cli_binary_for_tests() -> Option<std::path::PathBuf> {
    if let Some(raw) = env::var_os("CTX_HARNESS_SANDBOX_CLI_PATH") {
        let path = std::path::PathBuf::from(raw);
        if path.exists() {
            return Some(path);
        }
    }
    which::which("nerdctl").ok()
}

fn sandbox_cli_env_for_data_root(data_root: &Path) -> Vec<(String, String)> {
    let sandbox_root = data_root.join("sandbox");
    let xdg_root = sandbox_root.join("xdg");
    vec![
        (
            "XDG_CONFIG_HOME".to_string(),
            xdg_root.join("config").to_string_lossy().to_string(),
        ),
        (
            "XDG_DATA_HOME".to_string(),
            xdg_root.join("data").to_string_lossy().to_string(),
        ),
        (
            "XDG_RUNTIME_DIR".to_string(),
            sandbox_root.join("run").to_string_lossy().to_string(),
        ),
        (
            "HOME".to_string(),
            sandbox_root.join("home").to_string_lossy().to_string(),
        ),
        (
            "TMPDIR".to_string(),
            sandbox_root.join("tmp").to_string_lossy().to_string(),
        ),
        (
            "TMP".to_string(),
            sandbox_root.join("tmp").to_string_lossy().to_string(),
        ),
        (
            "TEMP".to_string(),
            sandbox_root.join("tmp").to_string_lossy().to_string(),
        ),
    ]
}

#[tokio::test]
async fn disk_isolated_task_creation_produces_valid_git_worktree() {
    if !should_run() {
        return;
    }
    if env::var("CTX_BUNDLE_DIR").ok().is_none() {
        return;
    }

    let Some(sandbox_cli) = sandbox_cli_binary_for_tests() else {
        return;
    };
    let _sandbox_cli_path =
        EnvVarGuard::set("CTX_HARNESS_SANDBOX_CLI_PATH", sandbox_cli.as_os_str());
    if Command::new(&sandbox_cli)
        .arg("version")
        .output()
        .await
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        return;
    }

    let repo = setup_git_repo().await;
    let fixture = common::fake_daemon_fixture_with_providers(
        common::fake_providers(),
        "http://127.0.0.1:4399",
    )
    .await;
    let app = fixture.router();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let ws: ctx_core::models::Workspace = client
        .post(format!("{base}/api/workspaces"))
        .json(&json!({
            "root_path": repo.path().to_string_lossy(),
            "name": "ws"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let cfg_resp = client
        .post(format!(
            "{base}/api/workspaces/{}/execution_config",
            ws.id.0
        ))
        .json(&json!({
            "enabled": true,
            "environment": "sandbox",
            "mount_mode": "disk_isolated"
        }))
        .send()
        .await
        .unwrap();
    assert!(
        cfg_resp.status().is_success(),
        "failed to configure execution: {}",
        cfg_resp.text().await.unwrap_or_default()
    );

    let ensure_resp = client
        .post(format!(
            "{base}/api/workspaces/{}/harness_container/ensure",
            ws.id.0
        ))
        .send()
        .await
        .unwrap();
    assert!(
        ensure_resp.status().is_success(),
        "failed to ensure container: {}",
        ensure_resp.text().await.unwrap_or_default()
    );

    let task_resp = client
        .post(format!("{base}/api/workspaces/{}/tasks", ws.id.0))
        .json(&json!({ "title": "disk-isolated-vcs" }))
        .send()
        .await
        .unwrap();
    assert!(
        task_resp.status().is_success(),
        "task creation failed: {}",
        task_resp.text().await.unwrap_or_default()
    );
    let task: TaskWithSession = task_resp.json().await.unwrap();
    let session_id = task
        .primary_session_id
        .expect("task should have a primary session");

    let session: ctx_core::models::Session = client
        .get(format!("{base}/api/tasks/{}/sessions", task.id))
        .send()
        .await
        .unwrap()
        .json::<Vec<ctx_core::models::Session>>()
        .await
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.id.0.to_string() == session_id)
        .expect("primary session should exist");
    let worktree: ctx_core::models::Worktree = client
        .get(format!("{base}/api/worktrees/{}", session.worktree_id.0))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let container_id = format!("ctx-harness-{}", ws.id.0);
    let mut cmd = Command::new(&sandbox_cli);
    cmd.arg("exec")
        .arg("--workdir")
        .arg(&worktree.root_path)
        .arg(&container_id)
        .arg("sh")
        .arg("-lc")
        .arg("git rev-parse --is-inside-work-tree && git rev-parse HEAD >/dev/null");
    for (k, v) in sandbox_cli_env_for_data_root(fixture.data_dir.path()) {
        cmd.env(k, v);
    }
    let output = cmd.output().await.unwrap();
    assert!(
        output.status.success(),
        "disk-isolated worktree should be a valid git repo: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.trim() == "true"),
        "expected git rev-parse --is-inside-work-tree to return true"
    );

    let mut mutate = Command::new(&sandbox_cli);
    mutate
        .arg("exec")
        .arg("--workdir")
        .arg(&worktree.root_path)
        .arg(&container_id)
        .arg("sh")
        .arg("-lc")
        .arg("printf 'hello from container\\n' > file.txt");
    for (k, v) in sandbox_cli_env_for_data_root(fixture.data_dir.path()) {
        mutate.env(k, v);
    }
    let mutate_out = mutate.output().await.unwrap();
    assert!(
        mutate_out.status.success(),
        "failed to mutate disk-isolated worktree in container: {}",
        String::from_utf8_lossy(&mutate_out.stderr)
    );

    let diff_resp = client
        .get(format!("{base}/api/sessions/{session_id}/diff"))
        .send()
        .await
        .unwrap();
    let diff_status = diff_resp.status();
    let diff_text = diff_resp.text().await.unwrap_or_default();
    assert!(
        diff_status.is_success(),
        "session diff endpoint failed: {diff_text}"
    );
    let diff_json: Value = serde_json::from_str(&diff_text).unwrap();
    assert_eq!(
        diff_json.get("available").and_then(Value::as_bool),
        Some(true)
    );
    assert!(diff_json
        .get("unavailable_reason")
        .and_then(Value::as_str)
        .is_none());
    assert!(
        diff_json
            .get("diff")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("hello from container"),
        "expected diff payload to include in-container edit, got: {diff_text}"
    );

    let summary_resp = client
        .get(format!("{base}/api/sessions/{session_id}/diff/summary"))
        .send()
        .await
        .unwrap();
    let summary_status = summary_resp.status();
    let summary_text = summary_resp.text().await.unwrap_or_default();
    assert!(
        summary_status.is_success(),
        "session diff summary endpoint failed: {summary_text}"
    );
    let summary_json: Value = serde_json::from_str(&summary_text).unwrap();
    assert_eq!(
        summary_json.get("available").and_then(Value::as_bool),
        Some(true)
    );
    assert!(summary_json
        .get("unavailable_reason")
        .and_then(Value::as_str)
        .is_none());
    assert!(
        summary_json
            .get("file_count")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            >= 1
    );
}
