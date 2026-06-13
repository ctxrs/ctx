use ctx_harness_sources::{self as harness_sources, HarnessSourceKind};
use ctx_provider_runtime::provider_auth::provider_has_active_auth_config;

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

    fn without(key: &'static str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.prev.as_deref() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[tokio::test]
async fn codex_subscription_selection_requires_real_auth() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let _seed_guard = EnvGuard::without("CTX_SEED_CODEX_AUTH_FROM_HOST");
    let missing_host_auth = tempfile::tempdir()
        .expect("tempdir")
        .path()
        .join("missing-auth.json");
    let _path_guard = EnvGuard::set(
        "CTX_CODEX_HOST_AUTH_PATH",
        missing_host_auth.to_string_lossy().as_ref(),
    );
    let root = tempfile::tempdir().expect("tempdir");
    let source = harness_sources::HarnessProviderSourceConfig {
        provider_id: "codex".to_string(),
        selected_source_kind: HarnessSourceKind::Subscription,
        selected_endpoint_id: None,
        endpoints: vec![],
    };
    let active = provider_has_active_auth_config(root.path(), "codex", Some(&source))
        .await
        .unwrap();
    assert!(!active);
}

#[tokio::test]
async fn codex_subscription_selection_does_not_count_unimported_host_auth_as_active() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let _seed_guard = EnvGuard::without("CTX_SEED_CODEX_AUTH_FROM_HOST");
    let host = tempfile::tempdir().expect("tempdir");
    let host_auth = host.path().join("auth.json");
    tokio::fs::write(
        &host_auth,
        br#"{"tokens":{"access_token":"host-access","refresh_token":"host-refresh"}}"#,
    )
    .await
    .expect("write host auth");
    let _path_guard = EnvGuard::set(
        "CTX_CODEX_HOST_AUTH_PATH",
        host_auth.to_string_lossy().as_ref(),
    );
    let root = tempfile::tempdir().expect("tempdir");
    let source = harness_sources::HarnessProviderSourceConfig {
        provider_id: "codex".to_string(),
        selected_source_kind: HarnessSourceKind::Subscription,
        selected_endpoint_id: None,
        endpoints: vec![],
    };
    let active = provider_has_active_auth_config(root.path(), "codex", Some(&source))
        .await
        .unwrap();
    assert!(!active);
}
