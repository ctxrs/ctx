#[cfg(test)]
fn committed_generation_recovery_error(
    recovery: ctx_history_index::CommittedPredecessorMigrationRecovery,
) -> ctx_history_index::IndexError {
    ctx_history_index::IndexError::CommittedGenerationNeedsRecovery {
        generation_id: recovery.generation_id().to_owned(),
        stage: "predecessor migration recovery",
        detail: recovery.detail().to_owned(),
    }
}

mod composition;
pub use composition::{install_host, AppConfig, DaemonCliHost, DaemonConfig, DaemonMode};
pub use ctx_daemon_application::DaemonHostRunRequest;
pub use ctx_daemon_service::{CoreGenerationPublished, DaemonConfigSnapshot, DaemonUpgradePorts};

pub fn supervisor_environment_allowlist_names() -> Vec<&'static str> {
    ctx_daemon_application::supervisor_environment_allowlist_names()
}

mod config {
    #[cfg(test)]
    pub use crate::composition::DAEMON_MODE_ENV;
    pub use crate::composition::{
        persisted_daemon_enabled, set_daemon_enabled, AppConfig, DaemonMode, CONFIG_FILE,
        DAEMON_DEFAULT_ENABLED,
    };

    #[cfg(test)]
    pub(crate) static TEST_LOCAL_USAGE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}

use ctx_terminal::compact_json;

mod identity {
    pub fn home_dir() -> Option<std::path::PathBuf> {
        crate::composition::host().home_dir()
    }
}

mod analytics {
    use std::path::Path;

    use ctx_client_observability::analytics::PublicEventV1;

    pub fn send_batch(data_root: &Path, events: &[PublicEventV1]) {
        crate::composition::host().deliver_daemon_events(data_root, events);
    }
}

mod net {
    use std::{io::Write, time::Duration};

    use anyhow::Result;

    pub fn get_to_writer_limited(
        endpoint: &str,
        max_bytes: u64,
        timeout: Duration,
        writer: &mut dyn Write,
    ) -> Result<u64> {
        crate::composition::host().fetch_to_writer(endpoint, max_bytes, timeout, writer)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("CLI error was already rendered")]
pub struct RenderedCliError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonStartModeArg {
    Auto,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonTriggerCommandArg {
    Setup,
    Import,
    Search,
    Semantic,
}

impl DaemonTriggerCommandArg {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Import => "import",
            Self::Search => "search",
            Self::Semantic => "semantic",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DaemonArgs {
    pub command: DaemonCommand,
}

#[derive(Debug, Clone)]
pub enum DaemonCommand {
    Run(DaemonRunArgs),
    Status(FormatArgs),
    Enable(FormatArgs),
    Disable(DaemonDisableArgs),
}

#[derive(Debug, Clone)]
pub struct FormatArgs {
    pub format: ctx_terminal::JsonOutputFormat,
}

#[derive(Debug)]
pub struct IndexingModeUpdate {
    pub automatic: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub persistent: bool,
    pub supervisor: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct DaemonDisableArgs {
    pub format: ctx_terminal::JsonOutputFormat,
    pub prepare_uninstall: bool,
}

#[derive(Debug, Clone)]
pub struct DaemonRunArgs {
    pub loop_interval_seconds: Option<u64>,
    pub max_chunks: Option<usize>,
    pub finite_core_worker: bool,
    pub force: bool,
    pub start_mode: Option<DaemonStartModeArg>,
    pub trigger_command: Option<DaemonTriggerCommandArg>,
    pub format: ctx_terminal::JsonOutputFormat,
}

#[allow(unused_imports)]
pub use ctx_semantic_model::{
    prepare_platform_semantic_acceleration, semantic_managed_model_snapshot_dir,
    semantic_native_accelerator_target, semantic_provisioning_coreml_asset_matches,
    semantic_provisioning_model_contract_matches, semantic_provisioning_model_path_count,
    semantic_provisioning_model_path_matches, semantic_query_service_supported,
    semantic_required_model_file_count, semantic_required_model_file_matches,
    SemanticNativeAcceleratorTarget, SemanticOrtModelVariant,
};
#[cfg(test)]
#[allow(unused_imports)]
use ctx_semantic_model::{
    semantic_model_cache_available, semantic_model_key, SemanticDaemonCpuFallbackRequired,
    SemanticDaemonModelAcquisition, SemanticModelLoadDeferred, SharedSemanticRuntime,
    SEMANTIC_DIMENSIONS,
};
mod model_config;
pub use model_config::{semantic_runtime_cache_dir, semantic_worker_cache_dir};
mod runtime_limits;
pub use ctx_semantic_index::SemanticNotReady;
#[allow(unused_imports)]
pub use runtime_limits::SEMANTIC_WORKER_BATCH_MAX;
mod query_adapter;
pub use query_adapter::SemanticQueryAdapter;
mod query_service;
pub use query_service::wait_for_daemon_query_service;
mod daemon;
mod paths_status;
pub use daemon::{run_daemon_command, update_indexing_mode};
pub mod daemon_service_ports;
mod daemon_status;
mod daemon_supervisor;
mod source_status;
pub use source_status::{
    current_rejected_record_count, source_epoch_status_report, SourceEpochStatus,
};
mod source_backed_refresh_coordinator;
pub use source_backed_refresh_coordinator::{
    coordinate_import_source_backed_refresh_with_progress,
    coordinate_setup_source_backed_refresh_with_progress, coordinate_source_backed_refresh,
    coordinate_source_backed_refresh_with_progress, pin_active_verified_generation,
    published_explicit_source_relocation_authority, PinnedSourceBackedGeneration, RefreshStatus,
    SourceBackedRefreshDaemonUnavailable, SourceBackedRefreshMode, SourceBackedRefreshObservation,
    SourceBackedRefreshPendingPublication, SourceBackedRefreshTerminalError,
};
mod daemon_autostart;
#[allow(unused_imports)]
pub use daemon_autostart::{
    autostart_daemon_and_wait, autostart_daemon_for_setup_and_wait,
    begin_current_daemon_upgrade_handoff, begin_daemon_upgrade_handoff,
    begin_legacy_daemon_upgrade_handoff, complete_replacement_daemon_handoff,
    daemon_autostart_suppression_reason, finish_replacement_daemon_handoff,
    mark_replacement_helper_handoff, maybe_autostart_daemon, observe_daemon_for_setup_and_wait,
    replacement_helper_owns_daemon_handoff, DaemonHandoff, DaemonSetupHandoff,
    DaemonUpgradeHandoff,
};

/// Persists the final-binary restart intent consumed only after daemon readiness.
pub fn publish_daemon_restart_intent(
    data_root: &std::path::Path,
    trigger: DaemonTriggerCommandArg,
    request_id: &str,
) -> anyhow::Result<std::path::PathBuf> {
    daemon_autostart::write_daemon_restart_request(data_root, trigger, request_id)
}

/// Reports whether a recognized final-binary restart intent remains durable.
pub fn daemon_restart_intent_pending(data_root: &std::path::Path) -> bool {
    daemon_autostart::read_daemon_restart_request(data_root).is_some()
}
mod health_search;
#[cfg(test)]
mod tests;
