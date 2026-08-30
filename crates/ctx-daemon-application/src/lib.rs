//! Transport-neutral daemon lifecycle policy.
//!
//! The CLI supplies the concrete installation, endpoint, and host-service
//! operations through [`DaemonApplicationHost`].  This crate deliberately owns
//! neither CLI parsing/rendering nor upgrade-engine authority.

use std::{
    path::{Path, PathBuf},
    process::Child,
    time::Duration,
};

use anyhow::Result;
use ctx_client_observability::analytics::PublicEventV1;
use ctx_daemon_runtime::{DaemonHandoffRestartDeferral, NormalizedLaunch};
use serde_json::Value;

mod control;
mod host;
mod lifecycle;
mod status;
mod supervisor;

pub(crate) const SEMANTIC_EMBEDDING_TOKEN_ENV: &str = "CTX_SEMANTIC_EMBEDDING_TOKEN";
pub(crate) const SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV: &str =
    "CTX_SEMANTIC_EMBEDDING_TOKEN_ENDPOINT";

pub use control::{DaemonEnabledUpdate, DaemonEnabledUpdateError};
pub use host::{
    DaemonHostRunError, DaemonHostRunProfile, DaemonHostRunRequest, DaemonHostStartMode,
    DaemonObservedOperation, DAEMON_BACKGROUND_CHILD_ENV,
};
pub use lifecycle::{
    configured_daemon_autostart_command, daemon_autostart_allowed, daemon_autostart_command,
    daemon_autostart_suppression_reason, daemon_restart_trigger, parse_persisted_trigger,
    spawn_detached_daemon_child, DaemonHandoff, DaemonStartError,
};
pub use status::{
    DaemonConfigReloadContext, DaemonSemanticStatusContext, DaemonStatusPreparation,
    DaemonStatusSnapshot,
};
pub use supervisor::{
    DaemonSupervisorStart, DaemonSupervisorUpgradeFence, DaemonSupervisorUpgradeResume,
};

