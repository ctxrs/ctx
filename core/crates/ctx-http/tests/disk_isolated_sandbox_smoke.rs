use std::path::Path;
use std::time::Duration;
use std::{env, fs};

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;

const CONTAINER_FILE_SHA256: &str =
    "dc155555ce7bf6f6b7aa998bafe7e1cafa3c7017bc5dcdeb8ef72ebc5961c11a";
const TERM_WRITE_SHA256: &str = "21ce56d2f98a9ed161e56e42a704fb47cea917ffe91bee9d75405349fdc4ee68";

struct EnvVarGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

mod common;

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
struct SessionGitStatusResponse {
    #[serde(default)]
    entries: Vec<SessionGitStatusEntry>,
}

#[derive(Debug, Deserialize)]
struct SessionGitStatusEntry {
    path: String,
}

#[derive(Debug, Deserialize)]
struct WorkspaceAttachmentResp {
    name: String,
    status: String,
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
    let root = dir.path();
    run_git(root, &["init"]).await;
    run_git(root, &["config", "user.email", "test@example.com"]).await;
    run_git(root, &["config", "user.name", "Test"]).await;
    fs::write(root.join("file.txt"), "hello\n").unwrap();
    run_git(root, &["add", "."]).await;
    run_git(root, &["commit", "-m", "init"]).await;
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

async fn sandbox_volume_exists(data_root: &Path, name: &str) -> bool {
    let Some(sandbox_cli) = sandbox_cli_binary_for_tests() else {
        return false;
    };
    let mut cmd = Command::new(sandbox_cli);
    cmd.arg("volume").arg("inspect").arg(name);
    for (k, v) in sandbox_cli_env_for_data_root(data_root) {
        cmd.env(k, v);
    }
    match cmd.output().await {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

async fn wait_for_terminal_output(
    ws_stream: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    needle: &str,
    timeout: Duration,
) -> bool {
    tokio::time::timeout(timeout, async {
        while let Some(Ok(frame)) = ws_stream.next().await {
            if let tokio_tungstenite::tungstenite::Message::Binary(bytes) = frame {
                let txt = String::from_utf8_lossy(&bytes);
                if txt.contains(needle) {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}

#[tokio::test]
async fn disk_isolated_smoke_sandbox_volume_attachments_and_terminal() {
    if !should_run() {
        return;
    }
    if env::var("CTX_BUNDLE_DIR").ok().is_none() {
        // The daemon requires a bundled harness image tar for the default image when pulls are
        // disabled (release behavior). Running this smoke without a bundle isn't meaningful.
        return;
    }

    let Some(sandbox_cli) = sandbox_cli_binary_for_tests() else {
        return;
    };
    let _sandbox_cli_path =
        EnvVarGuard::set("CTX_HARNESS_SANDBOX_CLI_PATH", sandbox_cli.as_os_str());

    // Ensure the sandbox CLI is present before we spend time bootstrapping.
    if Command::new(&sandbox_cli)
        .arg("version")
        .output()
        .await
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        return;
    }

    let git_repo = setup_git_repo().await;
    let host_root = git_repo.path().to_path_buf();
    let host_file_path = host_root.join("file.txt");
    let host_text_before = fs::read_to_string(&host_file_path).unwrap();

    let ref_repo = setup_git_repo().await;
    fs::write(ref_repo.path().join("ref.txt"), "refdata\n").unwrap();
    run_git(ref_repo.path(), &["add", "."]).await;
    run_git(ref_repo.path(), &["commit", "-m", "ref"]).await;

    let home = tempfile::tempdir().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", home.path());

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

    // Create workspace.
    let ws: ctx_core::models::Workspace = client
        .post(format!("{base}/api/workspaces"))
        .json(&json!({
            "root_path": host_root.to_string_lossy(),
            "name": "ws"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // Configure disk-isolated container execution for the workspace.
    let _cfg: serde_json::Value = client
        .post(format!(
            "{base}/api/workspaces/{}/execution_config",
            ws.id.0
        ))
        .json(&json!({
            "environment": "sandbox",
            "network_mode": "all",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // Ensure harness container exists early (also exercises mount verification).
    let _container_status: serde_json::Value = client
        .post(format!(
            "{base}/api/workspaces/{}/harness_container/ensure",
            ws.id.0
        ))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // Create task (creates default worktree + session).
    let task: ctx_core::models::Task = client
        .post(format!("{base}/api/workspaces/{}/tasks", ws.id.0))
        .json(&json!({"title":"t1"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_id = task
        .primary_session_id
        .expect("default session not created");
    let worktree_id = task
        .primary_worktree_id
        .expect("default worktree not created");

    // Verify worktree is container-rooted.
    let worktree: ctx_core::models::Worktree = client
        .get(format!("{base}/api/worktrees/{}", worktree_id.0))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        worktree.root_path.starts_with("/ctx/ws/worktrees/"),
        "expected container worktree root, got {}",
        worktree.root_path
    );

    let term: ctx_core::models::TerminalSession = client
        .post(format!("{base}/api/workspaces/{}/terminals", ws.id.0))
        .json(&json!({
            "worktree_id": worktree_id.0,
            "shell": "/bin/bash",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let stream_path = client
        .post(format!("{base}/api/terminals/{}/stream_token", term.id.0))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["stream_path"]
        .as_str()
        .unwrap()
        .to_string();
    let ws_url = format!("http://{}{stream_path}", addr)
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let (mut ws_stream, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();

    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "cat file.txt\n".into(),
        ))
        .await
        .unwrap();
    assert!(
        wait_for_terminal_output(&mut ws_stream, "hello", Duration::from_secs(15)).await,
        "terminal output did not include initial file contents"
    );

    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "printf 'hello from container\\n' > file.txt\nsha256sum file.txt\n".into(),
        ))
        .await
        .unwrap();
    assert!(
        wait_for_terminal_output(
            &mut ws_stream,
            CONTAINER_FILE_SHA256,
            Duration::from_secs(15),
        )
        .await,
        "terminal output did not include rewritten file hash"
    );

    // Host FS remains unchanged.
    let host_text_after = fs::read_to_string(&host_file_path).unwrap();
    assert_eq!(host_text_after, host_text_before);
    // Attachments: daemon fetches via host git and imports into the disk-isolated volume.
    let _attachments: serde_json::Value = client
        .post(format!("{base}/api/workspaces/{}/attachments", ws.id.0))
        .json(&json!({
            "kind": "reference_repo",
            "name": "ref1",
            "source": ref_repo.path().to_string_lossy(),
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let attachment_ready = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let listed: Vec<WorkspaceAttachmentResp> = client
                .get(format!("{base}/api/workspaces/{}/attachments", ws.id.0))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if listed
                .iter()
                .any(|a| a.name == "ref1" && a.status == "ready")
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(attachment_ready, "attachment did not become ready");
    let _attachment_delete_target: serde_json::Value = client
        .post(format!("{base}/api/workspaces/{}/attachments", ws.id.0))
        .json(&json!({
            "kind": "reference_repo",
            "name": "ref2",
            "source": ref_repo.path().to_string_lossy(),
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let attachment_delete_target_ready = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let listed: Vec<WorkspaceAttachmentResp> = client
                .get(format!("{base}/api/workspaces/{}/attachments", ws.id.0))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if listed
                .iter()
                .any(|a| a.name == "ref2" && a.status == "ready")
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        attachment_delete_target_ready,
        "attachment delete target did not become ready"
    );
    assert!(!host_root
        .join(".ctx/attachments/refs/ref1/ref.txt")
        .exists());

    // Terminal should run inside the container worktree.
    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "pwd\n".into(),
        ))
        .await
        .unwrap();

    let expected_prefix = "/ctx/ws/worktrees/";
    let saw_pwd =
        wait_for_terminal_output(&mut ws_stream, expected_prefix, Duration::from_secs(15)).await;
    assert!(saw_pwd, "terminal output did not include {expected_prefix}");

    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "sha256sum file.txt\ncat .ctx/attachments/refs/ref1/ref.txt\n".into(),
        ))
        .await
        .unwrap();

    let saw_container_file_hash_and_attachment =
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut saw_file_hash = false;
            let mut saw_attachment = false;
            while let Some(Ok(frame)) = ws_stream.next().await {
                if let tokio_tungstenite::tungstenite::Message::Binary(bytes) = frame {
                    let txt = String::from_utf8_lossy(&bytes);
                    if txt.contains(CONTAINER_FILE_SHA256) {
                        saw_file_hash = true;
                    }
                    if txt.contains("refdata") {
                        saw_attachment = true;
                    }
                    if saw_file_hash && saw_attachment {
                        return true;
                    }
                }
            }
            false
        })
        .await
        .unwrap_or(false);
    assert!(
        saw_container_file_hash_and_attachment,
        "terminal output did not include container file hash and attachment contents"
    );

    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "if printf 'mutated\\n' > .ctx/attachments/refs/ref1/ref.txt; then echo ATTACHMENT_WRITE_SUCCEEDED; else echo ATTACHMENT_WRITE_BLOCKED; fi\ncat .ctx/attachments/refs/ref1/ref.txt\n".into(),
        ))
        .await
        .unwrap();

    let saw_attachment_write_blocked = tokio::time::timeout(Duration::from_secs(15), async {
        let mut saw_blocked = false;
        let mut saw_refdata = false;
        while let Some(Ok(frame)) = ws_stream.next().await {
            if let tokio_tungstenite::tungstenite::Message::Binary(bytes) = frame {
                let txt = String::from_utf8_lossy(&bytes);
                if txt.contains("ATTACHMENT_WRITE_BLOCKED") {
                    saw_blocked = true;
                }
                if txt.contains("refdata") {
                    saw_refdata = true;
                }
                if txt.contains("ATTACHMENT_WRITE_SUCCEEDED") {
                    return false;
                }
                if saw_blocked && saw_refdata {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(
        saw_attachment_write_blocked,
        "sandbox attachment mount allowed a write or hid the original contents"
    );
    let _deleted_attachment: serde_json::Value = client
        .delete(format!("{base}/api/workspaces/{}/attachments", ws.id.0))
        .json(&json!({
            "kind": "reference_repo",
            "name": "ref2",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "if [ -e .ctx/attachments/refs/ref2/ref.txt ]; then echo ATTACHMENT_DELETE_LEFT_STALE; else echo ATTACHMENT_DELETE_CLEAN; fi\n".into(),
        ))
        .await
        .unwrap();
    let saw_attachment_delete_clean = wait_for_terminal_output(
        &mut ws_stream,
        "ATTACHMENT_DELETE_CLEAN",
        Duration::from_secs(15),
    )
    .await;
    assert!(
        saw_attachment_delete_clean,
        "deleting a read-only sandbox attachment left a stale mount behind"
    );

    let _updated_attachment: serde_json::Value = client
        .post(format!("{base}/api/workspaces/{}/attachments", ws.id.0))
        .json(&json!({
            "kind": "reference_repo",
            "name": "ref1",
            "source": ref_repo.path().to_string_lossy(),
            "mode": "rw",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let _synced_attachments: serde_json::Value = client
        .post(format!(
            "{base}/api/workspaces/{}/attachments/sync",
            ws.id.0
        ))
        .json(&json!({ "refresh": true }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "if printf 'rw-mutated\\n' > .ctx/attachments/refs/ref1/ref.txt; then echo ATTACHMENT_RW_WRITE_SUCCEEDED; else echo ATTACHMENT_RW_WRITE_BLOCKED; fi\ncat .ctx/attachments/refs/ref1/ref.txt\n".into(),
        ))
        .await
        .unwrap();

    let saw_attachment_rw_write = tokio::time::timeout(Duration::from_secs(15), async {
        let mut saw_success = false;
        let mut saw_mutated = false;
        while let Some(Ok(frame)) = ws_stream.next().await {
            if let tokio_tungstenite::tungstenite::Message::Binary(bytes) = frame {
                let txt = String::from_utf8_lossy(&bytes);
                if txt.contains("ATTACHMENT_RW_WRITE_SUCCEEDED") {
                    saw_success = true;
                }
                if txt.contains("rw-mutated") {
                    saw_mutated = true;
                }
                if txt.contains("ATTACHMENT_RW_WRITE_BLOCKED") {
                    return false;
                }
                if saw_success && saw_mutated {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(
        saw_attachment_rw_write,
        "sandbox attachment mount did not remount writable after mode update"
    );

    let host_text_after = fs::read_to_string(&host_file_path).unwrap();
    assert_eq!(host_text_after, host_text_before);
    let host_attachment_after = fs::read_to_string(ref_repo.path().join("ref.txt")).unwrap();
    assert_eq!(host_attachment_after, "refdata\n");

    // Terminal write should affect container FS, and git status should reflect it.
    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "printf 'term wrote\\n' > term_write.txt\nsha256sum term_write.txt\n".into(),
        ))
        .await
        .unwrap();

    let saw_term_write =
        wait_for_terminal_output(&mut ws_stream, TERM_WRITE_SHA256, Duration::from_secs(15)).await;
    assert!(
        saw_term_write,
        "terminal output did not include expected file hash"
    );

    let status: SessionGitStatusResponse = client
        .get(format!("{base}/api/sessions/{}/git/status", session_id.0))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        status.entries.iter().any(|e| e.path == "term_write.txt"),
        "expected git status to include term_write.txt"
    );

    assert!(!host_root.join("term_write.txt").exists());

    // Deleting the workspace should clean up the disk-isolated volume.
    let vol_name = format!("ctx-ws-{}", ws.id.0);
    assert!(sandbox_volume_exists(fixture.data_dir.path(), &vol_name).await);
    let _ = client
        .delete(format!("{base}/api/workspaces/{}", ws.id.0))
        .send()
        .await
        .unwrap();
    // Give the async cleanup a brief moment.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!sandbox_volume_exists(fixture.data_dir.path(), &vol_name).await);
}
