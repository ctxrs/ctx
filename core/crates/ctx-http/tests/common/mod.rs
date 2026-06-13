#![allow(dead_code)]

use std::collections::HashMap;
use std::fs::File;
use std::fs::OpenOptions;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Method, Request, StatusCode};
use ctx_core::models::{Session, Task, Workspace};
use ctx_daemon::daemon::AppRuntimeFlags;
use ctx_daemon::test_support::TestDaemon;
use ctx_http::api;
use ctx_managed_installs::{
    load_agent_server_config, save_agent_server_config, AgentServerCommand, AgentServerConfigFile,
    ManagedInstallMetadata,
};
use ctx_provider_install::install_state::InstallTarget;
use ctx_providers::adapters::ProviderAdapter;
use ctx_providers::fake::FakeProviderAdapter;
use ctx_store::StoreManager;
use fs2::FileExt;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore, SemaphorePermit};
use tokio::task::JoinHandle;
use tower::ServiceExt;

pub mod crp_fixture_runtime;
pub mod openai_responses_stub;
pub mod updates_failure_safety;

const JJ_MIN_VERSION: (u64, u64, u64) = (0, 25, 0);
// Loaded Bazel slices can starve tiny VCS subprocesses on macOS.
// Keep the helper bounded above observed sandbox latency.
const TEST_VCS_COMMAND_TIMEOUT: Duration = Duration::from_secs(180);

fn copied_test_binary_dir() -> &'static tempfile::TempDir {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| tempfile::tempdir().unwrap())
}

fn vcs_command_gate() -> &'static Semaphore {
    static GATE: OnceLock<Semaphore> = OnceLock::new();
    // Keep helper subprocess fan-out below local ulimit pressure during
    // concurrent integration test startup.
    GATE.get_or_init(|| Semaphore::new(2))
}

struct VcsFileLock {
    file: File,
}

impl Drop for VcsFileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

async fn acquire_vcs_file_lock(lock_name: &'static str) -> VcsFileLock {
    let lock_path = PathBuf::from(format!("/tmp/ctx-http-test-{lock_name}.lock"));
    tokio::task::spawn_blocking(move || {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .unwrap_or_else(|err| panic!("open vcs {lock_name} lock {lock_path:?}: {err}"));
        file.lock_exclusive()
            .unwrap_or_else(|err| panic!("acquire vcs {lock_name} lock {lock_path:?}: {err}"));
        VcsFileLock { file }
    })
    .await
    .unwrap_or_else(|err| panic!("join vcs {lock_name} lock acquisition: {err}"))
}

struct VcsCommandPermit {
    _local: SemaphorePermit<'static>,
    _global: VcsFileLock,
}

async fn acquire_vcs_command_permit() -> VcsCommandPermit {
    VcsCommandPermit {
        _local: vcs_command_gate().acquire().await.unwrap(),
        _global: acquire_vcs_file_lock("command").await,
    }
}

fn resolve_test_path(raw_path: &Path, kind: &str) -> PathBuf {
    let candidate = raw_path.to_path_buf();
    let mut searched: Vec<PathBuf> = Vec::new();

    if candidate.is_absolute() {
        if candidate.exists() {
            return std::fs::canonicalize(&candidate).unwrap_or(candidate);
        }
        searched.push(candidate);
    } else {
        searched.push(candidate.clone());
        for env_key in ["RUNFILES_DIR", "TEST_SRCDIR"] {
            let Some(base) = std::env::var_os(env_key) else {
                continue;
            };
            let base = PathBuf::from(base);
            searched.push(base.join(raw_path));
            if let Some(workspace) = std::env::var_os("TEST_WORKSPACE") {
                searched.push(base.join(workspace).join(raw_path));
            }
            searched.push(base.join("_main").join(raw_path));
        }
    }

    for path in &searched {
        if path.exists() {
            return std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        }
    }

    panic!(
        "failed to resolve {kind} path {raw_path:?}; checked {searched:?}; RUNFILES_DIR={:?}; TEST_SRCDIR={:?}; TEST_WORKSPACE={:?}",
        std::env::var_os("RUNFILES_DIR"),
        std::env::var_os("TEST_SRCDIR"),
        std::env::var_os("TEST_WORKSPACE"),
    );
}

