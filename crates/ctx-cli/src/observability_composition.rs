use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    analytics::{AnalyticsDeliveryAuthority, PublicEventV1},
    local_usage::{UsageControlRevision, UsageControlSnapshot},
};
use ctx_app_config::{AppConfig, LocalUsageConfigResolver, LocalUsageConfigState};

const LOCAL_USAGE_DATABASE_FILE: &str = "usage.sqlite";

pub(crate) fn local_usage_storage_authority(
    data_root: &Path,
) -> crate::local_usage::LocalUsageStorageAuthority {
    crate::local_usage::LocalUsageStorageAuthority::new(
        data_root.join(LOCAL_USAGE_DATABASE_FILE),
        env!("CARGO_PKG_VERSION"),
    )
}

const CAPABILITY_CLAIM_FILE: &str = "execution-capabilities-v1.claim";
const CAPABILITY_REPORTED_FILE: &str = "execution-capabilities-v1.reported";
const ANALYTICS_OUTBOX_FILE: &str = "analytics-outbox-v1.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AnalyticsPolicy {
    Purge,
    DryRun,
    Active,
}

enum ResolvedAnalyticsPolicy {
    Purge,
    DryRun,
    Active(AppConfig),
}

const fn analytics_policy_for(enabled: bool, dry_run: bool) -> AnalyticsPolicy {
    if !enabled {
        AnalyticsPolicy::Purge
    } else if dry_run {
        AnalyticsPolicy::DryRun
    } else {
        AnalyticsPolicy::Active
    }
}

fn analytics_policy(config: &AppConfig) -> AnalyticsPolicy {
    analytics_policy_for(
        crate::analytics::effective_analytics_enabled(config),
        std::env::var_os("CTX_ANALYTICS_DRY_RUN").is_some(),
    )
}

fn resolve_analytics_policy(data_root: &Path) -> anyhow::Result<ResolvedAnalyticsPolicy> {
    if ctx_app_config::normalized_analytics_environment_override() == Some(false) {
        return Ok(ResolvedAnalyticsPolicy::Purge);
    }
    let config = AppConfig::load(data_root)?;
    Ok(match analytics_policy(&config) {
        AnalyticsPolicy::Purge => ResolvedAnalyticsPolicy::Purge,
        AnalyticsPolicy::DryRun => ResolvedAnalyticsPolicy::DryRun,
        AnalyticsPolicy::Active => ResolvedAnalyticsPolicy::Active(config),
    })
}

pub(crate) fn append_analytics_batch(
    data_root: &Path,
    events: &[PublicEventV1],
) -> anyhow::Result<()> {
    let outbox_path = crate::identity::device_state_path(ANALYTICS_OUTBOX_FILE, data_root)?;
    match resolve_analytics_policy(data_root)? {
        ResolvedAnalyticsPolicy::Purge => {
            return crate::analytics_outbox::AnalyticsOutbox::purge(&outbox_path)
        }
        ResolvedAnalyticsPolicy::DryRun => return Ok(()),
        ResolvedAnalyticsPolicy::Active(_) if events.is_empty() => return Ok(()),
        ResolvedAnalyticsPolicy::Active(_) => {}
    }
    let client_profile_id = crate::identity::device_id(data_root)?;
    let data_root_id = crate::identity::installation_id(data_root)?;
    let capability_authority =
        crate::execution_capabilities::ExecutionCapabilityStorageAuthority::new(
            crate::identity::device_state_path(CAPABILITY_CLAIM_FILE, data_root)?,
            crate::identity::device_state_path(CAPABILITY_REPORTED_FILE, data_root)?,
        );
    let capability_snapshot = crate::execution_capabilities::pending(
        &capability_authority,
        crate::identity::create_private_file,
    )
    .ok()
    .flatten();
    let install_marker = ctx_upgrade_engine::current_exe_install_marker();
    let mut authority = AnalyticsDeliveryAuthority {
        app_version: env!("CARGO_PKG_VERSION"),
        client_profile_id: &client_profile_id,
        data_root_id: &data_root_id,
        install_attempt_id: install_marker
            .as_ref()
            .map(|marker| marker.install_attempt_id.as_str()),
        capability_snapshot,
    };
    let outbox = crate::analytics_outbox::AnalyticsOutbox::open(outbox_path.clone())?;
    ctx_client_observability::analytics::deliver_batch(&mut authority, events, |body| {
        match resolve_analytics_policy(data_root)? {
            ResolvedAnalyticsPolicy::Purge => {
                crate::analytics_outbox::AnalyticsOutbox::purge(&outbox_path)?;
                anyhow::bail!("analytics was disabled before durable append")
            }
            ResolvedAnalyticsPolicy::DryRun => {
                anyhow::bail!("analytics dry-run was enabled before durable append")
            }
            ResolvedAnalyticsPolicy::Active(current) => {
                outbox.append(&current.analytics.endpoint, body)
            }
        }
    })
}

