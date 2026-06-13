use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const CODEX_CONTINUITY_RUNTIME_LOCK_FILE: &str = ".ctx-continuity-runtime.lock";

pub(super) struct CodexRuntimeLocks {
    _continuity: File,
}

fn codex_home_from_env() -> Option<PathBuf> {
    std::env::var("CODEX_HOME")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub(super) fn acquire_codex_runtime_locks() -> Result<Option<CodexRuntimeLocks>> {
    let Some(codex_home) = codex_home_from_env() else {
        return Ok(None);
    };
    std::fs::create_dir_all(&codex_home)
        .with_context(|| format!("creating Codex home at {}", codex_home.display()))?;
    let continuity = acquire_codex_continuity_runtime_lock(&codex_home)?;
    Ok(Some(CodexRuntimeLocks {
        _continuity: continuity,
    }))
}

fn acquire_codex_continuity_runtime_lock(codex_home: &Path) -> Result<File> {
    let lock_path = codex_home.join(CODEX_CONTINUITY_RUNTIME_LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| {
            format!(
                "opening Codex continuity runtime lock {}",
                lock_path.display()
            )
        })?;
    match fs2::FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(file),
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
            anyhow::bail!(
                "Codex home {} is undergoing continuity migration. Retry after launch preparation finishes.",
                codex_home.display()
            )
        }
        Err(err) => Err(err).with_context(|| {
            format!(
                "locking Codex continuity runtime lock {}",
                lock_path.display()
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::acquire_codex_runtime_locks;

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
            if let Some(prev) = self.prev.as_deref() {
                std::env::set_var(self.key, prev);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[tokio::test]
    async fn refresh_capable_home_uses_continuity_lock_without_ctx_refresh_lock() {
        let _lock = crate::test_env_lock().lock().await;
        let tempdir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tempdir.path().join("auth.json"),
            r#"{"tokens":{"access_token":"access","refresh_token":"refresh"}}"#,
        )
        .expect("write auth");
        let _guard = EnvGuard::set("CODEX_HOME", tempdir.path().to_string_lossy().as_ref());

        let locks = acquire_codex_runtime_locks()
            .expect("refresh-capable broker home runtime lock")
            .expect("runtime locks");
        assert!(tempdir.path().join(".ctx-continuity-runtime.lock").exists());
        assert!(!tempdir.path().join(".ctx-refresh-token.lock").exists());
        drop(locks);
    }

    #[tokio::test]
    async fn runtime_lock_is_taken_for_api_key_home_without_oauth_authority_lock() {
        let _lock = crate::test_env_lock().lock().await;
        let tempdir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tempdir.path().join("auth.json"),
            r#"{"OPENAI_API_KEY":"key"}"#,
        )
        .expect("write auth");
        let _guard = EnvGuard::set("CODEX_HOME", tempdir.path().to_string_lossy().as_ref());

        let first = acquire_codex_runtime_locks()
            .expect("api key home runtime lock")
            .expect("runtime locks");
        assert!(tempdir.path().join(".ctx-continuity-runtime.lock").exists());
        assert!(!tempdir.path().join(".ctx-refresh-token.lock").exists());
        drop(first);
    }

    #[tokio::test]
    async fn runtime_lock_accepts_access_token_only_oauth_home() {
        let _lock = crate::test_env_lock().lock().await;
        let tempdir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tempdir.path().join("auth.json"),
            r#"{"tokens":{"access_token":"access","account_id":"acct"}}"#,
        )
        .expect("write auth");
        let _guard = EnvGuard::set("CODEX_HOME", tempdir.path().to_string_lossy().as_ref());

        let first = acquire_codex_runtime_locks()
            .expect("access-only home runtime lock")
            .expect("runtime locks");
        assert!(tempdir.path().join(".ctx-continuity-runtime.lock").exists());
        assert!(!tempdir.path().join(".ctx-refresh-token.lock").exists());
        drop(first);
    }
}
