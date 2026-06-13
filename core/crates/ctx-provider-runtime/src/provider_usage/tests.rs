use super::*;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(prev) = self.prev.as_deref() {
                std::env::set_var(self.key, prev);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

struct TestUsageHost {
    data_root: PathBuf,
    provider_runtime: ProviderRuntime,
    shutdown_tx: broadcast::Sender<()>,
}

impl TestUsageHost {
    fn new(data_root: PathBuf) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        Self {
            data_root,
            provider_runtime: ProviderRuntime::new(HashMap::new()),
            shutdown_tx,
        }
    }
}

impl ProviderUsageHost for TestUsageHost {
    fn data_root(&self) -> &Path {
        &self.data_root
    }

    fn provider_runtime(&self) -> &ProviderRuntime {
        &self.provider_runtime
    }

    fn subscribe_shutdown(&self) -> broadcast::Receiver<()> {
        self.shutdown_tx.subscribe()
    }
}

fn hold_exclusive_codex_runtime_lock(home: &Path) -> std::fs::File {
    std::fs::create_dir_all(home).expect("create runtime home");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join(".ctx-continuity-runtime.lock"))
        .expect("open continuity lock");
    fs2::FileExt::try_lock_exclusive(&lock).expect("exclusive continuity lock");
    lock
}

#[tokio::test]
async fn refresh_provider_usage_surfaces_agent_server_config_errors() {
    let _env_lock = ENV_LOCK.lock().expect("provider usage env lock");
    let data_root = tempfile::tempdir().expect("tempdir");
    let runtime_home = tempfile::tempdir().expect("runtime home");
    let _codex_home = EnvVarGuard::set("CTX_CODEX_HOME", &runtime_home.path().to_string_lossy());

    let config_path = data_root
        .path()
        .join("providers")
        .join("agent-servers")
        .join("agent_servers.json");
    std::fs::create_dir_all(config_path.parent().expect("config parent")).expect("mkdir");
    std::fs::write(&config_path, "{ not valid json").expect("write invalid config");

    let host = TestUsageHost::new(data_root.path().to_path_buf());
    let err = refresh_provider_usage(&host)
        .await
        .expect_err("invalid managed config should fail usage refresh");
    assert!(err.to_string().contains("loading agent server config"));
}

#[tokio::test]
async fn refresh_provider_usage_replaces_stale_cache_with_error_snapshot_on_config_error() {
    let _env_lock = ENV_LOCK.lock().expect("provider usage env lock");
    let data_root = tempfile::tempdir().expect("tempdir");
    let runtime_home = tempfile::tempdir().expect("runtime home");
    let _codex_home = EnvVarGuard::set("CTX_CODEX_HOME", &runtime_home.path().to_string_lossy());

    let config_path = data_root
        .path()
        .join("providers")
        .join("agent-servers")
        .join("agent_servers.json");
    std::fs::create_dir_all(config_path.parent().expect("config parent")).expect("mkdir");
    std::fs::write(&config_path, "{ not valid json").expect("write invalid config");

    let host = TestUsageHost::new(data_root.path().to_path_buf());
    host.provider_runtime
        .with_provider_usage_cache(|cache| {
            cache.insert(
                "codex".to_string(),
                ProviderUsageSnapshot {
                    provider_id: "codex".to_string(),
                    source: "oauth".to_string(),
                    fetched_at: Utc::now(),
                    payload: Some(serde_json::json!({"cached": true})),
                    error: None,
                },
            );
        })
        .await;

    let err = refresh_provider_usage(&host)
        .await
        .expect_err("invalid managed config should fail usage refresh");
    assert!(err.to_string().contains("loading agent server config"));

    let snapshot = host
        .provider_runtime
        .with_provider_usage_cache(|cache| cache.get("codex").cloned())
        .await
        .expect("usage cache entry should be replaced with an error snapshot");
    assert_eq!(snapshot.source, "error");
    assert!(snapshot.payload.is_none());
    assert!(
        snapshot
            .error
            .as_deref()
            .is_some_and(|value| value.contains("loading agent server config")),
        "expected managed config error snapshot: {snapshot:?}"
    );
}

#[tokio::test]
async fn codex_oauth_usage_error_does_not_spawn_rpc_fallback() {
    let runtime_home = tempfile::tempdir().expect("runtime home");
    tokio::fs::write(
        runtime_home.path().join("auth.json"),
        serde_json::json!({
            "tokens": {
                "access_token": "stale-access",
                "refresh_token": "refresh-token",
                "account_id": "acct-1"
            }
        })
        .to_string(),
    )
    .await
    .expect("write auth.json");
    tokio::fs::write(
        runtime_home.path().join("config.toml"),
        "chatgpt_base_url = \"http://127.0.0.1:9\"",
    )
    .await
    .expect("write config.toml");

    let snapshot = fetch_codex_usage_snapshot(HashMap::from([(
        "CODEX_HOME".to_string(),
        runtime_home.path().to_string_lossy().to_string(),
    )]))
    .await
    .expect("usage snapshot should be represented as an error snapshot");

    assert_eq!(snapshot.source, "error");
    let error = snapshot.error.as_deref().expect("usage error");
    assert!(
        error.contains("will not refresh Codex OAuth tokens"),
        "unexpected error: {error}"
    );
    assert!(
        !error.contains("CTX_CODEX_BIN_PATH"),
        "OAuth usage must not spawn a separate Codex app-server fallback: {error}"
    );
}

#[tokio::test]
async fn codex_usage_missing_auth_returns_error_snapshot() {
    let runtime_home = tempfile::tempdir().expect("runtime home");

    let snapshot = fetch_codex_usage_snapshot(HashMap::from([(
        "CODEX_HOME".to_string(),
        runtime_home.path().to_string_lossy().to_string(),
    )]))
    .await
    .expect("missing auth should be represented as an error snapshot");

    assert_eq!(snapshot.source, "error");
    assert!(snapshot.payload.is_none());
    let error = snapshot.error.as_deref().expect("usage error");
    assert!(
        error.contains("missing codex auth.json"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn codex_api_key_usage_takes_continuity_lock_before_app_server_spawn() {
    let runtime_home = tempfile::tempdir().expect("runtime home");
    tokio::fs::write(
        runtime_home.path().join("auth.json"),
        serde_json::json!({"OPENAI_API_KEY": "sk-test"}).to_string(),
    )
    .await
    .expect("write auth.json");
    let _lock = hold_exclusive_codex_runtime_lock(runtime_home.path());
    let codex_bin = std::env::current_exe().expect("current executable");

    let snapshot = fetch_codex_usage_snapshot(HashMap::from([
        (
            "CODEX_HOME".to_string(),
            runtime_home.path().to_string_lossy().to_string(),
        ),
        (
            "CTX_CODEX_BIN_PATH".to_string(),
            codex_bin.to_string_lossy().to_string(),
        ),
    ]))
    .await
    .expect("usage snapshot should represent lock contention as an error snapshot");

    assert_eq!(snapshot.source, "error");
    let error = snapshot.error.as_deref().expect("usage error");
    assert!(
        error.contains("undergoing continuity migration"),
        "usage poll must take the shared runtime lock before spawning app-server, got {error}"
    );
}