pub(crate) fn drain_analytics_outbox(data_root: &Path, timeout: Duration) -> anyhow::Result<()> {
    let outbox_path = crate::identity::device_state_path(ANALYTICS_OUTBOX_FILE, data_root)?;
    let config = match resolve_analytics_policy(data_root)? {
        ResolvedAnalyticsPolicy::Purge => {
            return crate::analytics_outbox::AnalyticsOutbox::purge(&outbox_path)
        }
        ResolvedAnalyticsPolicy::DryRun => return Ok(()),
        ResolvedAnalyticsPolicy::Active(config) => config,
    };
    let outbox = crate::analytics_outbox::AnalyticsOutbox::open(outbox_path.clone())?;
    let Some(_uploader) = outbox.try_begin_upload()? else {
        return Ok(());
    };
    let snapshot = outbox.snapshot(&config.analytics.endpoint)?;
    let mut attempted = Vec::with_capacity(snapshot.len());
    for entry in snapshot {
        let current = match resolve_analytics_policy(data_root)? {
            ResolvedAnalyticsPolicy::Purge => {
                return crate::analytics_outbox::AnalyticsOutbox::purge(&outbox_path)
            }
            ResolvedAnalyticsPolicy::DryRun => return Ok(()),
            ResolvedAnalyticsPolicy::Active(current) => current,
        };
        if current.analytics.endpoint != config.analytics.endpoint
            || !outbox.contains_snapshot(&entry)?
        {
            break;
        }
        let disposition = match crate::net::post_telemetry_json_with_timeout(
            &current.analytics.endpoint,
            entry.payload(),
            timeout,
        ) {
            Ok(()) => crate::analytics_outbox::DeliveryDisposition::Accepted,
            Err(error) if error.retryable() => {
                crate::analytics_outbox::DeliveryDisposition::Retry {
                    class: error.class(),
                    retry_after: error.retry_after(),
                }
            }
            Err(error) => crate::analytics_outbox::DeliveryDisposition::Permanent {
                class: error.class(),
            },
        };
        let retry_later = matches!(
            disposition,
            crate::analytics_outbox::DeliveryDisposition::Retry { .. }
        );
        attempted.push((entry, disposition));
        if retry_later {
            break;
        }
    }
    let config = match resolve_analytics_policy(data_root)? {
        ResolvedAnalyticsPolicy::Purge => {
            return crate::analytics_outbox::AnalyticsOutbox::purge(&outbox_path)
        }
        ResolvedAnalyticsPolicy::DryRun => return Ok(()),
        ResolvedAnalyticsPolicy::Active(current) => current,
    };
    outbox.reconcile(&attempted)?;
    queue_pending_delivery_observation(data_root, &config, &outbox)
}

fn queue_pending_delivery_observation(
    data_root: &Path,
    config: &AppConfig,
    outbox: &crate::analytics_outbox::AnalyticsOutbox,
) -> anyhow::Result<()> {
    let Some(observation) = outbox.pending_observation()? else {
        return Ok(());
    };
    let client_profile_id = crate::identity::device_id(data_root)?;
    let data_root_id = crate::identity::installation_id(data_root)?;
    let authority = AnalyticsDeliveryAuthority {
        app_version: env!("CARGO_PKG_VERSION"),
        client_profile_id: &client_profile_id,
        data_root_id: &data_root_id,
        install_attempt_id: None,
        capability_snapshot: None,
    };
    ctx_client_observability::analytics::deliver_delivery_observation(
        &authority,
        observation.event,
        |body| outbox.queue_observation(&config.analytics.endpoint, body, &observation),
    )
}

pub(crate) const fn usage_control_snapshot(enabled: bool) -> UsageControlSnapshot {
    UsageControlSnapshot::unversioned(enabled)
}

fn usage_control_revision(config_path: &Path) -> Option<UsageControlRevision> {
    observe_usage_control_metadata_read();
    match config_path.metadata() {
        Ok(metadata) => UsageControlRevision::from_file_metadata(&metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Some(UsageControlRevision::missing())
        }
        Err(_) => None,
    }
}

#[cfg(test)]
thread_local! {
    static USAGE_CONTROL_METADATA_READ_COUNT: std::cell::Cell<Option<usize>> = const {
        std::cell::Cell::new(None)
    };
}

fn observe_usage_control_metadata_read() {
    #[cfg(test)]
    USAGE_CONTROL_METADATA_READ_COUNT.with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current.saturating_add(1)));
        }
    });
}