fn maybe_copy_test_binary(resolved: &Path) -> PathBuf {
    if !cfg!(target_os = "macos") {
        return resolved.to_path_buf();
    }
    let resolved_str = resolved.to_string_lossy();
    if !resolved_str.contains("/bazel-out/") && !resolved_str.contains("/bazel-bin/") {
        return resolved.to_path_buf();
    }

    let Some(file_name) = resolved.file_name().and_then(|name| name.to_str()) else {
        return resolved.to_path_buf();
    };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    resolved.hash(&mut hasher);
    let suffix = hasher.finish();
    let copied = copied_test_binary_dir()
        .path()
        .join(format!("{suffix:016x}-{file_name}"));
    if copied.exists() {
        return copied;
    }

    std::fs::copy(resolved, &copied).unwrap_or_else(|err| {
        panic!("failed to copy test binary {resolved:?} to {copied:?}: {err}")
    });
    let permissions = std::fs::metadata(resolved)
        .unwrap_or_else(|err| panic!("failed to stat test binary {resolved:?}: {err}"))
        .permissions();
    std::fs::set_permissions(&copied, permissions).unwrap_or_else(|err| {
        panic!("failed to set copied test binary permissions {copied:?}: {err}")
    });
    copied
}

pub fn resolve_cargo_bin_exe(raw_path: &str) -> PathBuf {
    maybe_copy_test_binary(&resolve_test_path(Path::new(raw_path), "test binary"))
}

pub fn resolve_ctx_mcp_command_for_test() -> PathBuf {
    let raw_path = option_env!("CARGO_BIN_EXE_ctx-mcp").unwrap_or_else(|| {
        panic!(
            "ctx-http CRP integration tests require CARGO_BIN_EXE_ctx-mcp; run them through the Bazel ctx-http test targets"
        )
    });
    resolve_cargo_bin_exe(raw_path)
}

pub fn ctx_mcp_command_env_pair() -> (String, String) {
    (
        "CTX_MCP_COMMAND".to_string(),
        resolve_ctx_mcp_command_for_test()
            .to_string_lossy()
            .to_string(),
    )
}

pub struct TestEnvGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl TestEnvGuard {
    pub fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let prev = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, prev }
    }

    pub fn unset(key: &'static str) -> Self {
        let prev = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, prev }
    }
}

impl Drop for TestEnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.prev.take() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

pub fn set_ctx_mcp_command_env_for_test() -> TestEnvGuard {
    TestEnvGuard::set("CTX_MCP_COMMAND", resolve_ctx_mcp_command_for_test())
}

pub fn process_env_test_lock() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

pub fn resolve_manifest_dir() -> PathBuf {
    resolve_test_path(Path::new(env!("CARGO_MANIFEST_DIR")), "manifest dir")
}

fn parse_jj_version(output: &str) -> Option<(u64, u64, u64)> {
    for token in output.split_whitespace() {
        let token = token.trim_start_matches('v');
        let mut version = String::new();
        let mut saw_digit = false;
        for ch in token.chars() {
            if ch.is_ascii_digit() {
                saw_digit = true;
                version.push(ch);
                continue;
            }
            if ch == '.' && saw_digit {
                version.push(ch);
                continue;
            }
            break;
        }
        if version.is_empty() {
            continue;
        }
        let parts = version.split('.').collect::<Vec<_>>();
        if parts.len() < 2 {
            continue;
        }
        let major = parts[0].parse().ok()?;
        let minor = parts[1].parse().ok()?;
        let patch = parts.get(2).and_then(|part| part.parse().ok()).unwrap_or(0);
        return Some((major, minor, patch));
    }
    None
}

pub async fn init_git_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    run_git(root, &["init"]).await;
    run_git(root, &["config", "user.email", "test@example.com"]).await;
    run_git(root, &["config", "user.name", "Test"]).await;
    for (path, contents) in files {
        let p = root.join(path);
        if let Some(parent) = p.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        tokio::fs::write(p, *contents).await.unwrap();
    }
    run_git(root, &["add", "."]).await;
    run_git(root, &["commit", "-m", "init"]).await;

    dir
}

