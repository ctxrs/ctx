use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use axum::http::StatusCode;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::oneshot;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

use ctx_core::models::{MessageDelivery, SessionEventType, SessionTurnStatus};
use ctx_providers::adapters::{
    ProviderAdapter, ProviderHealth, ProviderStatus, ProviderUsability, RunHandle, TurnInput,
};
use ctx_providers::events::NormalizedEvent;
use uuid::Uuid;

mod common;

const MATCH_WAIT_TIMEOUT_MS: i64 = 15_000;

#[derive(Default)]
struct BrokenOutcomeProviderAdapter;

#[async_trait]
impl ProviderAdapter for BrokenOutcomeProviderAdapter {
    async fn inspect(&self) -> Result<ProviderStatus> {
        Ok(ProviderStatus {
            // The adapter is registered under "broken"; fake is test-special-cased
            // as ready without a provider-matrix install contract.
            provider_id: "fake".into(),
            installed: true,
            detected_path: None,
            version: Some("test".into()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ProviderUsability::default(),
        })
    }

    async fn run(
        &self,
        input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        event_sink: tokio::sync::mpsc::Sender<NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<RunHandle> {
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let (outcome_tx, outcome_rx) = oneshot::channel();
        let omit_abort_handle = input.content.contains("no-abort");
        let join = tokio::spawn(async move {
            if input.content.contains("cancel-") {
                let _ = event_sink
                    .send(NormalizedEvent {
                        event_type: SessionEventType::AssistantChunk,
                        payload_json: json!({
                            "content": "started",
                            "content_fragment": "started",
                            "message_id": Uuid::new_v4().to_string(),
                            "order_seq": 1,
                        }),
                    })
                    .await;
            }

            if input.content.contains("done-without-outcome") {
                let _ = done_tx.send(());
                let _keep_event_sink = event_sink;
                let _keep_outcome_open = outcome_tx;
                std::future::pending::<()>().await;
            } else if input.content.contains("done-close-outcome") {
                let _ = done_tx.send(());
                let _keep_event_sink = event_sink;
                drop(outcome_tx);
                std::future::pending::<()>().await;
            } else if input.content.contains("cancel-without-outcome") {
                let _ = cancel_rx.await;
                let _keep_event_sink = event_sink;
                let _keep_done_open = done_tx;
                let _keep_outcome_open = outcome_tx;
                std::future::pending::<()>().await;
            } else if input.content.contains("cancel-close-outcome") {
                let _ = cancel_rx.await;
                let _keep_event_sink = event_sink;
                let _keep_done_open = done_tx;
                drop(outcome_tx);
                std::future::pending::<()>().await;
            } else {
                let _keep_event_sink = event_sink;
                let _keep_done_open = done_tx;
                let _keep_outcome_open = outcome_tx;
                std::future::pending::<()>().await;
            }
        });

        Ok(RunHandle {
            done: done_rx,
            outcome: outcome_rx,
            cancel: Some(cancel_tx),
            abort: if omit_abort_handle {
                None
            } else {
                Some(join.abort_handle())
            },
        })
    }

    async fn cancel(&self, handle: &mut RunHandle) -> Result<()> {
        if let Some(cancel) = handle.cancel.take() {
            let _ = cancel.send(());
        }
        Ok(())
    }
}

async fn setup_state_with_providers(
    repo_root: &Path,
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
) -> common::SubagentMcpDaemonFixture {
    common::subagent_mcp_daemon_fixture_with_providers(repo_root, providers, "http://127.0.0.1:0")
        .await
}

async fn setup_state(repo_root: &Path) -> common::SubagentMcpDaemonFixture {
    setup_state_with_providers(repo_root, common::fake_providers()).await
}

async fn spawn_agent(
    client: &reqwest::Client,
    base: &str,
    parent_id: &str,
    task_label: &str,
    prompt: &str,
    harness: &str,
    model: &str,
) -> serde_json::Value {
    let resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/spawn_agent"))
        .json(&json!({
            "worktree": "inherit",
            "task_label": task_label,
            "prompt": prompt,
            "harness": harness,
            "model": model,
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn spawned_agent_id(body: &serde_json::Value) -> String {
    body["agent"]["agent"]["agent_id"]
        .as_str()
        .expect("spawned agent_id")
        .to_string()
}

async fn wait_for_agent_current_run(
    client: &reqwest::Client,
    base: &str,
    parent_id: &str,
    agent_id: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let resp = client
            .post(format!("{base}/api/mcp/sessions/{parent_id}/get_agent"))
            .json(&json!({ "agent_id": agent_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        let agent = &body["agent"]["agent"];
        if agent["state"] == "running" && agent["current_run_id"].is_string() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for agent run to start: {body:#?}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

async fn git_branch_exists(root: &Path, branch: &str) -> bool {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .await
        .unwrap();
    output.status.success()
}

fn create_branch_lock(root: &Path, branch: &str) -> PathBuf {
    let lock_path = root
        .join(".git")
        .join("refs")
        .join("heads")
        .join(format!("{branch}.lock"));
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&lock_path, "").unwrap();
    lock_path
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set_path(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = self.previous.take() {
            unsafe {
                std::env::set_var(self.key, value);
            }
        } else {
            unsafe {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn sandbox_cli_env_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn write_running_container_sandbox_cli_shim(
    dir: &Path,
    log_path: &Path,
    container_name: &str,
) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join("sandbox-cli-running-container-test.sh");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nLOG=\"{log}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"info\" ]; then\n  printf '{{}}\\n'\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"start\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"init\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"image\" ] && [ \"$2\" = \"inspect\" ]; then\n  echo 'transient image store failure' >&2\n  exit 125\nfi\nif [ \"$1\" = \"volume\" ] && [ \"$2\" = \"inspect\" ]; then\n  exit 1\nfi\nif [ \"$1\" = \"volume\" ] && [ \"$2\" = \"create\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"inspect\" ] && [ \"$2\" = \"{container}\" ]; then\n  suffix=${{2#ctx-harness-}}\n  printf '[{{\"Mounts\":[{{\"Type\":\"volume\",\"Name\":\"ctx-ws-%s\",\"Destination\":\"/ctx/ws\"}}]}}]\\n' \"$suffix\"\n  exit 0\nfi\nif [ \"$1\" = \"container\" ] && [ \"$2\" = \"inspect\" ] && [ \"$3\" = \"{container}\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"container\" ] && [ \"$2\" = \"inspect\" ] && [ \"$5\" = \"{container}\" ]; then\n  printf 'true\\n'\n  exit 0\nfi\nif [ \"$1\" = \"exec\" ]; then\n  shift\n  while [ \"$#\" -gt 0 ]; do\n    case \"$1\" in\n      --interactive)\n        shift\n        ;;\n      --user|--workdir|--env)\n        shift 2\n        ;;\n      *)\n        break\n        ;;\n    esac\n  done\n  target_container=\"$1\"\n  shift\n  command=\"$1\"\n  shift\n  if [ \"$target_container\" != \"{container}\" ]; then\n    echo \"unexpected container: $target_container\" >&2\n    exit 1\n  fi\n  if [ \"$command\" = \"tar\" ] && [ \"$1\" = \"-xf\" ] && [ \"$2\" = \"-\" ]; then\n    cat >/dev/null\n    exit 0\n  fi\n  if [ \"$command\" = \"git\" ] && [ \"$1\" = \"checkout\" ]; then\n    exit 0\n  fi\n  if [ \"$command\" = \"id\" ] && [ \"$1\" = \"-u\" ]; then\n    printf '1000\\n'\n    exit 0\n  fi\n  if [ \"$command\" = \"id\" ] && [ \"$1\" = \"-g\" ]; then\n    printf '1000\\n'\n    exit 0\n  fi\n  if [ \"$command\" = \"df\" ] && [ \"$1\" = \"-Pk\" ]; then\n    printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\\n'\n    printf 'overlay 10485760 1024 7340032 1%% /ctx/ws\\n'\n    exit 0\n  fi\n  if [ \"$command\" = \"sh\" ] && [ \"$1\" = \"-lc\" ]; then\n    case \"$2\" in\n      *\"git rev-parse --is-inside-work-tree\"*)\n        printf 'true\\n'\n        exit 0\n        ;;\n      *)\n        exit 0\n        ;;\n    esac\n  fi\n  exit 0\nfi\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n",
            log = log_path.display(),
            container = container_name,
        ),
    )
    .expect("write running-container sandbox CLI shim");
    let mut perms = fs::metadata(&path)
        .expect("sandbox cli metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).expect("chmod sandbox cli shim");
    path
}

async fn recv_workspace_stream_text(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> String {
    let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .expect("workspace stream timeout")
        .expect("workspace stream frame")
        .expect("workspace stream message");
    let WsMessage::Text(text) = message else {
        panic!("expected text frame");
    };
    text.to_string()
}

#[tokio::test]
async fn spawn_agent_accepts_raw_provider_status_when_derived_status_is_ready() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let parent_id = fixture.parent_id_string();

    let resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/spawn_agent"))
        .json(&json!({
            "worktree": "inherit",
            "prompt": "ready",
            "task_label": "Ready",
            "harness": "fake",
            "model": "fake-model"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["agent"]["agent"]["task_label"], "Ready");
    assert_eq!(body["agent"]["agent"]["state"], "running");
}

#[tokio::test]
async fn wait_agent_rejects_duplicate_agent_ids() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let parent_id = fixture.parent_id_string();
    let spawned = spawn_agent(client, base, &parent_id, "Dup", "a", "fake", "fake-model").await;
    let agent_id = spawned_agent_id(&spawned);

    let resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/wait_agent"))
        .json(&json!({
            "agent_ids": [agent_id, agent_id]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"]
        .as_str()
        .unwrap_or("")
        .contains("duplicate agent_id"));
}

#[tokio::test]
async fn spawn_agent_rejects_existing_task_label() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    fixture
        .daemon
        .seed_subagent_mcp_existing_label_child_for_test(fixture.parent_session.id, "Existing")
        .await
        .unwrap();
    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let parent_id = fixture.parent_id_string();
    let resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/spawn_agent"))
        .json(&json!({
            "worktree": "inherit",
            "prompt": "a",
            "task_label": "Existing",
            "harness": "fake",
            "model": "fake-model"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"]
        .as_str()
        .unwrap_or("")
        .contains("already exists"));
}

#[tokio::test]
async fn wait_agent_rejects_since_seq_for_multiple_agents() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let parent_id = fixture.parent_id_string();
    let first = spawn_agent(client, base, &parent_id, "One", "a", "fake", "fake-model").await;
    let second = spawn_agent(client, base, &parent_id, "Two", "b", "fake", "fake-model").await;

    let resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/wait_agent"))
        .json(&json!({
            "agent_ids": [
                spawned_agent_id(&first),
                spawned_agent_id(&second)
            ],
            "until": "update",
            "since_seq": 1
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap_or("").contains("since_seq"));
}

#[tokio::test]
async fn spawn_agent_rejects_worktree_new_when_dirty() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    fs::write(repo.path().join("dirty.txt"), "dirty").unwrap();

    let fixture = setup_state(repo.path()).await;
    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let parent_id = fixture.parent_id_string();

    let resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/spawn_agent"))
        .json(&json!({
            "worktree": "new",
            "prompt": "a",
            "task_label": "Dirty",
            "harness": "fake",
            "model": "fake-model"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["error"].as_str().unwrap_or(""),
        "Your worktree has uncommitted changes. Before starting new subagents in new worktree mode, you must commit or stash your changes to be explicit about whether subagents should inherit these diffs."
    );
}

#[tokio::test]
async fn send_input_queues_when_agent_is_busy() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let parent_id = fixture.parent_id_string();
    let spawned = spawn_agent(
        client,
        base,
        &parent_id,
        "BusyQueued",
        "first task",
        "fake",
        "fake-model",
    )
    .await;
    let queued_agent_id = spawned_agent_id(&spawned);
    let resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/send_input"))
        .json(&json!({ "agent_id": queued_agent_id, "message": "hi" }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["delivery"], "queued");
    assert!(body["queued_run_id"]
        .as_str()
        .unwrap_or("")
        .starts_with("run_"));

    let latest_turn = fixture
        .daemon
        .subagent_mcp_latest_turn_snapshot_for_test(fixture.parent_session.id, "BusyQueued")
        .await
        .unwrap();
    assert!(matches!(
        latest_turn.status,
        SessionTurnStatus::Queued | SessionTurnStatus::Starting | SessionTurnStatus::Running
    ));
    match latest_turn.message_delivery {
        Some(MessageDelivery::Queued) => assert!(!latest_turn.delivered_at_present),
        Some(MessageDelivery::Immediate) => assert!(latest_turn.delivered_at_present),
        other => panic!("unexpected queued message delivery: {other:?}"),
    }
}

#[tokio::test]
async fn archive_agent_hides_child_and_frees_active_slot_and_label() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    let archived_history = fixture
        .daemon
        .seed_subagent_mcp_archived_history_children_for_test(
            fixture.parent_session.id,
            12,
            "Reusable",
        )
        .await
        .unwrap();

    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let parent_id = fixture.parent_id_string();
    let list_resp = client
        .get(format!("{base}/api/mcp/sessions/{parent_id}/list_agents"))
        .send()
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let listed: serde_json::Value = list_resp.json().await.unwrap();
    assert_eq!(listed.as_array().map(Vec::len), Some(12));
    let archived_agent_id = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["task_label"] == "Reusable")
        .and_then(|agent| agent["agent_id"].as_str())
        .expect("reusable agent id")
        .to_string();

    let overflow_resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/spawn_agent"))
        .json(&json!({
            "worktree": "inherit",
            "prompt": "overflow",
            "task_label": "Overflow",
            "harness": "fake",
            "model": "fake-model"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(overflow_resp.status(), StatusCode::BAD_REQUEST);
    let overflow_body: serde_json::Value = overflow_resp.json().await.unwrap();
    assert!(overflow_body["error"]
        .as_str()
        .unwrap_or("")
        .contains("max 12 active child agents per parent"));

    let archive_resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/archive_agent"))
        .json(&json!({ "agent_id": archived_agent_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(archive_resp.status(), StatusCode::OK);
    let archive_body: serde_json::Value = archive_resp.json().await.unwrap();
    assert_eq!(archive_body["task_label"], "Reusable");
    assert_eq!(archive_body["archived"], true);
    assert_eq!(archive_body["cleanup_failed"], false);

    let get_archived_resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/get_agent"))
        .json(&json!({ "agent_id": archived_agent_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(get_archived_resp.status(), StatusCode::NOT_FOUND);
    let write_archived_resp = client
        .post(format!(
            "{base}/api/sessions/{}/messages",
            archived_history.child_session_id.0
        ))
        .json(&json!({ "content": "should fail" }))
        .send()
        .await
        .unwrap();
    assert_eq!(write_archived_resp.status(), StatusCode::NOT_FOUND);
    let snapshot_archived_resp = client
        .get(format!(
            "{base}/api/sessions/{}/snapshot",
            archived_history.child_session_id.0
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(snapshot_archived_resp.status(), StatusCode::OK);
    let snapshot_body: serde_json::Value = snapshot_archived_resp.json().await.unwrap();
    assert_eq!(
        snapshot_body["summary"]["session"]["id"],
        archived_history.child_session_id.0.to_string()
    );
    let archived_turn_tools_resp = client
        .get(format!(
            "{base}/api/sessions/{}/turns/{}/tools",
            archived_history.child_session_id.0, archived_history.turn_id.0
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(archived_turn_tools_resp.status(), StatusCode::OK);
    let archived_turn_tools: serde_json::Value = archived_turn_tools_resp.json().await.unwrap();
    assert_eq!(archived_turn_tools.as_array().map(Vec::len), Some(1));
    let completions_archived_resp = client
        .get(format!(
            "{base}/api/sessions/{}/file-completions",
            archived_history.child_session_id.0
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(completions_archived_resp.status(), StatusCode::NOT_FOUND);
    let archived_parent_list_resp = client
        .get(format!(
            "{base}/api/mcp/sessions/{}/list_agents",
            archived_history.child_session_id.0
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(archived_parent_list_resp.status(), StatusCode::NOT_FOUND);

    let list_after_resp = client
        .get(format!("{base}/api/mcp/sessions/{parent_id}/list_agents"))
        .send()
        .await
        .unwrap();
    assert_eq!(list_after_resp.status(), StatusCode::OK);
    let listed_after: serde_json::Value = list_after_resp.json().await.unwrap();
    let listed_after = listed_after.as_array().expect("list_agents array");
    assert_eq!(listed_after.len(), 11);
    assert!(listed_after
        .iter()
        .all(|agent| agent["task_label"] != "Reusable"));

    let resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/spawn_agent"))
        .json(&json!({
            "worktree": "inherit",
            "prompt": "replacement",
            "task_label": "Reusable",
            "harness": "fake",
            "model": "fake-model"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["agent"]["agent"]["task_label"], "Reusable");

    let replacement = fixture
        .daemon
        .subagent_mcp_child_by_label_for_test(fixture.parent_session.id, "Reusable")
        .await
        .unwrap();
    assert_ne!(replacement.session_id, archived_history.child_session_id);
    assert_eq!(
        fixture
            .daemon
            .subagent_mcp_active_child_count_for_test(fixture.parent_session.id)
            .await
            .unwrap(),
        12
    );
}

#[tokio::test]
async fn archive_agent_reclaims_dedicated_child_worktree() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    let parent_id = fixture.parent_id_string();

    let spawn_resp = fixture
        .server
        .client
        .post(format!(
            "{}/api/mcp/sessions/{parent_id}/spawn_agent",
            fixture.server.base_url
        ))
        .json(&json!({
            "worktree": "new",
            "prompt": "reclaim dedicated worktree",
            "task_label": "DedicatedCleanup",
            "harness": "fake",
            "model": "fake-model"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(spawn_resp.status(), StatusCode::OK);
    let spawn_body: serde_json::Value = spawn_resp.json().await.unwrap();
    let agent_id = spawned_agent_id(&spawn_body);
    let worktree_path = PathBuf::from(
        spawn_body["agent"]["worktree_path"]
            .as_str()
            .expect("worktree_path"),
    );
    let child = fixture
        .daemon
        .subagent_mcp_child_worktree_snapshot_for_test(
            fixture.parent_session.id,
            "DedicatedCleanup",
        )
        .await
        .unwrap();
    let child_branch = child.git_branch.clone().expect("child branch");
    let _serial = sandbox_cli_env_test_lock().lock().await;
    let log_path = fixture.data_dir.path().join("sandbox-cli.log");
    let sandbox_cli_path = write_running_container_sandbox_cli_shim(
        fixture.data_dir.path(),
        &log_path,
        &ctx_workspace_container::workspace_container_name(child.workspace_id),
    );
    let _sandbox_cli = EnvVarGuard::set_path("CTX_HARNESS_SANDBOX_CLI_PATH", &sandbox_cli_path);
    fixture
        .daemon
        .seed_subagent_mcp_sandbox_binding_for_test(child.workspace_id, child.worktree_id)
        .await
        .unwrap();
    assert_ne!(child.worktree_id, fixture.parent_session.worktree_id);
    assert!(worktree_path.exists());
    assert!(git_branch_exists(repo.path(), &child_branch).await);

    let wait_resp = fixture
        .server
        .client
        .post(format!(
            "{}/api/mcp/sessions/{parent_id}/wait_agent",
            fixture.server.base_url
        ))
        .json(&json!({ "agent_id": agent_id, "timeout_ms": MATCH_WAIT_TIMEOUT_MS }))
        .send()
        .await
        .unwrap();
    assert_eq!(wait_resp.status(), StatusCode::OK);

    let archive_resp = fixture
        .server
        .client
        .post(format!(
            "{}/api/mcp/sessions/{parent_id}/archive_agent",
            fixture.server.base_url
        ))
        .json(&json!({ "agent_id": agent_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(archive_resp.status(), StatusCode::OK);
    let archive_body: serde_json::Value = archive_resp.json().await.unwrap();
    assert_eq!(archive_body["cleanup_failed"], false);

    let cleanup = fixture
        .daemon
        .subagent_mcp_cleanup_snapshot_for_test(child.session_id)
        .await
        .unwrap();
    assert!(cleanup.archived);
    assert!(
        !cleanup.sandbox_binding_present,
        "successful dedicated child cleanup should remove the dedicated sandbox binding"
    );
    assert!(
        cleanup.worktree_metadata_present,
        "archived child should keep worktree metadata for transcript history"
    );
    assert!(
        !worktree_path.exists(),
        "archive_agent should reclaim dedicated child worktrees on disk"
    );
    assert!(
        !git_branch_exists(repo.path(), &child_branch).await,
        "archive_agent should reclaim dedicated child branches"
    );
    assert!(
        cleanup.workspace_index_present,
        "archived child should keep its workspace index because the session still references the worktree metadata"
    );
}

#[tokio::test]
async fn archive_agent_reports_cleanup_failure_when_dedicated_cleanup_is_incomplete() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    let parent_id = fixture.parent_id_string();

    let spawn_resp = fixture
        .server
        .client
        .post(format!(
            "{}/api/mcp/sessions/{parent_id}/spawn_agent",
            fixture.server.base_url
        ))
        .json(&json!({
            "worktree": "new",
            "prompt": "fail dedicated cleanup",
            "task_label": "DedicatedCleanupFailure",
            "harness": "fake",
            "model": "fake-model"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(spawn_resp.status(), StatusCode::OK);
    let spawn_body: serde_json::Value = spawn_resp.json().await.unwrap();
    let agent_id = spawned_agent_id(&spawn_body);
    let worktree_path = PathBuf::from(
        spawn_body["agent"]["worktree_path"]
            .as_str()
            .expect("worktree_path"),
    );
    let child = fixture
        .daemon
        .subagent_mcp_child_worktree_snapshot_for_test(
            fixture.parent_session.id,
            "DedicatedCleanupFailure",
        )
        .await
        .unwrap();
    let child_branch = child.git_branch.clone().expect("child branch");
    let _serial = sandbox_cli_env_test_lock().lock().await;
    let log_path = fixture.data_dir.path().join("sandbox-cli.log");
    let sandbox_cli_path = write_running_container_sandbox_cli_shim(
        fixture.data_dir.path(),
        &log_path,
        &ctx_workspace_container::workspace_container_name(child.workspace_id),
    );
    let _sandbox_cli = EnvVarGuard::set_path("CTX_HARNESS_SANDBOX_CLI_PATH", &sandbox_cli_path);
    fixture
        .daemon
        .seed_subagent_mcp_sandbox_binding_for_test(child.workspace_id, child.worktree_id)
        .await
        .unwrap();

    let wait_resp = fixture
        .server
        .client
        .post(format!(
            "{}/api/mcp/sessions/{parent_id}/wait_agent",
            fixture.server.base_url
        ))
        .json(&json!({ "agent_id": agent_id, "timeout_ms": MATCH_WAIT_TIMEOUT_MS }))
        .send()
        .await
        .unwrap();
    assert_eq!(wait_resp.status(), StatusCode::OK);

    let _branch_lock = create_branch_lock(repo.path(), &child_branch);

    let archive_resp = fixture
        .server
        .client
        .post(format!(
            "{}/api/mcp/sessions/{parent_id}/archive_agent",
            fixture.server.base_url
        ))
        .json(&json!({ "agent_id": agent_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(archive_resp.status(), StatusCode::OK);
    let archive_body: serde_json::Value = archive_resp.json().await.unwrap();
    assert_eq!(archive_body["archived"], true);
    assert_eq!(archive_body["cleanup_failed"], true);

    let cleanup = fixture
        .daemon
        .subagent_mcp_cleanup_snapshot_for_test(child.session_id)
        .await
        .unwrap();
    assert!(cleanup.archived);
    assert!(
        cleanup.sandbox_binding_present,
        "partial dedicated child cleanup should preserve the sandbox binding so leaked materialization can still be reclaimed"
    );
    assert!(
        !worktree_path.exists(),
        "dedicated child worktree cleanup should still remove the on-disk worktree before surfacing branch cleanup failure"
    );
    assert!(
        git_branch_exists(repo.path(), &child_branch).await,
        "archive_agent should surface branch cleanup failure instead of silently succeeding"
    );
}

#[tokio::test]
async fn archive_agent_preserves_inherited_parent_worktree() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    let parent_id = fixture.parent_id_string();

    let spawn_resp = fixture
        .server
        .client
        .post(format!(
            "{}/api/mcp/sessions/{parent_id}/spawn_agent",
            fixture.server.base_url
        ))
        .json(&json!({
            "worktree": "inherit",
            "prompt": "keep parent worktree",
            "task_label": "InheritedCleanup",
            "harness": "fake",
            "model": "fake-model"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(spawn_resp.status(), StatusCode::OK);
    let spawn_body: serde_json::Value = spawn_resp.json().await.unwrap();
    let agent_id = spawned_agent_id(&spawn_body);
    let child = fixture
        .daemon
        .subagent_mcp_child_by_label_for_test(fixture.parent_session.id, "InheritedCleanup")
        .await
        .unwrap();
    assert_eq!(child.worktree_id, fixture.parent_session.worktree_id);

    let wait_resp = fixture
        .server
        .client
        .post(format!(
            "{}/api/mcp/sessions/{parent_id}/wait_agent",
            fixture.server.base_url
        ))
        .json(&json!({ "agent_id": agent_id, "timeout_ms": MATCH_WAIT_TIMEOUT_MS }))
        .send()
        .await
        .unwrap();
    assert_eq!(wait_resp.status(), StatusCode::OK);

    let archive_resp = fixture
        .server
        .client
        .post(format!(
            "{}/api/mcp/sessions/{parent_id}/archive_agent",
            fixture.server.base_url
        ))
        .json(&json!({ "agent_id": agent_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(archive_resp.status(), StatusCode::OK);
    let archive_body: serde_json::Value = archive_resp.json().await.unwrap();
    assert_eq!(archive_body["cleanup_failed"], false);

    let cleanup = fixture
        .daemon
        .subagent_mcp_cleanup_snapshot_for_test(child.session_id)
        .await
        .unwrap();
    assert!(cleanup.archived);
    assert!(
        repo.path().exists(),
        "archive_agent must not reclaim the parent worktree for inherited children"
    );
}

#[tokio::test]
async fn archive_agent_emits_workspace_stream_session_removed_for_explicit_child_subscription() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    let parent_id = fixture.parent_id_string();

    let child = fixture
        .daemon
        .seed_subagent_mcp_existing_label_child_for_test(fixture.parent_session.id, "Watch Me")
        .await
        .unwrap();
    let ws_url = format!(
        "{}/api/workspaces/{}/stream",
        fixture.server.base_url, fixture.parent_session.workspace_id.0
    )
    .replace("http://", "ws://");
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();

    let ready = recv_workspace_stream_text(&mut socket).await;
    let ready_message: ctx_core::models::WorkspaceActiveSnapshotStreamMessage =
        serde_json::from_str(&ready).unwrap();
    assert!(matches!(
        ready_message,
        ctx_core::models::WorkspaceActiveSnapshotStreamMessage::Event { ref event, .. }
            if matches!(event.as_ref(), ctx_core::models::WorkspaceActiveSnapshotEvent::Ready { .. })
    ));

    socket
        .send(WsMessage::Text(
            json!({
                "type": "subscribe",
                "sessions": [{
                    "session_id": child.session_id.0,
                    "replay": { "mode": "auto" }
                }],
                "include_active_heads": true,
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    let _initial_snapshot = recv_workspace_stream_text(&mut socket).await;

    let list_resp = fixture
        .server
        .client
        .get(format!(
            "{}/api/mcp/sessions/{parent_id}/list_agents",
            fixture.server.base_url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let listed: serde_json::Value = list_resp.json().await.unwrap();
    let child_agent_id = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["task_label"] == "Watch Me")
        .and_then(|agent| agent["agent_id"].as_str())
        .expect("watch me agent id")
        .to_string();

    let archive_resp = fixture
        .server
        .client
        .post(format!(
            "{}/api/mcp/sessions/{parent_id}/archive_agent",
            fixture.server.base_url
        ))
        .json(&json!({ "agent_id": child_agent_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(archive_resp.status(), StatusCode::OK);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline {
        let next = tokio::time::timeout(Duration::from_millis(250), socket.next()).await;
        let Ok(Some(Ok(WsMessage::Text(txt)))) = next else {
            continue;
        };
        let message: ctx_core::models::WorkspaceActiveSnapshotStreamMessage =
            serde_json::from_str(&txt).unwrap();
        let ctx_core::models::WorkspaceActiveSnapshotStreamMessage::Event { event, .. } = message
        else {
            continue;
        };
        match event.as_ref() {
            ctx_core::models::WorkspaceActiveSnapshotEvent::SessionRemoved {
                session_id, ..
            } if *session_id == child.session_id => {
                fixture
                    .daemon
                    .publish_subagent_mcp_head_delta_for_test(child.session_id)
                    .await
                    .unwrap();
                let leaked = tokio::time::timeout(Duration::from_millis(300), socket.next()).await;
                if let Ok(Some(Ok(WsMessage::Text(txt)))) = leaked {
                    let message: ctx_core::models::WorkspaceActiveSnapshotStreamMessage =
                        serde_json::from_str(&txt).unwrap();
                    if matches!(
                        message,
                        ctx_core::models::WorkspaceActiveSnapshotStreamMessage::HeadsBatch { .. }
                    ) {
                        panic!(
                            "archived explicit child subscription should be evicted before later head deltas"
                        );
                    }
                }
                return;
            }
            _ => {}
        }
    }

    panic!("expected session_removed event for archived explicit child subscription");
}

#[tokio::test]
async fn archive_agent_rejects_busy_child() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    let parent_id = fixture.parent_id_string();
    fixture
        .daemon
        .seed_subagent_mcp_busy_archive_child_for_test(fixture.parent_session.id, "BusyArchive")
        .await
        .unwrap();

    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let list_resp = client
        .get(format!("{base}/api/mcp/sessions/{parent_id}/list_agents"))
        .send()
        .await
        .unwrap();
    let listed: serde_json::Value = list_resp.json().await.unwrap();
    let agent_id = listed[0]["agent_id"]
        .as_str()
        .expect("agent_id")
        .to_string();

    let archive_resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/archive_agent"))
        .json(&json!({ "agent_id": agent_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(archive_resp.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = archive_resp.json().await.unwrap();
    assert!(body["error"]
        .as_str()
        .unwrap_or("")
        .contains("active or queued work"));
}

#[tokio::test]
async fn spawn_agent_rejects_nested_child_depth() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    let child = fixture
        .daemon
        .seed_subagent_mcp_existing_label_child_for_test(fixture.parent_session.id, "Nested")
        .await
        .unwrap();

    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let resp = client
        .post(format!(
            "{base}/api/mcp/sessions/{}/spawn_agent",
            child.session_id.0
        ))
        .json(&json!({
            "worktree": "inherit",
            "prompt": "nested",
            "task_label": "TooDeep",
            "harness": "fake",
            "model": "fake-model"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"]
        .as_str()
        .unwrap_or("")
        .contains("max depth is 1"));
}

#[tokio::test]
async fn send_input_reports_immediate_delivery_when_agent_is_idle() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    let parent_id = fixture.parent_id_string();
    fixture
        .daemon
        .seed_subagent_mcp_existing_label_child_for_test(fixture.parent_session.id, "IdleImmediate")
        .await
        .unwrap();

    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let list_resp = client
        .get(format!("{base}/api/mcp/sessions/{parent_id}/list_agents"))
        .send()
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let listed: serde_json::Value = list_resp.json().await.unwrap();
    let agent_id = listed[0]["agent_id"]
        .as_str()
        .expect("agent_id")
        .to_string();

    let resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/send_input"))
        .json(&json!({ "agent_id": agent_id, "message": "start work" }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["delivery"], "immediate");
    assert!(body["queued_run_id"]
        .as_str()
        .unwrap_or("")
        .starts_with("run_"));
    assert_eq!(body["agent"]["agent"]["state"], "starting");

    let latest_turn = fixture
        .daemon
        .subagent_mcp_latest_turn_snapshot_for_test(fixture.parent_session.id, "IdleImmediate")
        .await
        .unwrap();
    assert_eq!(latest_turn.status, SessionTurnStatus::Starting);
    assert!(matches!(
        latest_turn.message_delivery,
        Some(MessageDelivery::Immediate)
    ));
}

#[tokio::test]
async fn list_agents_is_summary_only_and_get_agent_returns_context_window() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    let parent_id = fixture.parent_id_string();
    fixture
        .daemon
        .seed_subagent_mcp_context_window_child_for_test(fixture.parent_session.id, "Alpha")
        .await
        .unwrap();

    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let list_resp = client
        .get(format!("{base}/api/mcp/sessions/{parent_id}/list_agents"))
        .send()
        .await
        .unwrap();

    assert_eq!(list_resp.status(), StatusCode::OK);
    let listed: serde_json::Value = list_resp.json().await.unwrap();
    let agents = listed.as_array().expect("list_agents array");
    assert_eq!(agents.len(), 1);
    let agent = agents[0].as_object().expect("agent summary");
    assert!(agent.get("current_run_id").is_none());
    assert!(agent.get("context_window").is_none());
    let agent_id = agent["agent_id"].as_str().expect("agent_id").to_string();

    let get_resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/get_agent"))
        .json(&json!({ "agent_id": agent_id }))
        .send()
        .await
        .unwrap();

    assert_eq!(get_resp.status(), StatusCode::OK);
    let body: serde_json::Value = get_resp.json().await.unwrap();
    let ctx = body["agent"]["latest_result"]["context_window"]
        .as_object()
        .unwrap();
    assert_eq!(ctx.len(), 4);
    assert!(ctx.contains_key("total"));
    assert!(ctx.contains_key("used"));
    assert!(ctx.contains_key("remaining"));
    assert!(ctx.contains_key("utilization"));
}

#[tokio::test]
async fn list_agents_reports_queued_turn_as_active_and_preserves_latest_result() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    let parent_id = fixture.parent_id_string();
    fixture
        .daemon
        .seed_subagent_mcp_queued_history_child_for_test(fixture.parent_session.id, "QueuedAgent")
        .await
        .unwrap();

    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let list_resp = client
        .get(format!("{base}/api/mcp/sessions/{parent_id}/list_agents"))
        .send()
        .await
        .unwrap();

    assert_eq!(list_resp.status(), StatusCode::OK);
    let listed: serde_json::Value = list_resp.json().await.unwrap();
    let agent = listed[0].as_object().expect("agent summary");
    assert_eq!(agent["task_label"], "QueuedAgent");
    assert_eq!(agent["state"], "queued");
    assert_eq!(agent["latest_result_status"], "failed");
    assert!(agent["current_run_id"]
        .as_str()
        .unwrap_or("")
        .starts_with("run_"));

    let get_resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/get_agent"))
        .json(&json!({ "agent_id": agent["agent_id"] }))
        .send()
        .await
        .unwrap();

    assert_eq!(get_resp.status(), StatusCode::OK);
    let body: serde_json::Value = get_resp.json().await.unwrap();
    assert_eq!(body["agent"]["agent"]["state"], "queued");
    assert_eq!(body["agent"]["agent"]["latest_result_status"], "failed");
    assert_eq!(body["agent"]["latest_result"]["status"], "failed");
}

#[tokio::test]
async fn subagent_wait_fails_when_child_exits_without_terminal_event() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let parent_id = fixture.parent_id_string();

    let spawned = spawn_agent(
        client,
        base,
        &parent_id,
        "MissingTerminal",
        "omit-terminal-event",
        "fake",
        "fake-model",
    )
    .await;
    let agent_id = spawned_agent_id(&spawned);

    let wait_resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/wait_agent"))
        .json(&json!({ "agent_id": agent_id, "timeout_ms": MATCH_WAIT_TIMEOUT_MS }))
        .send()
        .await
        .unwrap();

    assert_eq!(wait_resp.status(), StatusCode::OK);
    let body: serde_json::Value = wait_resp.json().await.unwrap();
    assert_eq!(body["wait_status"], "matched");
    assert_eq!(body["results"][0]["agent"]["task_label"], "MissingTerminal");
    assert_eq!(
        body["results"][0]["agent"]["latest_result_status"],
        "failed"
    );
}

#[tokio::test]
async fn subagent_wait_fails_when_child_finishes_without_reporting_outcome() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let mut providers = common::fake_providers();
    providers.insert("broken".into(), Arc::new(BrokenOutcomeProviderAdapter));
    let fixture = setup_state_with_providers(repo.path(), providers).await;
    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let parent_id = fixture.parent_id_string();

    let spawned = spawn_agent(
        client,
        base,
        &parent_id,
        "MissingOutcome",
        "done-without-outcome",
        "broken",
        "broken-model",
    )
    .await;
    let agent_id = spawned_agent_id(&spawned);

    let wait_resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/wait_agent"))
        .json(&json!({ "agent_id": agent_id, "timeout_ms": MATCH_WAIT_TIMEOUT_MS }))
        .send()
        .await
        .unwrap();

    assert_eq!(wait_resp.status(), StatusCode::OK);
    let body: serde_json::Value = wait_resp.json().await.unwrap();
    assert_eq!(body["wait_status"], "matched");
    assert_eq!(body["results"][0]["agent"]["task_label"], "MissingOutcome");
    assert_eq!(
        body["results"][0]["agent"]["latest_result_status"],
        "failed"
    );
}

#[tokio::test]
async fn subagent_wait_fails_when_child_closes_outcome_without_reporting() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let mut providers = common::fake_providers();
    providers.insert("broken".into(), Arc::new(BrokenOutcomeProviderAdapter));
    let fixture = setup_state_with_providers(repo.path(), providers).await;
    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let parent_id = fixture.parent_id_string();

    let spawned = spawn_agent(
        client,
        base,
        &parent_id,
        "ClosedOutcome",
        "done-close-outcome-no-abort",
        "broken",
        "broken-model",
    )
    .await;
    let agent_id = spawned_agent_id(&spawned);

    let wait_resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/wait_agent"))
        .json(&json!({ "agent_id": agent_id, "timeout_ms": MATCH_WAIT_TIMEOUT_MS }))
        .send()
        .await
        .unwrap();

    assert_eq!(wait_resp.status(), StatusCode::OK);
    let body: serde_json::Value = wait_resp.json().await.unwrap();
    assert_eq!(body["wait_status"], "matched");
    assert_eq!(body["results"][0]["agent"]["task_label"], "ClosedOutcome");
    assert_eq!(
        body["results"][0]["agent"]["latest_result_status"],
        "failed"
    );
}

#[tokio::test]
async fn subagent_wait_fails_when_child_stalls_without_done_or_outcome() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let mut providers = common::fake_providers();
    providers.insert("broken".into(), Arc::new(BrokenOutcomeProviderAdapter));
    let fixture = setup_state_with_providers(repo.path(), providers).await;
    fixture
        .daemon
        .set_provider_inactivity_timeout(Duration::from_millis(250))
        .await;
    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let parent_id = fixture.parent_id_string();

    let spawned = spawn_agent(
        client,
        base,
        &parent_id,
        "StalledOutcome",
        "stall-without-outcome",
        "broken",
        "broken-model",
    )
    .await;
    let agent_id = spawned_agent_id(&spawned);

    let wait_resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/wait_agent"))
        .json(&json!({ "agent_id": agent_id, "timeout_ms": 1_000 }))
        .send()
        .await
        .unwrap();

    assert_eq!(wait_resp.status(), StatusCode::OK);
    let body: serde_json::Value = wait_resp.json().await.unwrap();
    assert_eq!(body["wait_status"], "timeout");
    assert_eq!(body["results"][0]["agent"]["task_label"], "StalledOutcome");
    assert_eq!(body["results"][0]["agent"]["health"], "stalled");
}

#[tokio::test]
async fn subagent_interrupt_does_not_override_completed_child_outcome() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let parent_id = fixture.parent_id_string();

    let spawned = spawn_agent(
        client,
        base,
        &parent_id,
        "CancelCompletes",
        "slow-diff-test complete-on-cancel",
        "fake",
        "fake-model",
    )
    .await;
    let agent_id = spawned_agent_id(&spawned);

    sleep(Duration::from_millis(100)).await;

    let interrupt_resp = client
        .post(format!(
            "{base}/api/mcp/sessions/{parent_id}/interrupt_agent"
        ))
        .json(&json!({ "agent_id": agent_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(interrupt_resp.status(), StatusCode::OK);

    let wait_resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/wait_agent"))
        .json(
            &json!({ "agent_id": spawned_agent_id(&spawned), "timeout_ms": MATCH_WAIT_TIMEOUT_MS }),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(wait_resp.status(), StatusCode::OK);
    let body: serde_json::Value = wait_resp.json().await.unwrap();
    assert_eq!(body["wait_status"], "matched");
    assert_eq!(body["results"][0]["agent"]["task_label"], "CancelCompletes");
    assert_eq!(
        body["results"][0]["agent"]["latest_result_status"],
        "completed"
    );

    let latest_turn = fixture
        .daemon
        .subagent_mcp_latest_turn_snapshot_for_test(fixture.parent_session.id, "CancelCompletes")
        .await
        .unwrap();
    assert_eq!(latest_turn.status, SessionTurnStatus::Completed);
    assert!(
        !latest_turn.turn_interrupted,
        "completed-on-cancel flow should not persist an interrupted event"
    );
}

#[tokio::test]
async fn subagent_interrupt_falls_back_to_interrupted_when_child_never_reports_outcome() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let mut providers = common::fake_providers();
    providers.insert("broken".into(), Arc::new(BrokenOutcomeProviderAdapter));
    let fixture = setup_state_with_providers(repo.path(), providers).await;
    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let parent_id = fixture.parent_id_string();

    let spawned = spawn_agent(
        client,
        base,
        &parent_id,
        "MissingInterruptOutcome",
        "cancel-without-outcome",
        "broken",
        "broken-model",
    )
    .await;
    let agent_id = spawned_agent_id(&spawned);
    wait_for_agent_current_run(client, base, &parent_id, &agent_id).await;

    let interrupt_resp = client
        .post(format!(
            "{base}/api/mcp/sessions/{parent_id}/interrupt_agent"
        ))
        .json(&json!({ "agent_id": agent_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(interrupt_resp.status(), StatusCode::OK);

    let wait_resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/wait_agent"))
        .json(
            &json!({ "agent_id": spawned_agent_id(&spawned), "timeout_ms": MATCH_WAIT_TIMEOUT_MS }),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(wait_resp.status(), StatusCode::OK);
    let body: serde_json::Value = wait_resp.json().await.unwrap();
    assert_eq!(body["wait_status"], "matched");
    assert_eq!(
        body["results"][0]["agent"]["task_label"],
        "MissingInterruptOutcome"
    );
    assert_eq!(
        body["results"][0]["agent"]["latest_result_status"],
        "interrupted"
    );

    let latest_turn = fixture
        .daemon
        .subagent_mcp_latest_turn_snapshot_for_test(
            fixture.parent_session.id,
            "MissingInterruptOutcome",
        )
        .await
        .unwrap();
    assert!(
        latest_turn.turn_interrupted,
        "fallback interrupt should persist TurnInterrupted"
    );
}

#[tokio::test]
async fn subagent_interrupt_falls_back_to_interrupted_when_child_closes_outcome() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let mut providers = common::fake_providers();
    providers.insert("broken".into(), Arc::new(BrokenOutcomeProviderAdapter));
    let fixture = setup_state_with_providers(repo.path(), providers).await;
    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let parent_id = fixture.parent_id_string();

    let spawned = spawn_agent(
        client,
        base,
        &parent_id,
        "ClosedInterruptOutcome",
        "cancel-close-outcome-no-abort",
        "broken",
        "broken-model",
    )
    .await;
    let agent_id = spawned_agent_id(&spawned);
    wait_for_agent_current_run(client, base, &parent_id, &agent_id).await;

    let interrupt_resp = client
        .post(format!(
            "{base}/api/mcp/sessions/{parent_id}/interrupt_agent"
        ))
        .json(&json!({ "agent_id": agent_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(interrupt_resp.status(), StatusCode::OK);

    let wait_resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/wait_agent"))
        .json(
            &json!({ "agent_id": spawned_agent_id(&spawned), "timeout_ms": MATCH_WAIT_TIMEOUT_MS }),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(wait_resp.status(), StatusCode::OK);
    let body: serde_json::Value = wait_resp.json().await.unwrap();
    assert_eq!(body["wait_status"], "matched");
    assert_eq!(
        body["results"][0]["agent"]["task_label"],
        "ClosedInterruptOutcome"
    );
    assert_eq!(
        body["results"][0]["agent"]["latest_result_status"],
        "interrupted"
    );

    let latest_turn = fixture
        .daemon
        .subagent_mcp_latest_turn_snapshot_for_test(
            fixture.parent_session.id,
            "ClosedInterruptOutcome",
        )
        .await
        .unwrap();
    assert!(
        latest_turn.turn_interrupted,
        "closed-channel fallback interrupt should persist TurnInterrupted"
    );
}

#[tokio::test]
async fn spawn_agent_worktree_new_runs_bootstrap() {
    let repo = common::init_git_repo(&[("README.md", "ok")]).await;
    let fixture = setup_state(repo.path()).await;
    fixture
        .daemon
        .seed_subagent_mcp_worktree_bootstrap_config_for_test(
            fixture.parent_session.id,
            "sh -c \"mkdir -p .ctx && echo bootstrapped > .ctx/bootstrap.txt\"".to_string(),
        )
        .await
        .unwrap();
    let client = &fixture.server.client;
    let base = &fixture.server.base_url;
    let parent_id = fixture.parent_id_string();

    let resp = client
        .post(format!("{base}/api/mcp/sessions/{parent_id}/spawn_agent"))
        .json(&json!({
            "worktree": "new",
            "prompt": "boot",
            "task_label": "Bootstrap",
            "harness": "fake",
            "model": "fake-model"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    let worktree_path = body["agent"]["worktree_path"]
        .as_str()
        .expect("missing worktree_path");
    assert_ne!(worktree_path, repo.path().to_string_lossy());
    assert!(Path::new(worktree_path).exists());

    let bootstrap_path = Path::new(worktree_path).join(".ctx/bootstrap.txt");
    let mut found = false;
    for _ in 0..20 {
        if bootstrap_path.exists() {
            found = true;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(found, "bootstrap did not create marker file");
}