/// Narrow boundary for product-specific operations around neutral lifecycle
/// policy. Implementations belong to the CLI composition layer.
pub trait DaemonApplicationHost: Send + Sync {
    fn hosted_uninstall_active(&self) -> Result<bool>;
    fn hosted_uninstall_active_for_executable(&self, executable: &Path) -> Result<bool>;
    fn managed_install_executable(&self) -> Result<Option<PathBuf>>;
    fn installation_upgrade_active(&self) -> Result<bool>;
    fn automatic_upgrade_recovery_allowed(&self, data_root: &Path) -> Result<bool>;
    fn daemon_config(&self, data_root: &Path) -> Result<DaemonConfigSnapshot>;
    fn persisted_daemon_enabled(&self, data_root: &Path) -> Result<bool>;
    fn defer_restart_for_upgrade_handoff(
        &self,
        data_root: &Path,
        trigger: DaemonTrigger,
    ) -> Result<Option<DaemonHandoffRestartDeferral>>;
    fn request_lifecycle_wakeup(
        &self,
        data_root: &Path,
        request: Value,
        timeout: Duration,
        response_limit: u64,
    ) -> Result<Option<Value>>;
    fn home_dir(&self) -> Option<PathBuf>;
    fn run_daemon_service(&self, data_root: &Path, request: DaemonHostRunRequest) -> Result<()>;
    fn set_daemon_enabled(&self, data_root: &Path, enabled: bool) -> Result<()>;
    fn request_daemon_shutdown(
        &self,
        data_root: &Path,
        timeout: Duration,
        response_limit: u64,
    ) -> Result<()>;
    fn terminate_current_executable_daemon(&self, data_root: &Path) -> Result<()>;
    fn remove_released_daemon_service_artifacts(&self, data_root: &Path) -> Result<()>;
    fn cancel_core_finalization_generation_lease(
        &self,
        data_root: &Path,
        reason: &str,
    ) -> Result<()>;
    fn observe_source_refresh_endpoint(&self, identity_path: &Path) -> DaemonEndpointObservation;
    fn deliver_daemon_events(&self, data_root: &Path, events: &[PublicEventV1]);
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DaemonEndpointObservation {
    pub available: bool,
    pub transport: Option<String>,
    pub owner_pid: Option<u32>,
    pub address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonConfigSnapshot {
    pub enabled: bool,
    pub mode: DaemonMode,
    pub semantic_enabled: bool,
    /// Redaction-safe exact selector: `builtin` or a normalized endpoint URL.
    pub semantic_executor: String,
    /// Redaction-safe exact fingerprint of the selected semantic vector-space contract.
    pub semantic_contract_fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonMode {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonTrigger {
    Setup,
    Import,
    Search,
    Semantic,
}

impl DaemonTrigger {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Import => "import",
            Self::Search => "search",
            Self::Semantic => "semantic",
        }
    }

    pub fn parse_persisted(value: &str) -> Option<Self> {
        match value {
            "setup" => Some(Self::Setup),
            "import" => Some(Self::Import),
            "search" => Some(Self::Search),
            "semantic" => Some(Self::Semantic),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonStartMode {
    Auto,
}

/// Names the complete approved environment contract that may cross the
/// Core/companion boundary for setup and persist in daemon supervision.
pub fn supervisor_environment_allowlist_names() -> Vec<&'static str> {
    supervisor::supervisor_environment_allowlist_names()
}

impl DaemonStartMode {
    const fn as_str(self) -> &'static str {
        "auto"
    }
}

/// Borrowed application policy facade. Constructing it performs no allocation,
/// lookup, configuration load, or I/O.
#[derive(Clone, Copy)]
pub struct DaemonApplication<'a> {
    host: &'a dyn DaemonApplicationHost,
}

impl<'a> DaemonApplication<'a> {
    pub const fn new(host: &'a dyn DaemonApplicationHost) -> Self {
        Self { host }
    }

    pub fn ensure_daemon_supervisor(&self, data_root: &Path) -> Result<DaemonSupervisorStart> {
        supervisor::ensure_daemon_supervisor(self.host, data_root)
    }

    pub fn disable_daemon_supervisor(&self, data_root: &Path) -> Result<()> {
        supervisor::disable_daemon_supervisor(self.host, data_root)
    }

    pub fn daemon_supervisor_report(&self, data_root: &Path) -> DaemonSupervisorReport {
        DaemonSupervisorReport::new(supervisor::daemon_supervisor_report(self.host, data_root))
    }

    pub fn resume_daemon_supervisor_after_upgrade(
        &self,
        data_root: &Path,
        executable: &Path,
        loop_interval_seconds: Option<u64>,
        upgrade_fence: &mut dyn DaemonSupervisorUpgradeFence,
    ) -> Result<DaemonSupervisorUpgradeResume> {
        supervisor::resume_daemon_supervisor_after_upgrade(
            self.host,
            data_root,
            executable,
            loop_interval_seconds,
            upgrade_fence,
        )
    }

    pub fn start_daemon_and_wait(
        &self,
        data_root: &Path,
        config: &DaemonConfigSnapshot,
        trigger: DaemonTrigger,
    ) -> std::result::Result<DaemonHandoff, DaemonStartError> {
        lifecycle::start_daemon_and_wait(self.host, data_root, config, trigger)
    }

    pub fn start_core_daemon_and_wait(
        &self,
        data_root: &Path,
        config: &DaemonConfigSnapshot,
        trigger: DaemonTrigger,
    ) -> std::result::Result<DaemonHandoff, DaemonStartError> {
        lifecycle::start_core_daemon_and_wait(self.host, data_root, config, trigger)
    }

    pub fn restart_daemon_with_current_environment(
        &self,
        data_root: &Path,
        config: &DaemonConfigSnapshot,
        trigger: DaemonTrigger,
    ) -> std::result::Result<DaemonHandoff, DaemonStartError> {
        control::restart_daemon_with_current_environment(self.host, data_root, config, trigger)
    }

    pub fn start_finite_core_worker_and_wait(
        &self,
        data_root: &Path,
        config: &DaemonConfigSnapshot,
        trigger: DaemonTrigger,
    ) -> std::result::Result<DaemonHandoff, DaemonStartError> {
        lifecycle::start_finite_core_worker_and_wait(self.host, data_root, config, trigger)
    }

    pub fn observe_daemon_and_wait(
        &self,
        data_root: &Path,
        config: &DaemonConfigSnapshot,
    ) -> Result<DaemonHandoff> {
        lifecycle::observe_daemon_and_wait(self.host, data_root, config)
    }

    pub fn daemon_start_is_fenced(&self) -> bool {
        lifecycle::daemon_start_is_fenced(self.host)
    }

    pub fn active_daemon_matches_current_executable(&self, data_root: &Path) -> Result<bool> {
        lifecycle::active_daemon_matches_current_executable(data_root)
    }

    pub fn request_daemon_start(
        &self,
        data_root: &Path,
        config: &DaemonConfigSnapshot,
        trigger: DaemonTrigger,
    ) -> Result<()> {
        lifecycle::request_daemon_start(self.host, data_root, config, trigger)
    }

    pub fn handoff_mismatched_daemon_owner(
        &self,
        data_root: &Path,
        executable: &Path,
    ) -> Result<()> {
        lifecycle::handoff_mismatched_daemon_owner(self.host, data_root, executable)
    }

    pub fn spawn_daemon_child(&self, launch: NormalizedLaunch) -> std::io::Result<Child> {
        lifecycle::spawn_daemon_child(self.host, launch)
    }

    pub fn spawn_daemon_child_for_upgrade_handoff(
        &self,
        launch: NormalizedLaunch,
        executable: &Path,
    ) -> std::io::Result<Child> {
        lifecycle::spawn_daemon_child_for_upgrade_handoff(self.host, launch, executable)
    }

    pub fn daemon_restart_allowed(&self, data_root: &Path) -> Result<bool> {
        lifecycle::daemon_restart_allowed(self.host, data_root)
    }

    pub fn run_daemon_host(
        &self,
        data_root: &Path,
        request: DaemonHostRunRequest,
    ) -> std::result::Result<(), DaemonHostRunError> {
        host::run_daemon_host(self.host, data_root, request)
    }

    pub fn observe_daemon_operation(
        &self,
        data_root: &Path,
        operation: DaemonObservedOperation,
        succeeded: bool,
        elapsed: Duration,
    ) {
        host::observe_daemon_operation(self.host, data_root, operation, succeeded, elapsed);
    }

    pub fn update_daemon_enabled(
        &self,
        data_root: &Path,
        enabled: bool,
    ) -> std::result::Result<DaemonEnabledUpdate, DaemonEnabledUpdateError> {
        control::update_daemon_enabled(self.host, data_root, enabled)
    }

    pub fn prepare_daemon_status<'b>(
        &'b self,
        data_root: &'b Path,
        disabled_overrides_lifecycle: bool,
        current_config: Option<&DaemonConfigSnapshot>,
        default_daemon_enabled: bool,
    ) -> DaemonStatusPreparation<'b> {
        status::prepare_daemon_status(
            self.host,
            data_root,
            disabled_overrides_lifecycle,
            current_config,
            default_daemon_enabled,
        )
    }
}

