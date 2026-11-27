//! Canonical indexing policy, including the retained legacy daemon mapping.

pub const INDEXING_MEMORY_MB_MIN: usize = 15;
pub const INDEXING_MEMORY_MB_MAX: usize = 65_536;
pub const INDEXING_WORKERS_MIN: usize = 1;
pub const INDEXING_WORKERS_MAX: usize = 16;
pub const INDEXING_INTERVAL_MINUTES_MIN: u64 = 1;
pub const INDEXING_INTERVAL_MINUTES_MAX: u64 = 24 * 60;
/// Refresh interval used by `scheduled` mode when none is configured.
pub const INDEXING_INTERVAL_MINUTES_DEFAULT: u64 = 10;

#[derive(Debug, Clone)]
pub struct IndexingConfig {
    pub mode: IndexingMode,
    /// Optional explicit indexing writer memory budget in MiB. When omitted,
    /// indexing uses its built-in default writer memory budget (512 MiB).
    pub memory_mb: Option<usize>,
    /// Optional explicit indexing worker concurrency bound. When omitted,
    /// indexing sizes Tantivy indexer threads and leaf scanners automatically.
    pub workers: Option<usize>,
    /// Optional refresh interval for `scheduled` mode, in minutes. Ignored by
    /// the other modes; `scheduled` falls back to
    /// [`INDEXING_INTERVAL_MINUTES_DEFAULT`].
    pub interval_minutes: Option<u32>,
}

impl IndexingConfig {
    /// The periodic incremental refresh interval when this configuration runs
    /// the daemon without filesystem watching.
    pub fn scheduled_interval(&self) -> Option<std::time::Duration> {
        (self.mode == IndexingMode::Scheduled).then(|| {
            std::time::Duration::from_secs(
                self.interval_minutes
                    .map_or(INDEXING_INTERVAL_MINUTES_DEFAULT, u64::from)
                    .saturating_mul(60),
            )
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IndexingMode {
    #[default]
    Automatic,
    Manual,
    /// The daemon stays running without filesystem watching and performs an
    /// incremental refresh of all history every configured interval.
    Scheduled,
}

impl IndexingMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "auto",
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
        }
    }

    pub const fn is_automatic(self) -> bool {
        matches!(self, Self::Automatic)
    }

    /// Whether a long-lived background daemon maintains the index.
    pub const fn runs_daemon(self) -> bool {
        matches!(self, Self::Automatic | Self::Scheduled)
    }

    /// Whether the daemon reacts to filesystem events. `scheduled` refreshes
    /// on a timer instead.
    pub const fn watches_filesystem(self) -> bool {
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

    #[test]
    fn scheduled_mode_runs_daemon_without_watching_and_defaults_interval() {
        let _env = IndexingEnvGuard::new(&["CTX_DAEMON_ENABLED"]);
        let data_root = tempfile::tempdir().unwrap();
        fs::write(
            data_root.path().join(CONFIG_FILE),
            "[indexing]\nmode = \"scheduled\"\n",
        )
        .unwrap();

        let config = AppConfig::load(data_root.path()).unwrap();

        assert_eq!(config.indexing.mode, IndexingMode::Scheduled);
        assert_eq!(config.indexing.mode.as_str(), "scheduled");
        assert!(config.indexing.mode.runs_daemon());
        assert!(!config.indexing.mode.watches_filesystem());
        assert!(config.automatic_indexing_enabled());
        assert_eq!(
            config.indexing.scheduled_interval(),
            Some(std::time::Duration::from_secs(
                crate::INDEXING_INTERVAL_MINUTES_DEFAULT * 60
            ))
        );
        assert!(IndexingMode::Automatic.watches_filesystem());
        assert!(!IndexingMode::Manual.runs_daemon());
    }

    #[test]
    fn scheduled_interval_parses_and_is_ignored_by_other_modes() {
        let _env = IndexingEnvGuard::new(&["CTX_DAEMON_ENABLED"]);
        let data_root = tempfile::tempdir().unwrap();
        fs::write(
            data_root.path().join(CONFIG_FILE),
            "[indexing]\nmode = \"scheduled\"\ninterval_minutes = 25\n",
        )
        .unwrap();
        let config = AppConfig::load(data_root.path()).unwrap();
        assert_eq!(config.indexing.interval_minutes, Some(25));
        assert_eq!(
            config.indexing.scheduled_interval(),
            Some(std::time::Duration::from_secs(25 * 60))
        );

        fs::write(
            data_root.path().join(CONFIG_FILE),
            "[indexing]\nmode = \"auto\"\ninterval_minutes = 25\n",
        )
        .unwrap();
        let config = AppConfig::load(data_root.path()).unwrap();
        assert_eq!(config.indexing.scheduled_interval(), None);
    }

    #[test]
    fn scheduled_interval_rejects_out_of_range_values() {
        let _env = IndexingEnvGuard::new(&["CTX_DAEMON_ENABLED"]);
        for value in ["0", "1441", "-1", "ten"] {
            let data_root = tempfile::tempdir().unwrap();
            fs::write(
                data_root.path().join(CONFIG_FILE),
                format!("[indexing]\nmode = \"scheduled\"\ninterval_minutes = {value}\n"),
            )
            .unwrap();
            let error = format!("{:#}", AppConfig::load(data_root.path()).unwrap_err());
            assert!(
                error.contains("indexing.interval_minutes"),
                "{value}: {error}"
            );
        }
    }

    #[test]
    fn daemon_enabled_environment_keeps_scheduled_mode() {
        let env = IndexingEnvGuard::new(&["CTX_DAEMON_ENABLED"]);
        let data_root = tempfile::tempdir().unwrap();
        fs::write(
            data_root.path().join(CONFIG_FILE),
            "[indexing]\nmode = \"scheduled\"\n",
        )
        .unwrap();

        env.set("CTX_DAEMON_ENABLED", "true");
        assert_eq!(
            AppConfig::load(data_root.path()).unwrap().indexing.mode,
            IndexingMode::Scheduled
        );
        env.set("CTX_DAEMON_ENABLED", "false");
        assert_eq!(
            AppConfig::load(data_root.path()).unwrap().indexing.mode,
            IndexingMode::Manual
        );
    }

    #[test]
    fn scheduled_mode_and_interval_persist_through_mutations() {
        let _env = IndexingEnvGuard::new(&["CTX_DAEMON_ENABLED"]);
        let data_root = tempfile::tempdir().unwrap();

        crate::set_indexing_interval_minutes(data_root.path(), 15).unwrap();
        crate::set_indexing_mode(data_root.path(), IndexingMode::Scheduled).unwrap();

        assert_eq!(
            crate::persisted_indexing_mode(data_root.path()).unwrap(),
            IndexingMode::Scheduled
        );
        assert!(crate::persisted_daemon_enabled(data_root.path()).unwrap());
        let config = AppConfig::load(data_root.path()).unwrap();
        assert_eq!(config.indexing.interval_minutes, Some(15));
        assert!(crate::set_indexing_interval_minutes(data_root.path(), 0).is_err());

        crate::set_daemon_enabled(data_root.path(), false).unwrap();
        assert_eq!(
            crate::persisted_indexing_mode(data_root.path()).unwrap(),
            IndexingMode::Manual
        );
        assert_eq!(
            AppConfig::load(data_root.path())
                .unwrap()
                .indexing
                .interval_minutes,
            Some(15)
        );
    }
}