pub async fn jj_available() -> bool {
    Command::new("jj")
        .arg("--version")
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| parse_jj_version(&String::from_utf8_lossy(&output.stdout)))
        .map(|version| version >= JJ_MIN_VERSION)
        .unwrap_or(false)
}

pub async fn init_jj_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_jj_repo_root(root).await;

    run_git(root, &["config", "user.email", "test@example.com"]).await;
    run_git(root, &["config", "user.name", "Test"]).await;
    for (path, contents) in files {
        let p = root.join(path);
        if let Some(parent) = p.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        tokio::fs::write(p, *contents).await.unwrap();
    }
    run_git(root, &["add", "."]).await;
    run_git(root, &["commit", "-m", "init"]).await;
    run_git(root, &["branch", "-M", "main"]).await;
    run_jj(root, &["git", "import"]).await;

    dir
}

async fn init_jj_repo_root(root: &Path) {
    let candidates: &[&[&str]] = &[
        &["git", "init"],
        &["git", "init", "--colocate"],
        &["init", "--git"],
        &["init", "--git-repo", "."],
    ];
    let mut last_err = None;
    for args in candidates {
        match Command::new("jj")
            .current_dir(root)
            .args(*args)
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                assert!(root.join(".jj").exists());
                return;
            }
            Ok(output) => {
                last_err = Some(String::from_utf8_lossy(&output.stderr).to_string());
            }
            Err(err) => {
                last_err = Some(err.to_string());
            }
        }
    }
    panic!(
        "jj init failed: {}",
        last_err.unwrap_or_else(|| "unknown error".to_string())
    );
}

pub async fn setup_store(data_root: &Path) -> StoreManager {
    StoreManager::open(data_root).await.unwrap()
}

pub async fn seed_managed_codex_cli_host_runtime(data_root: &Path, command_abs_path: &Path) {
    seed_managed_codex_cli_host_runtime_with_args(data_root, command_abs_path, Vec::new()).await;
}

pub async fn seed_managed_codex_cli_host_runtime_with_args(
    data_root: &Path,
    command_abs_path: &Path,
    args: Vec<String>,
) {
    assert!(
        command_abs_path.is_absolute(),
        "codex-cli runtime path must be absolute"
    );
    assert!(
        command_abs_path.exists(),
        "codex-cli runtime path must exist"
    );

    let command = std::fs::canonicalize(command_abs_path)
        .unwrap_or_else(|_| command_abs_path.to_path_buf())
        .to_string_lossy()
        .to_string();
    let meta = ManagedInstallMetadata {
        package: Some("codex-cli".to_string()),
        version: Some("fixture".to_string()),
        artifact_fingerprint: None,
        archive_sha256: None,
        target: Some(InstallTarget::Host),
        install_dir_rel: None,
        bin_dir_rel: None,
        last_success_at: None,
        last_error: None,
    };

    let mut cfg = load_agent_server_config(data_root)
        .await
        .unwrap_or_else(|_| AgentServerConfigFile::default());
    cfg.managed_installs
        .insert("codex-cli".to_string(), meta.clone());
    cfg.managed_provider_targets.insert(
        "codex-cli".to_string(),
        HashMap::from([(
            InstallTarget::Host.as_str().to_string(),
            AgentServerCommand {
                command,
                args,
                dependencies: Vec::new(),
                managed: Some(meta),
            },
        )]),
    );
    save_agent_server_config(data_root, &cfg)
        .await
        .expect("save codex-cli managed runtime");
}

pub fn fake_providers() -> HashMap<String, Arc<dyn ProviderAdapter>> {
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("fake".into(), Arc::new(FakeProviderAdapter::new()));
    providers
}

pub fn build_daemon(
    data_root: impl Into<std::path::PathBuf>,
    stores: StoreManager,
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
    base_url: impl Into<String>,
) -> TestDaemon {
    TestDaemon::new(data_root.into(), stores, providers, base_url.into(), None)
}

pub struct FakeDaemonFixture {
    pub daemon: TestDaemon,
    pub data_dir: tempfile::TempDir,
}

impl FakeDaemonFixture {
    pub fn router(&self) -> axum::Router {
        router_for_daemon(&self.daemon)
    }