#[derive(Debug)]
pub struct DaemonSupervisorReport(Value);

impl DaemonSupervisorReport {
    pub(crate) fn new(value: Value) -> Self {
        Self(value)
    }

    pub fn into_json(self) -> Value {
        self.0
    }
}

#[cfg(test)]
pub(crate) struct TestHost;

#[cfg(test)]
impl DaemonApplicationHost for TestHost {
    fn hosted_uninstall_active(&self) -> Result<bool> {
        Ok(false)
    }

    fn hosted_uninstall_active_for_executable(&self, _executable: &Path) -> Result<bool> {
        Ok(false)
    }

    fn managed_install_executable(&self) -> Result<Option<PathBuf>> {
        Ok(None)
    }

    fn installation_upgrade_active(&self) -> Result<bool> {
        Ok(false)
    }

    fn automatic_upgrade_recovery_allowed(&self, _data_root: &Path) -> Result<bool> {
        Ok(false)
    }

    fn daemon_config(&self, _data_root: &Path) -> Result<DaemonConfigSnapshot> {
        Ok(DaemonConfigSnapshot {
            enabled: true,
            mode: DaemonMode::Full,
            semantic_enabled: true,
            semantic_executor: "builtin".to_owned(),
            semantic_contract_fingerprint: "sha256:test-builtin-contract".to_owned(),
        })
    }

    fn persisted_daemon_enabled(&self, _data_root: &Path) -> Result<bool> {
        Ok(true)
    }

    fn defer_restart_for_upgrade_handoff(
        &self,
        _data_root: &Path,
        _trigger: DaemonTrigger,
    ) -> Result<Option<DaemonHandoffRestartDeferral>> {
        Ok(None)
    }

