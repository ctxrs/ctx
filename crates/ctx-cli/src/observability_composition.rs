use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
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
    let config = AppConfig::load_read_only(data_root)?;
    Ok(match analytics_policy(&config) {
        AnalyticsPolicy::Purge => ResolvedAnalyticsPolicy::Purge,
        AnalyticsPolicy::DryRun => ResolvedAnalyticsPolicy::DryRun,
        AnalyticsPolicy::Active => ResolvedAnalyticsPolicy::Active(config),
    })
}

fn resolve_analytics_policy_for_owner(
    data_root: &Path,
    data_root_id: &str,
) -> anyhow::Result<ResolvedAnalyticsPolicy> {
    let policy = resolve_analytics_policy(data_root)?;
    if crate::identity::existing_installation_id(data_root)?.as_deref() != Some(data_root_id) {
        anyhow::bail!("analytics consent owner is no longer available at this data root");
    }
    Ok(policy)
}

fn purge_analytics_outbox(data_root: &Path, outbox_path: &Path) -> anyhow::Result<()> {
    let data_root_id = crate::identity::existing_installation_id(data_root)?;
    crate::analytics_outbox::AnalyticsOutbox::purge(outbox_path, data_root_id.as_deref())
}

pub(crate) fn optional_analytics_enabled(data_root: &Path) -> bool {
    matches!(
        resolve_analytics_policy(data_root),
        Ok(ResolvedAnalyticsPolicy::Active(_))
    )
}

pub(crate) fn optional_analytics_endpoint(data_root: &Path) -> Option<String> {
    match resolve_analytics_policy(data_root).ok()? {
        ResolvedAnalyticsPolicy::Active(config) => Some(config.analytics.endpoint),
        ResolvedAnalyticsPolicy::Purge | ResolvedAnalyticsPolicy::DryRun => None,
    }
}