    pub async fn spawn_server(&self) -> TestServer {
        spawn_http_server(self.router()).await
    }
}

pub struct DataRootFakeDaemonFixture {
    pub daemon: TestDaemon,
}

impl DataRootFakeDaemonFixture {
    pub fn router(&self) -> axum::Router {
        router_for_daemon(&self.daemon)
    }
}

pub async fn fake_daemon_fixture_for_data_root_with_providers(
    data_root: &Path,
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
    base_url: impl Into<String>,
) -> DataRootFakeDaemonFixture {
    let daemon = TestDaemon::new_with_providers_for_test(
        data_root.to_path_buf(),
        providers,
        base_url.into(),
        None,
    )
    .await
    .expect("create data-root fake-provider daemon");
    DataRootFakeDaemonFixture { daemon }
}

pub async fn fake_daemon_fixture_for_data_root(
    data_root: &Path,
    base_url: impl Into<String>,
) -> DataRootFakeDaemonFixture {
    fake_daemon_fixture_for_data_root_with_providers(data_root, fake_providers(), base_url).await
}

pub struct ProviderInstallDaemonFixture {
    pub daemon: TestDaemon,
}

impl ProviderInstallDaemonFixture {
    pub fn router(&self) -> axum::Router {
        router_for_daemon(&self.daemon)
    }
}

pub async fn provider_install_daemon_fixture_for_data_root_with_providers(
    data_root: &Path,
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
    base_url: impl Into<String>,
) -> ProviderInstallDaemonFixture {
    let daemon = TestDaemon::new_with_providers_for_test(
        data_root.to_path_buf(),
        providers,
        base_url.into(),
        None,
    )
    .await
    .expect("create provider-install daemon fixture");
    ProviderInstallDaemonFixture { daemon }
}

pub async fn reopen_provider_install_daemon_fixture_for_data_root_with_providers(
    data_root: &Path,
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
    base_url: impl Into<String>,
) -> ProviderInstallDaemonFixture {
    provider_install_daemon_fixture_for_data_root_with_providers(data_root, providers, base_url)
        .await
}

pub struct SubagentMcpDaemonFixture {
    pub data_dir: tempfile::TempDir,
    pub daemon: TestDaemon,
    pub server: TestServer,
    pub parent_session: Session,
}

impl SubagentMcpDaemonFixture {
    pub fn parent_id_string(&self) -> String {
        self.parent_session.id.0.to_string()
    }
}

pub async fn subagent_mcp_daemon_fixture_with_providers(
    repo_root: &Path,
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
    base_url: impl Into<String>,
) -> SubagentMcpDaemonFixture {
    let data_dir = tempfile::tempdir().expect("create subagent MCP data root");
    let statuses = providers.clone();
    let daemon = TestDaemon::new_with_providers_for_test(
        data_dir.path().to_path_buf(),
        providers,
        base_url.into(),
        None,
    )
    .await
    .expect("create subagent MCP daemon");
    for (provider_id, provider) in statuses {
        let status = provider
            .inspect()
            .await
            .expect("inspect subagent MCP provider");
        daemon.upsert_provider_status(provider_id, status).await;
    }

    let vcs = ctx_fs::vcs::driver_for_path(repo_root)
        .await
        .expect("subagent MCP repo VCS driver");
    let base_commit = vcs
        .rev_parse_head(repo_root)
        .await
        .expect("subagent MCP repo HEAD");
    let parent_session = daemon
        .seed_mcp_parent_session_for_test(repo_root, base_commit, "fake", "fake-model")
        .await
        .expect("seed subagent MCP parent session");
    let server = spawn_http_server(router_for_daemon(&daemon)).await;

    SubagentMcpDaemonFixture {
        data_dir,
        daemon,
        server,
        parent_session,
    }
}

pub async fn subagent_mcp_daemon_fixture(
    repo_root: &Path,
    base_url: impl Into<String>,
) -> SubagentMcpDaemonFixture {
    subagent_mcp_daemon_fixture_with_providers(repo_root, fake_providers(), base_url).await
}