#[cfg(test)]
pub(crate) fn count_usage_control_metadata_reads<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    USAGE_CONTROL_METADATA_READ_COUNT.with(|count| {
        let previous = count.replace(Some(0));
        assert!(
            previous.is_none(),
            "usage-control metadata counters must not be nested"
        );
        let result = operation();
        let observed = count.replace(previous).unwrap_or(0);
        (result, observed)
    })
}

fn stable_usage_control_snapshot(
    enabled: bool,
    revision_before: Option<UsageControlRevision>,
    revision_after: Option<UsageControlRevision>,
) -> UsageControlSnapshot {
    let revision = match (revision_before, revision_after) {
        (Some(before), Some(after)) if before == after => Some(after),
        _ => None,
    };
    UsageControlSnapshot::new(enabled, revision)
}

/// Path-aware local-usage policy resolver owned by process composition.
/// Observability receives only path-free snapshots from this authority.
pub(crate) struct LocalUsageControlAuthority {
    data_root: PathBuf,
    resolver: LocalUsageConfigResolver,
    previous: Option<bool>,
}

impl LocalUsageControlAuthority {
    pub(crate) fn new(data_root: PathBuf) -> Self {
        Self {
            data_root,
            resolver: LocalUsageConfigResolver::default(),
            previous: None,
        }
    }

    pub(crate) fn snapshot(&mut self) -> UsageControlSnapshot {
        let config_path = AppConfig::config_path(&self.data_root);
        let before = usage_control_revision(&config_path);
        let resolution = self.resolver.resolve(&self.data_root);
        let available = matches!(resolution.config_state, LocalUsageConfigState::Resolved(_));
        let enabled = resolution.effective_after(self.previous);
        let after = usage_control_revision(&config_path);
        self.previous = Some(enabled);
        let snapshot = stable_usage_control_snapshot(enabled, before, after);
        if available {
            snapshot
        } else {
            UsageControlSnapshot::unavailable(enabled, snapshot.revision().cloned())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, fs, net::TcpListener};

    use crate::analytics::{DaemonOperationV1, OperationCompletedV1, Outcome};

    use super::*;

    struct RestoreEnvironment {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl RestoreEnvironment {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for RestoreEnvironment {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn daemon_event() -> PublicEventV1 {
        PublicEventV1::OperationCompleted(OperationCompletedV1::for_daemon(
            DaemonOperationV1::Status,
            Outcome::Success,
            Duration::ZERO,
        ))
    }

    #[test]
    fn storage_authority_grants_only_the_exact_legacy_database_path() {
        let root = Path::new("/tmp/ctx-observability-authority-test");
        assert_eq!(
            local_usage_storage_authority(root).database_path(),
            root.join("usage.sqlite")
        );
    }

    #[test]
    fn opt_out_precedes_dry_run_policy() {
        assert_eq!(analytics_policy_for(false, true), AnalyticsPolicy::Purge);
        assert_eq!(analytics_policy_for(true, true), AnalyticsPolicy::DryRun);
        assert_eq!(analytics_policy_for(true, false), AnalyticsPolicy::Active);
    }

    #[test]
    fn foreground_append_opens_no_network_connection_and_dry_run_creates_no_backlog() {
        let _env_lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let data_root = tempfile::tempdir().unwrap();
        let device_root = tempfile::tempdir().unwrap();
        let _state = RestoreEnvironment::set("XDG_STATE_HOME", device_root.path());
        let _home = RestoreEnvironment::set("HOME", device_root.path());
        let _enabled = RestoreEnvironment::set("CTX_ANALYTICS_ENABLED", "true");
        let _dry_run = RestoreEnvironment::remove("CTX_ANALYTICS_DRY_RUN");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/events", listener.local_addr().unwrap());
        let _endpoint = RestoreEnvironment::set("CTX_ANALYTICS_ENDPOINT", &endpoint);
        append_analytics_batch(data_root.path(), &[daemon_event()]).unwrap();

        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        let path =
            crate::identity::device_state_path(ANALYTICS_OUTBOX_FILE, data_root.path()).unwrap();
        let outbox = crate::analytics_outbox::AnalyticsOutbox::open(path.clone()).unwrap();
        assert_eq!(outbox.snapshot(&endpoint).unwrap().len(), 1);

        let _dry_run = RestoreEnvironment::set("CTX_ANALYTICS_DRY_RUN", "1");
        crate::analytics_outbox::AnalyticsOutbox::purge(&path).unwrap();
        append_analytics_batch(data_root.path(), &[daemon_event()]).unwrap();
        assert!(!path.exists());

        drop(_enabled);
        let _disabled = RestoreEnvironment::set("CTX_ANALYTICS_ENABLED", "false");
        fs::write(&path, b"must be purged").unwrap();
        append_analytics_batch(data_root.path(), &[daemon_event()]).unwrap();
        assert!(!path.exists(), "opt-out must purge even during dry-run");
    }
}
