#[cfg(test)]
use std::{
    fs,
    path::Path,
    process,
    time::{Duration as StdDuration, Instant},
};

#[cfg(all(unix, test))]
use std::{io::Write, path::PathBuf};

#[cfg(test)]
use std::sync::Arc;

#[cfg(all(unix, test))]
use std::net::Shutdown;
#[cfg(all(unix, test))]
use std::os::unix::ffi::OsStrExt;
#[cfg(all(unix, test))]
use std::os::unix::net::UnixStream;

#[cfg(all(unix, test))]
use anyhow::Context;
#[cfg(test)]
use anyhow::{anyhow, Result};
#[cfg(test)]
use serde_json::{json, Value};
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
use ctx_history_core::utc_now;

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

#[cfg(test)]
use crate::config::CONFIG_FILE;
#[cfg(test)]
use crate::output::compact_json;
#[cfg(test)]
use crate::{DaemonRunArgs, DaemonStartModeArg, DaemonTriggerCommandArg};

#[allow(unused_imports)]
pub(crate) use ctx_semantic_model::{
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
pub(crate) use model_config::{
    semantic_model_config, semantic_runtime_cache_dir, semantic_worker_cache_dir,
};
mod resource_policy;
mod runtime_limits;
#[allow(unused_imports)]
pub(crate) use runtime_limits::{DAEMON_IDLE_EXIT_SECONDS_CAP, SEMANTIC_WORKER_BATCH_MAX};
mod document;
pub(in crate::semantic) use document::SemanticEventDocument;
mod vector_store;
#[cfg(test)]
use vector_store::*;
mod query_index;
pub(crate) use query_index::SemanticNotReady;
mod query_adapter;
pub(crate) use query_adapter::SemanticQueryAdapter;
mod query_service;
pub(crate) use query_service::wait_for_daemon_query_service;
#[cfg(test)]
use query_service::*;
mod paths_status;
#[cfg(test)]
use paths_status::*;
mod vector_store_schema;
#[cfg(test)]
use vector_store_schema::{SemanticVectorStoreError, SEMANTIC_VECTOR_SCHEMA_VERSION};
mod daemon;
mod vector_store_search;
mod vector_store_state;
pub(crate) use daemon::run_daemon_command;
#[cfg(test)]
use daemon::*;
mod daemon_retry;
mod daemon_status;
mod daemon_supervisor;
mod daemon_wakeup;
#[cfg(test)]
use daemon_retry::*;
mod source_status;
pub(crate) use source_status::source_epoch_status_report;
mod source_backed_pro_catch_up;
pub(crate) use source_backed_pro_catch_up::wait_for_completed_generation as wait_for_source_backed_pro_generation;
pub(crate) use source_backed_pro_catch_up::{
    helper_recheck_targets as source_backed_pro_recheck_targets,
    publish_helper_recheck_intent as publish_source_backed_pro_recheck,
    wake_helper_recheck as wake_source_backed_pro_recheck,
};
mod source_backed_refresh_adapter;
mod source_backed_refresh_coordinator;
#[cfg(test)]
pub(crate) use source_backed_refresh_coordinator::SourceBackedRefreshPublication;
pub(crate) use source_backed_refresh_coordinator::{
    coordinate_import_source_backed_refresh_with_progress, coordinate_source_backed_refresh,
    coordinate_source_backed_refresh_with_progress, pin_active_verified_generation,
    published_explicit_source_relocation_authority, PinnedSourceBackedGeneration, RefreshStatus,
    SourceBackedCurrentSourceProgress, SourceBackedRefreshDaemonUnavailable,
    SourceBackedRefreshMode, SourceBackedRefreshObservation,
};
mod daemon_scheduler;
#[cfg(test)]
use daemon_scheduler::*;
mod daemon_worker;
#[cfg(test)]
use daemon_worker::*;
mod daemon_autostart;
#[allow(unused_imports)]
pub(crate) use daemon_autostart::{
    autostart_daemon_and_wait, begin_current_daemon_upgrade_handoff, begin_daemon_upgrade_handoff,
    begin_legacy_daemon_upgrade_handoff, complete_replacement_daemon_handoff,
    daemon_autostart_suppression_reason, finish_replacement_daemon_handoff,
    mark_replacement_helper_handoff, maybe_autostart_daemon,
    replacement_helper_owns_daemon_handoff, DaemonHandoff, DaemonUpgradeHandoff,
};
mod health_search;
#[cfg(test)]
use health_search::*;
mod indexing;
#[cfg(test)]
mod query_service_transport_tests;
#[cfg(all(
    test,
    any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64"),
            target_env = "gnu"
        ),
        all(
            target_os = "macos",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "freebsd", target_arch = "x86_64")
    )
))]
mod tests;