fn optional_policy_for_owner(
    data_root: &Path,
    owner: &str,
) -> anyhow::Result<ResolvedAnalyticsPolicy> {
    let policy = resolve_analytics_policy(data_root)?;
    if crate::identity::try_existing_installation_id(data_root)?.as_deref() != Some(owner) {
        anyhow::bail!("optional analytics consent owner is unavailable");
    }
    Ok(policy)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AppendMode {
    Ordinary,
    Optional,
    Summary,
}

pub(crate) fn append_optional_analytics_batch(
    data_root: &Path,
    events: &[PublicEventV1],
) -> anyhow::Result<()> {
    append_batch(data_root, events, AppendMode::Optional, None)
}

pub(crate) fn append_optional_analytics_batch_for_owner(
    data_root: &Path,
    owner: &str,
    endpoint: &str,
    events: &[PublicEventV1],
) -> anyhow::Result<()> {
    append_batch(
        data_root,
        events,
        AppendMode::Optional,
        Some((owner, endpoint)),
    )
}

pub(crate) fn append_analytics_summary_for_owner(
    data_root: &Path,
    owner: &str,
    endpoint: &str,
    events: &[PublicEventV1],
) -> anyhow::Result<()> {
    append_batch(
        data_root,
        events,
        AppendMode::Summary,
        Some((owner, endpoint)),
    )
}

pub(crate) fn append_analytics_batch(
    data_root: &Path,
    events: &[PublicEventV1],
) -> anyhow::Result<()> {
    append_batch(data_root, events, AppendMode::Ordinary, None)
}

fn append_batch(
    data_root: &Path,
    events: &[PublicEventV1],
    mode: AppendMode,
    expected: Option<(&str, &str)>,
) -> anyhow::Result<()> {
    let optional = mode != AppendMode::Ordinary;
    let outbox_path = crate::identity::device_state_path(ANALYTICS_OUTBOX_FILE, data_root)?;
    match resolve_analytics_policy(data_root)? {
        ResolvedAnalyticsPolicy::Purge if optional => {
            let owner = match expected {
                Some((owner, _)) => Some(owner.to_owned()),
                None => crate::identity::try_existing_installation_id(data_root)?,
            };
            return crate::analytics_outbox::AnalyticsOutbox::try_purge(
                &outbox_path,
                owner.as_deref(),
            );
        }
        ResolvedAnalyticsPolicy::Purge => return purge_analytics_outbox(data_root, &outbox_path),
        ResolvedAnalyticsPolicy::DryRun => return Ok(()),
        ResolvedAnalyticsPolicy::Active(_) if events.is_empty() => return Ok(()),
        ResolvedAnalyticsPolicy::Active(_) => {}
    }
    let (client_profile_id, data_root_id) = if optional {
        let owner = if expected.is_some() {
            crate::identity::try_existing_installation_id(data_root)?
        } else {
            crate::identity::try_installation_id(data_root)?
        };
        let Some(owner) = owner else {
            return Ok(());
        };
        if expected.is_some_and(|(wanted, _)| wanted != owner) {
            return Ok(());
        }
        let Some(profile) = crate::identity::try_device_id(data_root)? else {
            return Ok(());
        };
        (profile, owner)
    } else {
        (
            crate::identity::device_id(data_root)?,
            crate::identity::installation_id(data_root)?,
        )
    };
    let capability_snapshot = if optional {
        None
    } else {
        let capability_authority =
            crate::execution_capabilities::ExecutionCapabilityStorageAuthority::new(
                crate::identity::device_state_path(CAPABILITY_CLAIM_FILE, data_root)?,
                crate::identity::device_state_path(CAPABILITY_REPORTED_FILE, data_root)?,
            );
        crate::execution_capabilities::pending(
            &capability_authority,
            crate::identity::create_private_file,
        )
        .ok()
        .flatten()
    };
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
    let outbox = if optional {
        let Some(outbox) =
            crate::analytics_outbox::AnalyticsOutbox::try_open(outbox_path.clone(), &data_root_id)?
        else {
            return Ok(());
        };
        outbox
    } else {
        crate::analytics_outbox::AnalyticsOutbox::open(outbox_path.clone(), &data_root_id)?
    };
    ctx_client_observability::analytics::deliver_batch(&mut authority, events, |body| {
        let policy = if optional {
            optional_policy_for_owner(data_root, &data_root_id)?
        } else {
            resolve_analytics_policy_for_owner(data_root, &data_root_id)?
        };
        match policy {
            ResolvedAnalyticsPolicy::Purge => {
                if optional {
                    crate::analytics_outbox::AnalyticsOutbox::try_purge(
                        &outbox_path,
                        Some(&data_root_id),
                    )?;
                } else {
                    crate::analytics_outbox::AnalyticsOutbox::purge(
                        &outbox_path,
                        Some(&data_root_id),
                    )?;
                }
                anyhow::bail!("analytics was disabled before durable append")
            }
            ResolvedAnalyticsPolicy::DryRun => {
                anyhow::bail!("analytics dry-run was enabled before durable append")
            }
            ResolvedAnalyticsPolicy::Active(current) => {
                if expected.is_some_and(|(_, endpoint)| endpoint != current.analytics.endpoint) {
                    return Ok(());
                }
                if mode == AppendMode::Summary {
                    let _admitted = outbox.append_summary(&current.analytics.endpoint, body)?;
                    Ok(())
                } else {
                    outbox.append(&current.analytics.endpoint, body)
                }
            }
        }
    })
}

pub(crate) fn drain_analytics_outbox(data_root: &Path, timeout: Duration) -> anyhow::Result<()> {
    let started = Instant::now();
    let Some(owner) = crate::identity::try_existing_installation_id(data_root)? else {
        return Ok(());
    };
    drain_for_owner(data_root, &owner, timeout, started)
}

pub(crate) fn drain_analytics_outbox_for_owner(
    data_root: &Path,
    owner: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    drain_for_owner(data_root, owner, timeout, Instant::now())
}

fn drain_for_owner(
    data_root: &Path,
    data_root_id: &str,
    timeout: Duration,
    started: Instant,
) -> anyhow::Result<()> {
    let outbox_path = crate::identity::device_state_path(ANALYTICS_OUTBOX_FILE, data_root)?;
    let config = match optional_policy_for_owner(data_root, data_root_id)? {
        ResolvedAnalyticsPolicy::Purge => {
            return crate::analytics_outbox::AnalyticsOutbox::try_purge(
                &outbox_path,
                Some(data_root_id),
            )
        }
        ResolvedAnalyticsPolicy::DryRun => return Ok(()),
        ResolvedAnalyticsPolicy::Active(config) => config,
    };
    let Some(outbox) =
        crate::analytics_outbox::AnalyticsOutbox::try_open(outbox_path.clone(), data_root_id)?
    else {
        return Ok(());
    };
    let Some(_uploader) = outbox.try_begin_upload()? else {
        return Ok(());
    };
    let snapshot = outbox.snapshot(&config.analytics.endpoint)?;
    let mut attempted = Vec::with_capacity(snapshot.len());
    for entry in snapshot {
        let current = match optional_policy_for_owner(data_root, data_root_id)? {
            ResolvedAnalyticsPolicy::Purge => {
                return crate::analytics_outbox::AnalyticsOutbox::try_purge(
                    &outbox_path,
                    Some(data_root_id),
                )
            }
            ResolvedAnalyticsPolicy::DryRun => return Ok(()),
            ResolvedAnalyticsPolicy::Active(current) => current,
        };
        if current.analytics.endpoint != config.analytics.endpoint
            || !outbox.contains_snapshot(&entry)?
        {
            break;
        }
        let Some(remaining) = timeout
            .checked_sub(started.elapsed())
            .filter(|value| !value.is_zero())
        else {
            break;
        };
        let disposition = match crate::net::post_telemetry_json_with_timeout(
            &current.analytics.endpoint,
            entry.payload(),
            remaining,
        ) {
            Ok(()) => crate::analytics_outbox::DeliveryDisposition::Accepted,
            Err(error) if error.retryable() => {
                crate::analytics_outbox::DeliveryDisposition::Retry {
                    class: error.class(),
                    reason: error.reason(),
                    retry_after: error.retry_after(),
                }
            }
            Err(error) => crate::analytics_outbox::DeliveryDisposition::Permanent {
                class: error.class(),
                reason: error.reason(),
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
    let config = match optional_policy_for_owner(data_root, data_root_id)? {
        ResolvedAnalyticsPolicy::Purge => {
            return crate::analytics_outbox::AnalyticsOutbox::try_purge(
                &outbox_path,
                Some(data_root_id),
            )
        }
        ResolvedAnalyticsPolicy::DryRun => return Ok(()),
        ResolvedAnalyticsPolicy::Active(current) => current,
    };
    outbox.reconcile(&attempted)?;
    if started.elapsed() >= timeout {
        return Ok(());
    }
    queue_pending_delivery_observation(data_root, data_root_id, &config, &outbox)
}

fn queue_pending_delivery_observation(
    data_root: &Path,
    data_root_id: &str,
    config: &AppConfig,
    outbox: &crate::analytics_outbox::AnalyticsOutbox,
) -> anyhow::Result<()> {
    let Some(observation) = outbox.pending_observation()? else {
        return Ok(());
    };
    let Some(client_profile_id) = crate::identity::try_device_id(data_root)? else {
        return Ok(());
    };
    let authority = AnalyticsDeliveryAuthority {
        app_version: env!("CARGO_PKG_VERSION"),
        client_profile_id: &client_profile_id,
        data_root_id,
        install_attempt_id: None,
        capability_snapshot: None,
    };
    ctx_client_observability::analytics::deliver_delivery_observation(
        &authority,
        observation.event,
        |body| match optional_policy_for_owner(data_root, data_root_id)? {
            ResolvedAnalyticsPolicy::Purge => {
                let path = crate::identity::device_state_path(ANALYTICS_OUTBOX_FILE, data_root)?;
                crate::analytics_outbox::AnalyticsOutbox::try_purge(&path, Some(data_root_id))?;
                anyhow::bail!("analytics was disabled before recovery observation append")
            }
            ResolvedAnalyticsPolicy::DryRun => {
                anyhow::bail!("analytics dry-run was enabled before recovery observation append")
            }
            ResolvedAnalyticsPolicy::Active(current) => {
                if current.analytics.endpoint != config.analytics.endpoint {
                    anyhow::bail!("analytics endpoint changed before recovery observation append");
                }
                outbox.queue_observation(&current.analytics.endpoint, body, &observation)
            }
        },
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
mod tests;

#[cfg(test)]
#[path = "observability_composition/consent_tests.rs"]
pub(crate) mod consent_tests;