pub struct ReplayProjectionDaemonFixture {
    pub data_dir: tempfile::TempDir,
    pub daemon: TestDaemon,
    pub server: TestServer,
    pub workspace: Workspace,
    pub task: Task,
    pub session: Session,
}

pub async fn replay_projection_daemon_fixture(
    repo_root: &Path,
    base_url: impl Into<String>,
) -> ReplayProjectionDaemonFixture {
    let fixture = fake_daemon_fixture(base_url).await;
    let server = fixture.spawn_server().await;

    let workspace: Workspace = server
        .client
        .post(format!("{}/api/workspaces", server.base_url))
        .json(&serde_json::json!({
            "root_path": repo_root,
            "name": "projection-fixture",
        }))
        .send()
        .await
        .expect("create replay projection workspace")
        .json()
        .await
        .expect("decode replay projection workspace");

    let task: Task = server
        .client
        .post(format!(
            "{}/api/workspaces/{}/tasks",
            server.base_url, workspace.id.0
        ))
        .json(&serde_json::json!({ "title": "projection-fixture-task" }))
        .send()
        .await
        .expect("create replay projection task")
        .json()
        .await
        .expect("decode replay projection task");

    let session = load_primary_session_http(&server.client, &server.base_url, &task).await;
    fixture.daemon.remember_session_meta(&session).await;

    ReplayProjectionDaemonFixture {
        data_dir: fixture.data_dir,
        daemon: fixture.daemon,
        server,
        workspace,
        task,
        session,
    }
}

pub struct ProviderScenariosOfflineDaemonFixture {
    pub data_dir: tempfile::TempDir,
    pub daemon: TestDaemon,
    pub app: axum::Router,
}

pub async fn provider_scenarios_offline_daemon_fixture(
    data_dir: tempfile::TempDir,
    python: &Path,
    provider_ids: &[&str],
    base_url: impl Into<String>,
) -> ProviderScenariosOfflineDaemonFixture {
    let script_path = crp_fixture_runtime::write_crp_fixture_runtime(data_dir.path());
    seed_managed_codex_cli_host_runtime_with_args(
        data_dir.path(),
        python,
        vec![script_path.to_string_lossy().to_string()],
    )
    .await;
    let providers =
        crp_fixture_runtime::build_crp_fixture_providers(provider_ids, python, &script_path);
    let fixture =
        fake_daemon_fixture_in_data_dir_with_providers(data_dir, providers, base_url).await;
    let app = fixture.router();

    ProviderScenariosOfflineDaemonFixture {
        data_dir: fixture.data_dir,
        daemon: fixture.daemon,
        app,
    }
}

pub async fn fake_daemon_fixture_with_providers(
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
    base_url: impl Into<String>,
) -> FakeDaemonFixture {
    let data_dir = tempfile::tempdir().expect("tempdir");
    fake_daemon_fixture_in_data_dir_with_providers(data_dir, providers, base_url).await
}

pub async fn fake_daemon_fixture_with_providers_and_runtime_flags(
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
    base_url: impl Into<String>,
    runtime_flags: AppRuntimeFlags,
) -> FakeDaemonFixture {
    let data_dir = tempfile::tempdir().expect("tempdir");
    fake_daemon_fixture_in_data_dir_with_providers_and_runtime_flags(
        data_dir,
        providers,
        base_url,
        runtime_flags,
    )
    .await
}

pub async fn fake_daemon_fixture_in_data_dir_with_providers(
    data_dir: tempfile::TempDir,
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
    base_url: impl Into<String>,
) -> FakeDaemonFixture {
    let daemon = TestDaemon::new_with_providers_for_test(
        data_dir.path().to_path_buf(),
        providers,
        base_url.into(),
        None,
    )
    .await
    .expect("create fake-provider daemon");
    FakeDaemonFixture { daemon, data_dir }
}

pub async fn fake_daemon_fixture_in_data_dir_with_providers_and_runtime_flags(
    data_dir: tempfile::TempDir,
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
    base_url: impl Into<String>,
    runtime_flags: AppRuntimeFlags,
) -> FakeDaemonFixture {
    let daemon = TestDaemon::new_with_runtime_flags_for_test(
        data_dir.path().to_path_buf(),
        providers,
        base_url.into(),
        None,
        None,
        runtime_flags,
    )
    .await
    .expect("create runtime-flags fake-provider daemon");
    FakeDaemonFixture { daemon, data_dir }
}

