use super::*;

pub(super) struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    pub(super) fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }

    pub(super) fn remove(key: &'static str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            std::env::set_var(self.key, prev);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

pub(super) fn sandbox_cli_env_test_lock() -> &'static tokio::sync::Mutex<()> {
    crate::test_support::sandbox_cli_env_test_lock()
}

fn daemon_public_base_url_env_test_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[test]
fn daemon_public_base_url_from_env_accepts_http_or_https_origin_with_path_prefix() {
    let _guard = daemon_public_base_url_env_test_lock()
        .lock()
        .expect("daemon public base url env test lock");
    let _var = EnvVarGuard::set("CTX_DAEMON_PUBLIC_BASE_URL", "https://proxy.example/ctx/");
    assert_eq!(
        daemon_public_base_url_from_env().unwrap(),
        Some("https://proxy.example/ctx".to_string())
    );
}

#[test]
fn daemon_public_base_url_from_env_rejects_credentials_and_query_fragments() {
    let _guard = daemon_public_base_url_env_test_lock()
        .lock()
        .expect("daemon public base url env test lock");
    let _var = EnvVarGuard::set(
        "CTX_DAEMON_PUBLIC_BASE_URL",
        "https://user@example.com/ctx?a=1",
    );
    let err = daemon_public_base_url_from_env().unwrap_err();
    assert!(
        err.to_string().contains("must not embed credentials")
            || err
                .to_string()
                .contains("must not include query or fragment")
    );
}

#[test]
fn daemon_public_base_url_from_env_treats_absent_var_as_none() {
    let _guard = daemon_public_base_url_env_test_lock()
        .lock()
        .expect("daemon public base url env test lock");
    let _var = EnvVarGuard::remove("CTX_DAEMON_PUBLIC_BASE_URL");
    assert_eq!(daemon_public_base_url_from_env().unwrap(), None);
}
