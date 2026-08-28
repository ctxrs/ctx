use std::path::{Path, PathBuf};

use crate::{
    analytics::{AnalyticsDeliveryAuthority, AnalyticsDeliveryFailureClass, PublicEventV1},
    config::{AppConfig, LocalUsageConfigResolver, LocalUsageConfigState},
    local_usage::{UsageControlRevision, UsageControlSnapshot},
};

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

pub(crate) fn deliver_analytics_batch(
    data_root: &Path,
    config: &AppConfig,
    events: &[PublicEventV1],
) -> anyhow::Result<()> {
    if std::env::var_os("CTX_ANALYTICS_DRY_RUN").is_some() {
        return Ok(());
    }
    let outbox_path = crate::identity::device_state_path(ANALYTICS_OUTBOX_FILE, data_root)?;
    if !config.analytics.enabled {
        return crate::analytics_outbox::AnalyticsOutbox::purge(&outbox_path);
    }
    if events.is_empty() {
        return Ok(());
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
    let mut outbox = crate::analytics_outbox::AnalyticsOutbox::open(outbox_path)?;
    let flush = outbox.flush(&config.analytics.endpoint, |body| {
        crate::net::post_telemetry_json(&config.analytics.endpoint, body)
            .map_err(|error| error.class())
    })?;
    let mut blocked = match flush {
        crate::analytics_outbox::FlushStatus::Available => None,
        crate::analytics_outbox::FlushStatus::Blocked(class) => Some(class),
    };
    {
        let mut post_or_queue = |body: &[u8]| -> anyhow::Result<()> {
            if blocked.is_none() {
                match crate::net::post_telemetry_json(&config.analytics.endpoint, body) {
                    Ok(()) => return Ok(()),
                    Err(error) => blocked = Some(error.class()),
                }
            }
            let class = blocked.unwrap_or(AnalyticsDeliveryFailureClass::Unknown);
            outbox.enqueue(&config.analytics.endpoint, body, class)
        };
        ctx_client_observability::analytics::deliver_batch(
            &mut authority,
            events,
            &mut post_or_queue,
        )?;
    }
    if let Some(observation) = outbox.observation() {
        {
            let mut post_or_queue = |body: &[u8]| -> anyhow::Result<()> {
                if blocked.is_none() {
                    match crate::net::post_telemetry_json(&config.analytics.endpoint, body) {
                        Ok(()) => return Ok(()),
                        Err(error) => blocked = Some(error.class()),
                    }
                }
                let class = blocked.unwrap_or(AnalyticsDeliveryFailureClass::Unknown);
                outbox.enqueue(&config.analytics.endpoint, body, class)
            };
            ctx_client_observability::analytics::deliver_delivery_observation(
                &authority,
                observation.event,
                &mut post_or_queue,
            )?;
        }
        outbox.acknowledge(&observation)?;
    }
    Ok(())
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
    use super::*;

    #[test]
    fn storage_authority_grants_only_the_exact_legacy_database_path() {
        let root = Path::new("/tmp/ctx-observability-authority-test");
        assert_eq!(
            local_usage_storage_authority(root).database_path(),
            root.join("usage.sqlite")
        );
    }
}
