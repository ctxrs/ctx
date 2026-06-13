use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::{mpsc, Mutex as AsyncMutex};

use ctx_core::models::{Session, SessionEventType, SessionHeadSnapshot, Task, Workspace};
use ctx_providers::adapters::{
    ProviderAdapter, ProviderCapabilities, ProviderHealth, ProviderProcessInfo,
    ProviderRestartMode, ProviderStatus, RunHandle, TurnInput,
};
use ctx_providers::crp::Tier1CrpAdapter;
use ctx_providers::events::NormalizedEvent;

mod common;

static ENV_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

async fn lock_env() -> tokio::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().await
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

async fn wait_for_live_session_ready(
    event_rx: &mut mpsc::Receiver<NormalizedEvent>,
    timeout: Duration,
) {
    tokio::time::timeout(timeout, async {
        while let Some(event) = event_rx.recv().await {
            match event.event_type {
                SessionEventType::Init => return,
                SessionEventType::Notice => {
                    let kind = event
                        .payload_json
                        .get("kind")
                        .and_then(|value| value.as_str());
                    if matches!(
                        kind,
                        Some("authenticated")
                            | Some("auth_complete")
                            | Some("auth_completed")
                            | Some("auth_success")
                    ) {
                        continue;
                    }
                }
                _ => {}
            }
        }
        panic!("event stream closed before session became ready");
    })
    .await
    .expect("timed out waiting for live session readiness");
}

async fn wait_for_notice_kind(
    event_rx: &mut mpsc::Receiver<NormalizedEvent>,
    expected_kind: &str,
    timeout: Duration,
) {
    tokio::time::timeout(timeout, async {
        while let Some(event) = event_rx.recv().await {
            if !matches!(event.event_type, SessionEventType::Notice) {
                continue;
            }
            let kind = event
                .payload_json
                .get("kind")
                .and_then(|value| value.as_str());
            if kind == Some(expected_kind) {
                return;
            }
        }
        panic!("event stream closed before notice {expected_kind} arrived");
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for notice {expected_kind}"));
}

async fn create_task_with_default_session(
    client: &reqwest::Client,
    base: &str,
    workspace_id: ctx_core::ids::WorkspaceId,
    provider_id: &str,
    model_id: &str,
) -> (Task, Session) {
    create_task_with_default_session_and_reasoning(
        client,
        base,
        workspace_id,
        provider_id,
        model_id,
        None,
    )
    .await
}

async fn create_task_with_default_session_and_reasoning(
    client: &reqwest::Client,
    base: &str,
    workspace_id: ctx_core::ids::WorkspaceId,
    provider_id: &str,
    model_id: &str,
    reasoning_effort: Option<&str>,
) -> (Task, Session) {
    let session_id = uuid::Uuid::new_v4().to_string();
    let mut default_session = json!({
        "id": session_id,
        "provider_id": provider_id,
        "model_id": model_id,
    });
    if let Some(reasoning_effort) = reasoning_effort {
        default_session["reasoning_effort"] = json!(reasoning_effort);
    }

    let task_response = client
        .post(format!("{base}/api/workspaces/{}/tasks", workspace_id.0))
        .json(&json!({
            "title": "session-model",
            "default_session": default_session
        }))
        .send()
        .await
        .expect("create task");
    let task_status = task_response.status();
    let task_body = task_response.text().await.expect("task response body");
    assert!(
        task_status.is_success(),
        "create task failed with {task_status}: {task_body}"
    );
    let task: Task = serde_json::from_str(&task_body).unwrap_or_else(|err| {
        panic!("decode task response failed for status {task_status}: {err}; body: {task_body}")
    });

    let sessions_response = client
        .get(format!("{base}/api/tasks/{}/sessions", task.id.0))
        .send()
        .await
        .expect("list sessions");
    let sessions_status = sessions_response.status();
    let sessions_body = sessions_response
        .text()
        .await
        .expect("sessions response body");
    assert!(
        sessions_status.is_success(),
        "list sessions failed with {sessions_status}: {sessions_body}"
    );
    let sessions: Vec<Session> = serde_json::from_str(&sessions_body).unwrap_or_else(|err| {
        panic!(
            "decode sessions response failed for status {sessions_status}: {err}; body: {sessions_body}"
        )
    });
    let session = sessions
        .into_iter()
        .find(|session| Some(session.id) == task.primary_session_id)
        .expect("created task should include default session");
    (task, session)
}

#[derive(Default)]
struct RecordingSetModelAdapter {
    calls: Mutex<Vec<(String, String)>>,
    live_session: bool,
    failure: Option<String>,
}

impl RecordingSetModelAdapter {
    fn live() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            live_session: true,
            failure: None,
        }
    }

    fn live_failing(message: &str) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            live_session: true,
            failure: Some(message.to_string()),
        }
    }
}

