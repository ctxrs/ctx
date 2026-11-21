//! Canonical indexing policy, including the retained legacy daemon mapping.

pub const INDEXING_MEMORY_MB_MIN: usize = 15;
pub const INDEXING_MEMORY_MB_MAX: usize = 65_536;
pub const INDEXING_WORKERS_MIN: usize = 1;
pub const INDEXING_WORKERS_MAX: usize = 16;

#[derive(Debug, Clone)]
pub struct IndexingConfig {
    pub mode: IndexingMode,
    /// Optional explicit indexing writer memory budget in MiB. When omitted,
    /// indexing uses its built-in default writer memory budget (512 MiB).
    pub memory_mb: Option<usize>,
    /// Optional explicit indexing worker concurrency bound. When omitted,
    /// indexing sizes Tantivy indexer threads and leaf scanners automatically.
    pub workers: Option<usize>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IndexingMode {
    #[default]
    Automatic,
    Manual,
}

impl IndexingMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "auto",
            Self::Manual => "manual",
        }
    }

    pub const fn is_automatic(self) -> bool {
        matches!(self, Self::Automatic)
    }

    pub(super) const fn from_legacy_daemon_enabled(enabled: bool) -> Self {
        if enabled {
            Self::Automatic
        } else {
            Self::Manual
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{env, ffi::OsString, fs, sync::MutexGuard};

    use crate::{
        AppConfig, IndexingMode, CONFIG_FILE, INDEXING_MEMORY_MB_ENV, INDEXING_WORKERS_ENV,
        TEST_LOCAL_USAGE_ENV_LOCK,
    };

    struct IndexingEnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl IndexingEnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let lock = TEST_LOCAL_USAGE_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let saved = keys
                .iter()
                .map(|&key| {
                    let value = env::var_os(key);
                    env::remove_var(key);
                    (key, value)
                })
                .collect();
            Self { _lock: lock, saved }
        }

        fn set(&self, key: &'static str, value: &str) {
            env::set_var(key, value);
        }
    }

    impl Drop for IndexingEnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value {
                    Some(value) => env::set_var(*key, value),
                    None => env::remove_var(*key),
                }
            }
        }
    }

    #[test]
    fn indexing_memory_and_workers_default_to_unset_and_parse_from_config_and_env() {
        let env_guard = IndexingEnvGuard::new(&[INDEXING_MEMORY_MB_ENV, INDEXING_WORKERS_ENV]);
        let data_root = tempfile::tempdir().unwrap();

        let defaults = AppConfig::load(data_root.path()).unwrap();
        assert_eq!(defaults.indexing_memory_mb(), None);
        assert_eq!(defaults.indexing_memory_bytes(), None);
        assert_eq!(defaults.indexing_workers(), None);

        fs::write(
            data_root.path().join(CONFIG_FILE),
            "[indexing]\nmode = \"manual\"\nmemory_mb = 128\nworkers = 2\n",
        )
        .unwrap();
        let configured = AppConfig::load(data_root.path()).unwrap();
        assert_eq!(configured.indexing.mode, IndexingMode::Manual);
        assert_eq!(configured.indexing_memory_mb(), Some(128));
        assert_eq!(configured.indexing_memory_bytes(), Some(128 * 1024 * 1024));
        assert_eq!(configured.indexing_workers(), Some(2));

        env_guard.set(INDEXING_MEMORY_MB_ENV, "64");
        env_guard.set(INDEXING_WORKERS_ENV, "1");
        let overridden = AppConfig::load(data_root.path()).unwrap();
        assert_eq!(overridden.indexing_memory_mb(), Some(64));
        assert_eq!(overridden.indexing_memory_bytes(), Some(64 * 1024 * 1024));
        assert_eq!(overridden.indexing_workers(), Some(1));
    }

    #[test]
    fn indexing_memory_and_workers_reject_invalid_values() {
        let env_guard = IndexingEnvGuard::new(&[INDEXING_MEMORY_MB_ENV, INDEXING_WORKERS_ENV]);

        for invalid in [
            "[indexing]\nmemory_mb = 0\n",
            "[indexing]\nmemory_mb = 14\n",
            "[indexing]\nmemory_mb = 65537\n",
            "[indexing]\nmemory_mb = \"128\"\n",
            "[indexing]\nworkers = 0\n",
            "[indexing]\nworkers = 17\n",
            "[indexing]\nworkers = \"2\"\n",
        ] {
            let data_root = tempfile::tempdir().unwrap();
            fs::write(data_root.path().join(CONFIG_FILE), invalid).unwrap();
            let error = format!("{:#}", AppConfig::load(data_root.path()).unwrap_err());
            assert!(
                error.contains("indexing.memory_mb") || error.contains("indexing.workers"),
                "unexpected error for `{invalid}`: {error}"
            );
        }

        let data_root = tempfile::tempdir().unwrap();
        env_guard.set(INDEXING_MEMORY_MB_ENV, "8");
        let error = format!("{:#}", AppConfig::load(data_root.path()).unwrap_err());
        assert!(error.contains(INDEXING_MEMORY_MB_ENV), "{error}");

        env_guard.set(INDEXING_MEMORY_MB_ENV, "128");
        env_guard.set(INDEXING_WORKERS_ENV, "0");
        let error = format!("{:#}", AppConfig::load(data_root.path()).unwrap_err());
        assert!(error.contains(INDEXING_WORKERS_ENV), "{error}");
    }
}