    fn request_lifecycle_wakeup(
        &self,
        _data_root: &Path,
        _request: Value,
        _timeout: Duration,
        _response_limit: u64,
    ) -> Result<Option<Value>> {
        Ok(None)
    }

    fn home_dir(&self) -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }

    fn run_daemon_service(&self, _data_root: &Path, _request: DaemonHostRunRequest) -> Result<()> {
        Ok(())
    }

    fn set_daemon_enabled(&self, _data_root: &Path, _enabled: bool) -> Result<()> {
        Ok(())
    }

    fn request_daemon_shutdown(
        &self,
        _data_root: &Path,
        _timeout: Duration,
        _response_limit: u64,
    ) -> Result<()> {
        Ok(())
    }

    fn terminate_current_executable_daemon(&self, _data_root: &Path) -> Result<()> {
        Ok(())
    }

    fn remove_released_daemon_service_artifacts(&self, _data_root: &Path) -> Result<()> {
        Ok(())
    }

    fn cancel_core_finalization_generation_lease(
        &self,
        _data_root: &Path,
        _reason: &str,
    ) -> Result<()> {
        Ok(())
    }

    fn observe_source_refresh_endpoint(&self, _identity_path: &Path) -> DaemonEndpointObservation {
        DaemonEndpointObservation::default()
    }

    fn deliver_daemon_events(&self, _data_root: &Path, _events: &[PublicEventV1]) {}
}

#[cfg(test)]
pub(crate) fn test_environment_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

fn compact_json(mut value: Value) -> Value {
    fn compact(value: &mut Value) {
        match value {
            Value::Object(object) => {
                object.retain(|_, value| !value.is_null());
                for value in object.values_mut() {
                    compact(value);
                }
            }
            Value::Array(values) => values.iter_mut().for_each(compact),
            _ => {}
        }
    }
    compact(&mut value);
    value
}

#[cfg(test)]
mod dto_tests {
    use super::*;

    #[test]
    fn daemon_mode_names_and_persisted_parser_are_exact() {
        assert_eq!(DaemonMode::Full.as_str(), "full");
        assert_eq!(
            DaemonMode::SourceRefreshOnly.as_str(),
            "source-refresh-only"
        );
        assert!(!DaemonMode::Full.runs_only_source_refresh());
        assert!(DaemonMode::SourceRefreshOnly.runs_only_source_refresh());
        assert_eq!(DaemonMode::parse("full"), Some(DaemonMode::Full));
        assert_eq!(DaemonMode::parse("FULL"), Some(DaemonMode::Full));
        assert_eq!(
            DaemonMode::parse("SOURCE-REFRESH-ONLY"),
            Some(DaemonMode::SourceRefreshOnly)
        );
        assert_eq!(DaemonMode::parse("source_refresh_only"), None);
        assert_eq!(DaemonMode::parse(" full "), None);
        assert_eq!(DaemonMode::parse(""), None);
    }

    #[test]
    fn trigger_names_and_parser_preserve_the_schema_v1_vocabulary() {
        for (trigger, name) in [
            (DaemonTrigger::Setup, "setup"),
            (DaemonTrigger::Import, "import"),
            (DaemonTrigger::Search, "search"),
            (DaemonTrigger::Semantic, "semantic"),
        ] {
            assert_eq!(trigger.as_str(), name);
            assert_eq!(DaemonTrigger::parse_persisted(name), Some(trigger));
            assert_eq!(parse_persisted_trigger(Some(name)), Some(trigger));
        }
        assert_eq!(DaemonTrigger::parse_persisted("SEARCH"), None);
        assert_eq!(DaemonTrigger::parse_persisted("manual"), None);
        assert_eq!(DaemonTrigger::parse_persisted("setup "), None);
        assert_eq!(DaemonTrigger::parse_persisted(""), None);
        assert_eq!(parse_persisted_trigger(None), None);
        assert_eq!(parse_persisted_trigger(Some("unknown")), None);
        assert_eq!(parse_persisted_trigger(Some("Import")), None);
    }

    #[test]
    fn absent_endpoint_observation_has_no_invented_identity() {
        let observation = DaemonEndpointObservation::default();

        assert!(!observation.available);
        assert_eq!(observation.transport, None);
        assert_eq!(observation.owner_pid, None);
        assert_eq!(observation.address, None);
    }
}