#[async_trait]
impl ProviderAdapter for RecordingSetModelAdapter {
    async fn inspect(&self) -> anyhow::Result<ProviderStatus> {
        Ok(ProviderStatus {
            provider_id: "fake-set-model".to_string(),
            installed: true,
            detected_path: None,
            version: Some("test".to_string()),
            capabilities: Some(ProviderCapabilities {
                stream_events: false,
                stream_format: "none".to_string(),
                has_turn_boundaries: false,
                has_tool_call_ids: false,
                has_file_change_events: false,
                has_command_events: false,
                supports_resume: false,
                supports_stable_session_id: true,
                supports_fork_or_rewind: false,
                supports_headless: true,
                supports_server_mode: false,
                supports_interactive_tui: false,
                supports_private_state_dir: false,
                supports_sandbox_flags: false,
                supports_approval_flags: false,
                notes: vec![],
            }),
            health: ProviderHealth::Ok,
            diagnostics: vec![],
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        })
    }

    async fn run(
        &self,
        _input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _event_sink: tokio::sync::mpsc::Sender<NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> anyhow::Result<RunHandle> {
        anyhow::bail!("test adapter does not implement run");
    }

    async fn cancel(&self, _handle: &mut RunHandle) -> anyhow::Result<()> {
        Ok(())
    }

    async fn list_processes(&self) -> Vec<ProviderProcessInfo> {
        Vec::new()
    }

    async fn restart(&self, _reason: &str, _mode: ProviderRestartMode) -> anyhow::Result<()> {
        Ok(())
    }

    async fn has_live_session(&self, _session_key: &str) -> bool {
        self.live_session
    }

    async fn set_session_model(&self, session_key: String, model_id: String) -> anyhow::Result<()> {
        if let Some(message) = self.failure.as_deref() {
            anyhow::bail!("{message}");
        }
        self.calls
            .lock()
            .expect("recording calls")
            .push((session_key, model_id));
        Ok(())
    }
}

#[tokio::test]
async fn set_session_model_updates_session_and_appends_init_event() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let adapter = Arc::new(RecordingSetModelAdapter::live());
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("fake-set-model".to_string(), adapter.clone());
    let fixture = common::fake_daemon_fixture_with_providers(providers, "http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;
    let base = &server.base_url;
    let client = &server.client;

    let workspace: Workspace = client
        .post(format!("{base}/api/workspaces"))
        .json(&json!({"root_path": repo.path(), "name": "ws"}))
        .send()
        .await
        .expect("create workspace")
        .json()
        .await
        .expect("workspace json");

    let (_task, session) = create_task_with_default_session(
        client,
        base,
        workspace.id,
        "fake-set-model",
        "start-model",
    )
    .await;

    let updated: Session = client
        .post(format!("{base}/api/sessions/{}/model", session.id.0))
        .json(&json!({"model_id":"next-model"}))
        .send()
        .await
        .expect("set session model")
        .json()
        .await
        .expect("updated session json");

    assert_eq!(updated.model_id, "next-model");
    assert_eq!(
        adapter.calls.lock().expect("calls").as_slice(),
        &[(session.id.0.to_string(), "next-model".to_string())]
    );

    let head: SessionHeadSnapshot = client
        .get(format!(
            "{base}/api/sessions/{}/head?limit=10&include_events=true",
            session.id.0
        ))
        .send()
        .await
        .expect("get session head")
        .json()
        .await
        .expect("session head json");

    let init_event = head
        .events
        .iter()
        .rev()
        .find(|event| matches!(event.event_type, SessionEventType::Init))
        .expect("init event appended");
    assert_eq!(
        init_event.payload_json.get("current_model_id"),
        Some(&json!("next-model"))
    );
}

#[tokio::test]
async fn live_crp_fixture_authenticate_session_emits_ready_signals_and_stays_live() {
    let _env_lock = lock_env().await;

    let provider_id = "codex";
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let fixtures_dir = tempfile::tempdir().expect("fixtures tempdir");
    let fixture_provider_dir = fixtures_dir.path().join(provider_id);
    std::fs::create_dir_all(&fixture_provider_dir).expect("create fixture provider dir");
    std::fs::write(
        fixture_provider_dir.join("basic.json"),
        serde_json::to_vec(&json!({
            "current_model_id": "gpt-5.4/medium",
            "models": [
                {"id": "gpt-5.4/medium", "name": "codex medium"},
                {"id": "gpt-5.4/xhigh", "name": "codex xhigh"}
            ],
            "turns": [{}]
        }))
        .expect("fixture json"),
    )
    .expect("write fixture");
    let _fixture_root = EnvGuard::set(
        "CTX_TEST_FIXTURES_DIR",
        &fixtures_dir.path().to_string_lossy(),
    );
    let _fixture_scenario = EnvGuard::set("CTX_TEST_SCENARIO", "basic");

    let python = common::crp_fixture_runtime::python_binary().expect("python available");
    let script_path = common::crp_fixture_runtime::write_crp_fixture_runtime(workdir.path());
    let adapter = Tier1CrpAdapter::from_raw(
        provider_id,
        python.to_string_lossy().to_string(),
        vec![script_path.to_string_lossy().to_string()],
    );
    let session_key = "fixture-auth-live-session".to_string();
    let auth_env = HashMap::from([
        ("CTX_PROVIDER_ID".to_string(), provider_id.to_string()),
        common::ctx_mcp_command_env_pair(),
    ]);
    let (event_tx, mut event_rx) = mpsc::channel::<NormalizedEvent>(16);

    adapter
        .authenticate_session(
            session_key.clone(),
            workdir.path().to_path_buf(),
            auth_env,
            None,
            event_tx,
            ctx_providers::adapters::ProviderRunHooks::default(),
        )
        .await
        .expect("authenticate live session");

    wait_for_live_session_ready(&mut event_rx, Duration::from_secs(5)).await;
    wait_for_notice_kind(&mut event_rx, "authenticated", Duration::from_secs(5)).await;
    assert!(
        adapter.has_live_session(&session_key).await,
        "fixture-backed CRP adapter should remain live after authenticate"
    );

    adapter
        .restart("test complete", ProviderRestartMode::Immediate)
        .await
        .expect("restart adapter");
}

#[tokio::test]
async fn set_session_model_skips_adapter_when_session_is_not_live() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let adapter = Arc::new(RecordingSetModelAdapter::default());
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("fake-set-model".to_string(), adapter.clone());
    let fixture = common::fake_daemon_fixture_with_providers(providers, "http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;
    let base = &server.base_url;
    let client = &server.client;

    let workspace: Workspace = client
        .post(format!("{base}/api/workspaces"))
        .json(&json!({"root_path": repo.path(), "name": "ws"}))
        .send()
        .await
        .expect("create workspace")
        .json()
        .await
        .expect("workspace json");

    let (_task, session) = create_task_with_default_session(
        client,
        base,
        workspace.id,
        "fake-set-model",
        "start-model",
    )
    .await;

    let updated: Session = client
        .post(format!("{base}/api/sessions/{}/model", session.id.0))
        .json(&json!({"model_id":"queued-model"}))
        .send()
        .await
        .expect("set session model")
        .json()
        .await
        .expect("updated session json");

    assert_eq!(updated.model_id, "queued-model");
    assert!(
        adapter.calls.lock().expect("calls").is_empty(),
        "non-live sessions should update stored model without adapter forwarding"
    );

    let head: SessionHeadSnapshot = client
        .get(format!(
            "{base}/api/sessions/{}/head?limit=10&include_events=true",
            session.id.0
        ))
        .send()
        .await
        .expect("get session head")
        .json()
        .await
        .expect("session head json");

    let init_event = head
        .events
        .iter()
        .rev()
        .find(|event| matches!(event.event_type, SessionEventType::Init))
        .expect("init event appended");
    assert_eq!(
        init_event.payload_json.get("current_model_id"),
        Some(&json!("queued-model"))
    );
}

#[tokio::test]
async fn set_session_model_returns_structured_error_when_live_switch_fails() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let adapter = Arc::new(RecordingSetModelAdapter::live_failing(
        "timed out waiting for session model update",
    ));
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("fake-set-model".to_string(), adapter);
    let fixture = common::fake_daemon_fixture_with_providers(providers, "http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;
    let base = &server.base_url;
    let client = &server.client;

    let workspace: Workspace = client
        .post(format!("{base}/api/workspaces"))
        .json(&json!({"root_path": repo.path(), "name": "ws"}))
        .send()
        .await
        .expect("create workspace")
        .json()
        .await
        .expect("workspace json");

    let (_task, session) = create_task_with_default_session(
        client,
        base,
        workspace.id,
        "fake-set-model",
        "start-model",
    )
    .await;

    let response = client
        .post(format!("{base}/api/sessions/{}/model", session.id.0))
        .json(&json!({"model_id":"next-model"}))
        .send()
        .await
        .expect("set session model");

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let payload: serde_json::Value = response.json().await.expect("error payload");
    let error = payload
        .get("error")
        .and_then(|value| value.as_str())
        .expect("error string");
    assert!(
        error.contains("failed to switch the live fake-set-model session"),
        "unexpected error: {error}"
    );
    assert!(
        error.contains("timed out waiting for session model update"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn create_session_splits_legacy_combined_model_id_into_reasoning_effort() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let adapter = Arc::new(RecordingSetModelAdapter::live());
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("fake-set-model".to_string(), adapter);
    let fixture = common::fake_daemon_fixture_with_providers(providers, "http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;
    let base = &server.base_url;
    let client = &server.client;

    let workspace: Workspace = client
        .post(format!("{base}/api/workspaces"))
        .json(&json!({"root_path": repo.path(), "name": "ws"}))
        .send()
        .await
        .expect("create workspace")
        .json()
        .await
        .expect("workspace json");

    let (_task, session) = create_task_with_default_session(
        client,
        base,
        workspace.id,
        "fake-set-model",
        "gpt-5/xhigh",
    )
    .await;

    assert_eq!(session.model_id, "gpt-5");
    assert_eq!(session.reasoning_effort.as_deref(), Some("xhigh"));
}

#[tokio::test]
async fn set_session_model_persists_reasoning_effort_and_forwards_full_model_id() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let adapter = Arc::new(RecordingSetModelAdapter::live());
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("fake-set-model".to_string(), adapter.clone());
    let fixture = common::fake_daemon_fixture_with_providers(providers, "http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;
    let base = &server.base_url;
    let client = &server.client;

    let workspace: Workspace = client
        .post(format!("{base}/api/workspaces"))
        .json(&json!({"root_path": repo.path(), "name": "ws"}))
        .send()
        .await
        .expect("create workspace")
        .json()
        .await
        .expect("workspace json");

    let (_task, session) = create_task_with_default_session(
        client,
        base,
        workspace.id,
        "fake-set-model",
        "start-model",
    )
    .await;

    let updated: Session = client
        .post(format!("{base}/api/sessions/{}/model", session.id.0))
        .json(&json!({
            "model_id":"gpt-5",
            "reasoning_effort":"xhigh"
        }))
        .send()
        .await
        .expect("set session model")
        .json()
        .await
        .expect("updated session json");

    assert_eq!(updated.model_id, "gpt-5");
    assert_eq!(updated.reasoning_effort.as_deref(), Some("xhigh"));
    assert_eq!(
        adapter.calls.lock().expect("calls").as_slice(),
        &[(session.id.0.to_string(), "gpt-5/xhigh".to_string())]
    );

    let head: SessionHeadSnapshot = client
        .get(format!(
            "{base}/api/sessions/{}/head?limit=10&include_events=true",
            session.id.0
        ))
        .send()
        .await
        .expect("get session head")
        .json()
        .await
        .expect("session head json");

    let init_event = head
        .events
        .iter()
        .rev()
        .find(|event| matches!(event.event_type, SessionEventType::Init))
        .expect("init event appended");
    assert_eq!(
        init_event.payload_json.get("current_model_id"),
        Some(&json!("gpt-5/xhigh"))
    );
    assert_eq!(
        init_event.payload_json.get("reasoning_effort"),
        Some(&json!("xhigh"))
    );
}

async fn assert_live_crp_session_model_switch_case(
    provider_id: &str,
    initial_model_id: &str,
    initial_reasoning_effort: &str,
    next_model_id: &str,
    next_reasoning_effort: &str,
) {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    let fixtures_dir = tempfile::tempdir().expect("fixtures tempdir");
    let fixture_provider_dir = fixtures_dir.path().join(provider_id);
    std::fs::create_dir_all(&fixture_provider_dir).expect("create fixture provider dir");
    std::fs::write(
        fixture_provider_dir.join("basic.json"),
        serde_json::to_vec(&json!({
            "current_model_id": format!("{initial_model_id}/{initial_reasoning_effort}"),
            "models": [
                {
                    "id": format!("{initial_model_id}/{initial_reasoning_effort}"),
                    "name": format!("{provider_id} initial")
                },
                {
                    "id": format!("{next_model_id}/{next_reasoning_effort}"),
                    "name": format!("{provider_id} next")
                }
            ],
            "turns": [{}]
        }))
        .expect("fixture json"),
    )
    .expect("write fixture");
    let _fixture_root = EnvGuard::set(
        "CTX_TEST_FIXTURES_DIR",
        &fixtures_dir.path().to_string_lossy(),
    );
    let _fixture_scenario = EnvGuard::set("CTX_TEST_SCENARIO", "basic");

    let python = common::crp_fixture_runtime::python_binary().expect("python available");
    let script_path = common::crp_fixture_runtime::write_crp_fixture_runtime(data_dir.path());
    let adapter: Arc<Tier1CrpAdapter> = Arc::new(Tier1CrpAdapter::from_raw(
        provider_id,
        python.to_string_lossy().to_string(),
        vec![script_path.to_string_lossy().to_string()],
    ));
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert(provider_id.to_string(), adapter.clone());
    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        providers,
        "http://127.0.0.1:0",
    )
    .await;
    let server = fixture.spawn_server().await;
    let base = &server.base_url;
    let client = &server.client;

    let seeded = fixture
        .daemon
        .seed_session_model_switch_session_for_test(
            repo.path(),
            provider_id,
            initial_model_id,
            Some(initial_reasoning_effort),
        )
        .await
        .expect("seed session model switch session");
    fixture
        .daemon
        .seed_host_session_model_catalog_cache_for_test(
            seeded.workspace.id,
            provider_id,
            format!("{initial_model_id}/{initial_reasoning_effort}"),
            vec![
                (
                    format!("{initial_model_id}/{initial_reasoning_effort}"),
                    format!("{provider_id} initial"),
                ),
                (
                    format!("{next_model_id}/{next_reasoning_effort}"),
                    format!("{provider_id} next"),
                ),
            ],
        )
        .await;
    let session = seeded.session;

    let seeded_head: SessionHeadSnapshot = client
        .get(format!(
            "{base}/api/sessions/{}/head?limit=50&include_events=false",
            session.id.0
        ))
        .send()
        .await
        .expect("seed compact session head")
        .json()
        .await
        .expect("seeded session head json");
    assert_eq!(seeded_head.session.model_id, initial_model_id);
    assert_eq!(
        seeded_head.session.reasoning_effort.as_deref(),
        Some(initial_reasoning_effort)
    );

    let (event_tx, mut event_rx) = mpsc::channel::<NormalizedEvent>(8);
    let auth_env = HashMap::from([
        ("CTX_PROVIDER_ID".to_string(), provider_id.to_string()),
        common::ctx_mcp_command_env_pair(),
    ]);
    adapter
        .authenticate_session(
            session.id.0.to_string(),
            repo.path().to_path_buf(),
            auth_env,
            None,
            event_tx,
            ctx_providers::adapters::ProviderRunHooks::default(),
        )
        .await
        .expect("open live CRP session");
    wait_for_live_session_ready(&mut event_rx, Duration::from_secs(5)).await;
    assert!(
        adapter.has_live_session(&session.id.0.to_string()).await,
        "expected adapter to track live session after authenticate"
    );

    let updated: Session = client
        .post(format!("{base}/api/sessions/{}/model", session.id.0))
        .json(&json!({
            "model_id": next_model_id,
            "reasoning_effort": next_reasoning_effort
        }))
        .send()
        .await
        .expect("set session model")
        .json()
        .await
        .expect("updated session json");

    assert_eq!(updated.model_id, next_model_id);
    assert_eq!(
        updated.reasoning_effort.as_deref(),
        Some(next_reasoning_effort)
    );

    let compact_head: SessionHeadSnapshot = client
        .get(format!(
            "{base}/api/sessions/{}/head?limit=50&include_events=false",
            session.id.0
        ))
        .send()
        .await
        .expect("get compact session head")
        .json()
        .await
        .expect("compact session head json");
    assert_eq!(compact_head.session.model_id, next_model_id);
    assert_eq!(
        compact_head.session.reasoning_effort.as_deref(),
        Some(next_reasoning_effort)
    );

    let head: SessionHeadSnapshot = client
        .get(format!(
            "{base}/api/sessions/{}/head?limit=50&include_events=true",
            session.id.0
        ))
        .send()
        .await
        .expect("get session head")
        .json()
        .await
        .expect("session head json");

    let init_event = head
        .events
        .iter()
        .rev()
        .find(|event| matches!(event.event_type, SessionEventType::Init))
        .expect("init event appended");
    assert_eq!(
        init_event.payload_json.get("current_model_id"),
        Some(&json!(format!("{next_model_id}/{next_reasoning_effort}")))
    );
    assert_eq!(
        init_event.payload_json.get("reasoning_effort"),
        Some(&json!(next_reasoning_effort))
    );
}

#[tokio::test]
async fn live_crp_supported_harnesses_session_model_switch_succeeds_end_to_end() {
    let _env_lock = lock_env().await;

    assert_live_crp_session_model_switch_case("codex", "gpt-5.4", "medium", "gpt-5.4", "xhigh")
        .await;
    assert_live_crp_session_model_switch_case("claude-crp", "default", "medium", "default", "high")
        .await;
}

#[tokio::test]
async fn live_acp_runtime_catalog_harnesses_session_model_switch_succeeds_end_to_end() {
    let _env_lock = lock_env().await;

    for provider_id in ["amp", "copilot", "cursor", "gemini", "kimi", "qwen"] {
        assert_live_crp_session_model_switch_case(
            provider_id,
            "test-model",
            "medium",
            "test-model",
            "xhigh",
        )
        .await;
    }
}

#[tokio::test]
async fn set_session_model_allows_explicit_model_outside_cached_catalog() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let adapter = Arc::new(RecordingSetModelAdapter::live());
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("fake-set-model".to_string(), adapter.clone());
    let fixture = common::fake_daemon_fixture_with_providers(providers, "http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;
    let base = &server.base_url;
    let client = &server.client;

    let workspace: Workspace = client
        .post(format!("{base}/api/workspaces"))
        .json(&json!({"root_path": repo.path(), "name": "ws"}))
        .send()
        .await
        .expect("create workspace")
        .json()
        .await
        .expect("workspace json");

    fixture
        .daemon
        .seed_host_session_model_catalog_cache_for_test(
            workspace.id,
            "fake-set-model",
            "known-model",
            vec![("known-model".to_string(), "Known Model".to_string())],
        )
        .await;

    let (_task, session) = create_task_with_default_session(
        client,
        base,
        workspace.id,
        "fake-set-model",
        "known-model",
    )
    .await;

    let updated: Session = client
        .post(format!("{base}/api/sessions/{}/model", session.id.0))
        .json(&json!({"model_id":"unknown-model"}))
        .send()
        .await
        .expect("set session model")
        .json()
        .await
        .expect("updated session json");

    assert_eq!(updated.model_id, "unknown-model");
    assert_eq!(
        adapter.calls.lock().expect("calls").as_slice(),
        &[(session.id.0.to_string(), "unknown-model".to_string())]
    );
}
