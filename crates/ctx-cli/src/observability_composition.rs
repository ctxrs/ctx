use std::path::{Path, PathBuf};

use crate::{
    analytics::{AnalyticsDeliveryAuthority, PublicEventV1},
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

pub(crate) fn deliver_analytics_batch(
    data_root: &Path,
    config: &AppConfig,
    events: &[PublicEventV1],
) -> anyhow::Result<()> {
    if events.is_empty()
        || !config.analytics.enabled
        || std::env::var_os("CTX_ANALYTICS_DRY_RUN").is_some()
    {
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
    ctx_client_observability::analytics::deliver_batch(&mut authority, events, |body| {
        crate::net::post_telemetry_json(&config.analytics.endpoint, body)
    })
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

    #[test]
    fn selected_telemetry_contract_inventory_hashes_match_the_running_public_source() {
        use sha2::{Digest, Sha256};

        let provenance: serde_json::Value = serde_json::from_str(include_str!(
            "../../../contracts/telemetry-v1/source-provenance.json"
        ))
        .unwrap();
        assert_eq!(provenance["repository"], "ctxrs/ctx");
        assert_eq!(
            provenance["base_commit"],
            "f09692181835a8b81108a7d99dca8d1ed5712502"
        );
        assert_eq!(provenance["provenance_kind"], "content_addressed_candidate");
        assert_eq!(
            provenance["scope"],
            "selected_typed_telemetry_contract_inventory"
        );
        assert!(
            provenance.get("state").is_none(),
            "content provenance must not claim a transient worktree state"
        );
        let files = provenance["files"].as_object().unwrap();
        let sources = [
            (
                "contracts/telemetry-v1/providers-v1.json",
                include_bytes!("../../../contracts/telemetry-v1/providers-v1.json").as_slice(),
            ),
            (
                "crates/ctx-client-observability/src/analytics/client.rs",
                include_bytes!("../../ctx-client-observability/src/analytics/client.rs").as_slice(),
            ),
            (
                "crates/ctx-client-observability/src/analytics/operation.rs",
                include_bytes!("../../ctx-client-observability/src/analytics/operation.rs")
                    .as_slice(),
            ),
            (
                "crates/ctx-client-observability/src/analytics/daemon.rs",
                include_bytes!("../../ctx-client-observability/src/analytics/daemon.rs").as_slice(),
            ),
            (
                "crates/ctx-client-observability/src/analytics/mcp.rs",
                include_bytes!("../../ctx-client-observability/src/analytics/mcp.rs").as_slice(),
            ),
            (
                "crates/ctx-client-observability/src/analytics/search.rs",
                include_bytes!("../../ctx-client-observability/src/analytics/search.rs").as_slice(),
            ),
            (
                "crates/ctx-client-observability/src/analytics/runtime.rs",
                include_bytes!("../../ctx-client-observability/src/analytics/runtime.rs")
                    .as_slice(),
            ),
            (
                "crates/ctx-client-observability/src/analytics/buckets.rs",
                include_bytes!("../../ctx-client-observability/src/analytics/buckets.rs")
                    .as_slice(),
            ),
            (
                "crates/ctx-client-observability/src/analytics/provider.rs",
                include_bytes!("../../ctx-client-observability/src/analytics/provider.rs")
                    .as_slice(),
            ),
            (
                "crates/ctx-client-observability/src/analytics/product.rs",
                include_bytes!("../../ctx-client-observability/src/analytics/product.rs")
                    .as_slice(),
            ),
            (
                "crates/ctx-client-observability/src/analytics/sender.rs",
                include_bytes!("../../ctx-client-observability/src/analytics/sender.rs").as_slice(),
            ),
            (
                "crates/ctx-cli/src/upgrade/command.rs",
                include_bytes!("upgrade/command.rs").as_slice(),
            ),
            (
                "crates/ctx-cli/src/upgrade/ports.rs",
                include_bytes!("upgrade/ports.rs").as_slice(),
            ),
            (
                "crates/ctx-upgrade-engine/src/upgrade/command/daemon.rs",
                include_bytes!("../../ctx-upgrade-engine/src/upgrade/command/daemon.rs").as_slice(),
            ),
            (
                "crates/ctx-upgrade-engine/src/upgrade/state.rs",
                include_bytes!("../../ctx-upgrade-engine/src/upgrade/state.rs").as_slice(),
            ),
        ];
        for required in [
            "crates/ctx-client-observability/src/analytics/client.rs",
            "crates/ctx-client-observability/src/analytics/search.rs",
            "crates/ctx-client-observability/src/analytics/sender.rs",
        ] {
            assert!(
                files.contains_key(required) && sources.iter().any(|(path, _)| *path == required),
                "required telemetry serializer contract source is absent: {required}"
            );
        }
        assert_eq!(files.len(), sources.len());
        for (path, source) in sources {
            let digest = Sha256::digest(source)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            assert_eq!(files[path], digest, "stale telemetry provenance for {path}");
        }
    }
}
