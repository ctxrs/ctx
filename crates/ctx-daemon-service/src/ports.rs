use std::{path::Path, time::Duration};

use anyhow::Result;
use ctx_client_observability::analytics::PublicEventV1;
use ctx_history_capture::DiscoveryContext;
use ctx_semantic_model::SemanticModelConfig;
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DaemonMode {
    #[default]
    Full,
    SourceRefreshOnly,
}

impl DaemonMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::SourceRefreshOnly => "source-refresh-only",
        }
    }

    pub const fn runs_only_source_refresh(self) -> bool {
        matches!(self, Self::SourceRefreshOnly)
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "full" => Some(Self::Full),
            "source-refresh-only" => Some(Self::SourceRefreshOnly),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DaemonStartMode {
    #[default]
    Manual,
    Auto,
}

impl DaemonStartMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonTrigger {
    Setup,
    Import,
    Search,
}

impl DaemonTrigger {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Import => "import",
            Self::Search => "search",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DaemonRunArgs {
    pub loop_interval_seconds: Option<u64>,
    pub max_chunks: Option<usize>,
    pub handle_process_signals: bool,
    pub force: bool,
    pub profile: DaemonRunProfile,
    pub start_mode: Option<DaemonStartMode>,
    pub trigger_command: Option<DaemonTrigger>,
    pub supervisor: DaemonSupervisor,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DaemonRunProfile {
    #[default]
    Persistent,
    FiniteCoreWorker,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DaemonSupervisor {
    #[default]
    User,
    CliAutostart,
}

#[derive(Debug, Clone, Default)]
pub struct DaemonConfigSnapshot {
    pub daemon: DaemonProductConfig,
    pub semantic_enabled: bool,
    pub automatic_upgrade_enabled: bool,
    pub automatic_upgrade_interval: Duration,
    pub upgrade_channel: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DaemonProductConfig {
    pub enabled: bool,
    pub mode: DaemonMode,
}

impl DaemonConfigSnapshot {
    pub const fn semantic_search_enabled(&self) -> bool {
        self.semantic_enabled
    }
}

impl ctx_upgrade_engine::AutomaticUpgradePolicySnapshot for DaemonConfigSnapshot {
    fn daemon_maintenance_enabled(&self) -> bool {
        self.daemon.enabled && self.daemon.mode == DaemonMode::Full
    }

    fn automatic_upgrade_enabled(&self) -> bool {
        self.automatic_upgrade_enabled
    }

    fn interval(&self) -> Duration {
        self.automatic_upgrade_interval
    }

    fn channel(&self) -> &str {
        &self.upgrade_channel
    }

    fn semantic_enabled(&self) -> bool {
        self.semantic_enabled
    }
}

pub trait DaemonConfigPort: Sync {
    fn load(&self, data_root: &Path) -> Result<DaemonConfigSnapshot>;
    fn semantic_model_config(&self, data_root: &Path) -> SemanticModelConfig;
    fn discovery_context(&self, data_root: &Path) -> Result<DiscoveryContext>;
}

pub trait DaemonAvailabilityPort: Sync {
    fn ensure_available(
        &self,
        data_root: &Path,
        trigger: DaemonTrigger,
        demand: DaemonAvailabilityDemand,
    ) -> Result<DaemonAvailability>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonAvailabilityDemand {
    Background,
    ExplicitWait,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonAvailability {
    Available,
    Disabled,
}

pub trait DaemonInstallationLease {
    fn acknowledge(self, attempt_id: &str) -> Result<()>;
}

pub trait DaemonInstallationPort {
    type Lease: DaemonInstallationLease;

    fn lifecycle_blocks_current_process(
        &self,
        data_root: &Path,
        allow_automatic_recovery: bool,
    ) -> bool;
    fn upgrade_handoff_blocks_current_process(&self, data_root: &Path) -> bool;
    fn current_process_owns_upgrade_handoff(&self, data_root: &Path) -> bool;
    fn acquire(
        &self,
        data_root: &Path,
        trigger: DaemonTrigger,
        loop_interval_seconds: Option<u64>,
        allow_active_upgrade: bool,
        allow_automatic_recovery: bool,
        persistent: bool,
    ) -> Result<Option<Self::Lease>>;
    fn resume_completed(&self, data_root: &Path) -> Result<()>;
    fn acknowledge_restart_requests(&self, data_root: &Path);
}

/// Fixed-size facts from one durable Core generation publication.
///
/// This notification deliberately carries no source identities, repository
/// metadata, projection state, entitlement state, or retry instructions. A
/// consumer can use the generation identity and exact Core cardinalities
/// without acquiring any publication or query authority.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CoreGenerationPublished {
    generation_id: String,
    previous_generation_id: Option<String>,
    generation_changed: bool,
    source_count: u64,
    indexed_document_count: u64,
    complete_record_count: u64,
    retained_record_count: u64,
    rejected_record_count: u64,
    ignored_record_count: u64,
    certified_source_bytes: u64,
}

impl CoreGenerationPublished {
    pub(crate) fn from_job(job: &Value) -> Option<Self> {
        if job.get("status").and_then(Value::as_str) != Some("completed") {
            return None;
        }
        let receipt = job.get("receipt")?;
        let current = receipt.get("current")?;
        let generation_id = receipt
            .get("published_generation")
            .and_then(Value::as_str)?;
        if !is_sha256_identity(generation_id)
            || job.get("published_generation").and_then(Value::as_str) != Some(generation_id)
        {
            return None;
        }
        let previous_generation_id = receipt
            .get("previous_generation")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if previous_generation_id
            .as_deref()
            .is_some_and(|generation| !is_sha256_identity(generation))
        {
            return None;
        }
        Some(Self {
            generation_id: generation_id.to_owned(),
            previous_generation_id,
            generation_changed: receipt.get("generation_changed")?.as_bool()?,
            source_count: current.get("current_source_count")?.as_u64()?,
            indexed_document_count: current.get("current_indexed_documents")?.as_u64()?,
            complete_record_count: current.get("current_complete_records")?.as_u64()?,
            retained_record_count: current.get("current_retained_records")?.as_u64()?,
            rejected_record_count: current.get("current_rejected_records")?.as_u64()?,
            ignored_record_count: current.get("current_ignored_records")?.as_u64()?,
            certified_source_bytes: current.get("current_certified_source_bytes")?.as_u64()?,
        })
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn previous_generation_id(&self) -> Option<&str> {
        self.previous_generation_id.as_deref()
    }

    pub const fn generation_changed(&self) -> bool {
        self.generation_changed
    }

    pub const fn source_count(&self) -> u64 {
        self.source_count
    }

    pub const fn indexed_document_count(&self) -> u64 {
        self.indexed_document_count
    }

    pub const fn complete_record_count(&self) -> u64 {
        self.complete_record_count
    }

    pub const fn retained_record_count(&self) -> u64 {
        self.retained_record_count
    }

    pub const fn rejected_record_count(&self) -> u64 {
        self.rejected_record_count
    }

    pub const fn ignored_record_count(&self) -> u64 {
        self.ignored_record_count
    }

    pub const fn certified_source_bytes(&self) -> u64 {
        self.certified_source_bytes
    }
}

fn is_sha256_identity(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Best-effort publication notification. Implementations must not assume
/// ownership of Core state, and callers must not turn delivery failures into
/// Core publication failures or retries.
pub trait CoreGenerationPublishedPort: Sync {
    fn notify(&self, data_root: &Path, publication: &CoreGenerationPublished) -> Result<()>;
}

pub trait DaemonObservationPort: Sync {
    fn provider_refresh_event(&self, job: &Value, successor_pending: bool)
        -> Option<PublicEventV1>;
    fn deliver(&self, data_root: &Path, events: &[PublicEventV1]);
}

pub struct DaemonServicePorts<'a, C: ?Sized, A: ?Sized, I, N: ?Sized, O: ?Sized> {
    pub config: &'a C,
    pub availability: &'a A,
    pub installation: &'a I,
    pub generation_published: &'a N,
    pub observation: &'a O,
    pub artifact_fetcher: &'a (dyn ctx_semantic_model::ArtifactFetcher + Sync),
}

pub struct DaemonUpgradePorts<'a, D, P, O>
where
    D: ctx_upgrade_engine::DaemonUpgradePort + ?Sized,
{
    pub engine: &'a ctx_upgrade_engine::UpgradeEngine<'a, D>,
    pub daemon: &'a D,
    pub automatic_policy: &'a P,
    pub observer: &'a O,
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use ctx_upgrade_engine::AutomaticUpgradePolicySnapshot;

    use super::*;

    struct Lease(Arc<AtomicBool>);

    impl DaemonInstallationLease for Lease {
        fn acknowledge(self, attempt_id: &str) -> Result<()> {
            assert_eq!(attempt_id, "install-attempt");
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    struct Installation(Arc<AtomicBool>);

    impl DaemonInstallationPort for Installation {
        type Lease = Lease;

        fn lifecycle_blocks_current_process(
            &self,
            _data_root: &Path,
            _allow_automatic_recovery: bool,
        ) -> bool {
            false
        }
        fn upgrade_handoff_blocks_current_process(&self, _data_root: &Path) -> bool {
            false
        }
        fn current_process_owns_upgrade_handoff(&self, _data_root: &Path) -> bool {
            false
        }
        fn acquire(
            &self,
            _data_root: &Path,
            _trigger: DaemonTrigger,
            _loop_interval_seconds: Option<u64>,
            _allow_active_upgrade: bool,
            _allow_automatic_recovery: bool,
            _persistent: bool,
        ) -> Result<Option<Self::Lease>> {
            Ok(Some(Lease(Arc::clone(&self.0))))
        }
        fn resume_completed(&self, _data_root: &Path) -> Result<()> {
            Ok(())
        }
        fn acknowledge_restart_requests(&self, _data_root: &Path) {}
    }

    #[test]
    fn installation_acknowledgement_consumes_the_service_lease() {
        let acknowledged = Arc::new(AtomicBool::new(false));
        let installation = Installation(Arc::clone(&acknowledged));
        let lease = installation
            .acquire(
                Path::new("data"),
                DaemonTrigger::Search,
                None,
                false,
                false,
                true,
            )
            .unwrap()
            .unwrap();

        lease.acknowledge("install-attempt").unwrap();
        assert!(acknowledged.load(Ordering::SeqCst));
    }

    #[test]
    fn service_snapshot_is_the_single_borrowed_upgrade_policy() {
        let snapshot = DaemonConfigSnapshot {
            daemon: DaemonProductConfig {
                enabled: true,
                mode: DaemonMode::Full,
            },
            semantic_enabled: true,
            automatic_upgrade_enabled: true,
            automatic_upgrade_interval: Duration::from_secs(3_600),
            upgrade_channel: "stable".to_owned(),
        };

        assert!(snapshot.daemon.enabled);
        assert!(snapshot.daemon_maintenance_enabled());
        assert!(snapshot.semantic_enabled());
        assert!(snapshot.automatic_upgrade_enabled());
        assert_eq!(snapshot.interval(), Duration::from_secs(3_600));
        assert_eq!(snapshot.channel(), "stable");
    }

    #[test]
    fn daemon_mode_wire_names_and_parser_remain_exact() {
        assert_eq!(DaemonMode::Full.as_str(), "full");
        assert_eq!(
            DaemonMode::SourceRefreshOnly.as_str(),
            "source-refresh-only"
        );
        assert_eq!(DaemonMode::parse("FULL"), Some(DaemonMode::Full));
        assert_eq!(
            DaemonMode::parse("SOURCE-REFRESH-ONLY"),
            Some(DaemonMode::SourceRefreshOnly)
        );
        assert_eq!(DaemonMode::parse("source_refresh_only"), None);
    }
}