pub async fn fake_daemon_fixture(base_url: impl Into<String>) -> FakeDaemonFixture {
    fake_daemon_fixture_with_providers(fake_providers(), base_url).await
}

pub async fn fake_daemon_fixture_with_runtime_flags(
    base_url: impl Into<String>,
    runtime_flags: AppRuntimeFlags,
) -> FakeDaemonFixture {
    fake_daemon_fixture_with_providers_and_runtime_flags(fake_providers(), base_url, runtime_flags)
        .await
}

pub async fn provider_route_fake_daemon(data_root: &Path) -> TestDaemon {
    TestDaemon::new_with_providers_for_test(
        data_root.to_path_buf(),
        fake_providers(),
        "http://127.0.0.1:0".to_string(),
        None,
    )
    .await
    .expect("create fake-provider daemon")
}

pub async fn spawn_http_server(app: axum::Router) -> TestServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    TestServer {
        base_url: format!("http://{addr}"),
        client: reqwest::Client::new(),
        handle,
        _resource_permit: None,
    }
}

pub struct TestServer {
    pub base_url: String,
    pub client: reqwest::Client,
    handle: JoinHandle<()>,
    _resource_permit: Option<OwnedSemaphorePermit>,
}

impl TestServer {
    pub fn with_resource_permit(mut self, permit: OwnedSemaphorePermit) -> Self {
        self._resource_permit = Some(permit);
        self
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

pub fn router_for_daemon(daemon: &TestDaemon) -> axum::Router {
    api::router(api::RouteHandles::from_daemon_route_handles(
        daemon.route_handles(),
    ))
}

pub async fn oneshot_json<T: DeserializeOwned>(
    app: &axum::Router,
    req: Request<Body>,
) -> (StatusCode, T) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let parsed = serde_json::from_slice(&body).unwrap_or_else(|err| {
        panic!(
            "failed to parse JSON response (status {}): {}\nbody: {}",
            status,
            err,
            String::from_utf8_lossy(&body)
        )
    });
    (status, parsed)
}

pub async fn oneshot_bytes(app: &axum::Router, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (status, body.to_vec())
}

pub async fn json_request<T: DeserializeOwned>(
    app: &axum::Router,
    method: Method,
    uri: impl Into<String>,
    body: Option<Value>,
) -> (StatusCode, T) {
    let req = Request::builder()
        .method(method)
        .uri(uri.into())
        .header("content-type", "application/json")
        .body(Body::from(body.unwrap_or(Value::Null).to_string()))
        .unwrap();
    oneshot_json(app, req).await
}

pub async fn create_workspace(app: &axum::Router, root_path: &Path, name: &str) -> Workspace {
    let (status, ws) = json_request(
        app,
        Method::POST,
        "/api/workspaces",
        Some(serde_json::json!({"root_path": root_path.to_string_lossy(), "name": name})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    ws
}

pub async fn create_task(app: &axum::Router, workspace_id: uuid::Uuid, title: &str) -> Task {
    let (task, _) = create_task_with_session(app, workspace_id, title, "fake", "fake-model").await;
    task
}

/// Creates the production task shape: a task with its one primary/default session.
/// Tests that need extra sessions should create subagents with `create_subagent_session`.
pub async fn create_task_with_session(
    app: &axum::Router,
    workspace_id: uuid::Uuid,
    title: &str,
    provider_id: &str,
    model_id: &str,
) -> (Task, Session) {
    let session_id = uuid::Uuid::new_v4();
    let (status, task): (StatusCode, Task) = json_request(
        app,
        Method::POST,
        format!("/api/workspaces/{workspace_id}/tasks"),
        Some(serde_json::json!({
            "title": title,
            "default_session": {
                "id": session_id.to_string(),
                "provider_id": provider_id,
                "model_id": model_id,
            },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        task.primary_session_id.map(|id| id.0),
        Some(session_id),
        "created task must expose its default session"
    );

    let (status, sessions): (StatusCode, Vec<Session>) = json_request(
        app,
        Method::GET,
        format!("/api/tasks/{}/sessions", task.id.0),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = sessions
        .into_iter()
        .find(|session| session.id.0 == session_id)
        .expect("default session should be listed for task");
    (task, session)
}

pub async fn load_primary_session_http(
    client: &reqwest::Client,
    base: &str,
    task: &Task,
) -> Session {
    let sessions: Vec<Session> = client
        .get(format!("{base}/api/tasks/{}/sessions", task.id.0))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    sessions
        .into_iter()
        .find(|session| Some(session.id) == task.primary_session_id)
        .expect("created task should list its default session")
}

/// Creates an additional child/subagent session. Public HTTP tests should not
/// use the sessions endpoint to create another top-level session.
pub async fn create_subagent_session_http(
    client: &reqwest::Client,
    base: &str,
    task: &Task,
    parent_session_id: uuid::Uuid,
) -> Session {
    client
        .post(format!("{base}/api/tasks/{}/sessions", task.id.0))
        .json(&serde_json::json!({
            "provider_id": "fake",
            "model_id": "fake-model",
            "parent_session_id": parent_session_id.to_string(),
            "relationship": "sub_agent",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// Creates an additional child/subagent session. Public HTTP tests should not
/// use the sessions endpoint to create another top-level session.
pub async fn create_subagent_session(
    app: &axum::Router,
    task_id: uuid::Uuid,
    parent_session_id: uuid::Uuid,
    provider_id: &str,
    model_id: &str,
) -> Session {
    let (status, session) = json_request(
        app,
        Method::POST,
        format!("/api/tasks/{task_id}/sessions"),
        Some(serde_json::json!({
            "provider_id": provider_id,
            "model_id": model_id,
            "parent_session_id": parent_session_id.to_string(),
            "relationship": "sub_agent",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    session
}

pub fn fixed_uuid(seed: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(seed)
}

pub fn fixed_utc(offset_seconds: i64) -> chrono::DateTime<chrono::Utc> {
    let base = chrono::DateTime::from_timestamp(1735689600, 0).unwrap();
    base + chrono::Duration::seconds(offset_seconds)
}

pub async fn run_git(root: &Path, args: &[&str]) {
    let _permit = acquire_vcs_command_permit().await;
    let output = tokio::time::timeout(TEST_VCS_COMMAND_TIMEOUT, git_command(root, args).output())
        .await
        .unwrap_or_else(|_| panic!("git {args:?} timed out after {TEST_VCS_COMMAND_TIMEOUT:?}"))
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

pub async fn run_git_output(root: &Path, args: &[&str]) -> String {
    let _permit = acquire_vcs_command_permit().await;
    let output = tokio::time::timeout(TEST_VCS_COMMAND_TIMEOUT, git_command(root, args).output())
        .await
        .unwrap_or_else(|_| panic!("git {args:?} timed out after {TEST_VCS_COMMAND_TIMEOUT:?}"))
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn git_command(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .kill_on_drop(true)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(root)
        .args(args);
    command
}

pub async fn run_jj_output(root: &Path, args: &[&str]) -> String {
    let output = Command::new("jj")
        .arg("-R")
        .arg(root)
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "jj {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

pub async fn run_jj(root: &Path, args: &[&str]) {
    let _permit = vcs_command_gate().acquire().await.unwrap();
    let output = Command::new("jj")
        .arg("-R")
        .arg(root)
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "jj {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(test)]
mod tests {
    use super::maybe_copy_test_binary;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn maybe_copy_test_binary_only_rehomes_bazel_paths_on_macos() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir
            .path()
            .join("bazel-out/darwin-fastbuild/bin/mock-binary");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&source).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&source, permissions).unwrap();

        let resolved = maybe_copy_test_binary(&source);
        if cfg!(target_os = "macos") {
            assert_ne!(resolved, source);
            assert_eq!(std::fs::read(&resolved).unwrap(), b"#!/bin/sh\nexit 0\n");
            assert_eq!(
                std::fs::metadata(&resolved).unwrap().permissions().mode() & 0o111,
                0o111
            );
        } else {
            assert_eq!(resolved, source);
        }
    }
}
