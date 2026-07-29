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
use ctx_history_core::{database_path, utc_now};
#[cfg(test)]
use ctx_history_store::{EventEmbeddingDocument, Store};

#[cfg(test)]
use crate::commands::{import::ImportTotals, search::RefreshArg};
#[cfg(test)]
use crate::config::{AppConfig, CONFIG_FILE};
#[cfg(test)]
use crate::output::compact_json;
#[cfg(test)]
use crate::{DaemonRunArgs, DaemonStartModeArg, DaemonTriggerCommandArg, SearchBackendArg};

mod model_contract;
#[cfg(test)]
use model_contract::*;
#[allow(unused_imports)]
pub(crate) use model_contract::{
    semantic_managed_model_snapshot_dir, semantic_provisioning_coreml_asset_matches,
    semantic_provisioning_model_contract_matches, semantic_provisioning_model_path_count,
    semantic_provisioning_model_path_matches, semantic_required_model_file_count,
    semantic_required_model_file_matches, SemanticOrtModelVariant,
};
mod runtime_limits;
#[cfg(test)]
use runtime_limits::*;
#[allow(unused_imports)]
pub(crate) use runtime_limits::{
    DAEMON_IDLE_EXIT_SECONDS_CAP, SEMANTIC_CHUNK_OVERLAP_CHARS, SEMANTIC_WORKER_BATCH_MAX,
    SEMANTIC_WORKER_MAX_SECONDS_CAP,
};
mod reports;
#[cfg(test)]
use reports::*;
pub(crate) use reports::{semantic_worker_report_configured_json, SemanticRetrievalReport};
mod vector_store;
#[cfg(test)]
use vector_store::*;
mod query_service;
#[cfg(test)]
use query_service::*;
pub(crate) use query_service::{
    search_packet_with_backend, semantic_query_service_supported, wait_for_daemon_query_service,
    SourceBackedSemanticNotReady,
};
#[cfg(any(target_os = "macos", test))]
mod model_acquisition;
#[cfg(any(target_os = "macos", test))]
mod model_bundle;
mod resource_policy;
#[cfg(test)]
use resource_policy::*;
mod cache_paths;
mod model_runtime;
#[cfg(test)]
use model_runtime::*;
#[allow(unused_imports)]
pub(crate) use model_runtime::{
    prepare_platform_semantic_acceleration, semantic_native_accelerator_target,
    SemanticNativeAcceleratorTarget,
};
mod paths_status;
mod vector_store_reconciliation;
mod vector_store_schema;
#[cfg(test)]
use vector_store_schema::{SemanticVectorStoreError, SEMANTIC_VECTOR_SCHEMA_VERSION};
mod vector_store_search;
mod vector_store_state;
#[cfg(test)]
use paths_status::*;
#[allow(unused_imports)]
pub(crate) use paths_status::{
    daemon_report, semantic_worker_report, semantic_worker_report_best_effort,
    semantic_worker_report_cached,
};
mod daemon;
pub(crate) use daemon::run_daemon_command;
#[cfg(test)]
use daemon::*;
mod daemon_retry;
mod daemon_status;
#[cfg(test)]
use daemon_retry::*;
mod source_status;
pub(crate) use source_status::{source_epoch_status_report, SourceEpochStatus};
mod source_backed_pro_catch_up;
mod source_backed_refresh_coordinator;
mod source_backed_relational_catch_up;
#[allow(unused_imports)] // Provider-neutral executor types are the capture coordinator seam.
pub(crate) use source_backed_refresh_coordinator::{
    coordinate_source_backed_refresh, PinnedSourceBackedGeneration,
    SourceBackedRefreshDaemonUnavailable, SourceBackedRefreshExecution,
    SourceBackedRefreshExecutor, SourceBackedRefreshMode, SourceBackedRefreshObservation,
    SourceBackedRefreshPublication,
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
    complete_replacement_daemon_handoff, daemon_autostart_can_reuse_existing,
    daemon_autostart_suppression_reason, finish_replacement_daemon_handoff,
    mark_replacement_helper_handoff, maybe_autostart_daemon, maybe_autostart_daemon_for_search,
    DaemonHandoff, DaemonUpgradeHandoff,
};
mod health_search;
pub(crate) use health_search::semantic_health_findings;
pub(crate) use health_search::semantic_worker_cache_dir;
#[cfg(test)]
use health_search::*;
#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
pub(crate) fn semantic_runtime_cache_dir(data_root: &std::path::Path) -> std::path::PathBuf {
    let model_cache_dir = semantic_worker_cache_dir(data_root);
    semantic_runtime_cache_dir_for_model_cache(&model_cache_dir)
}

fn semantic_runtime_cache_dir_for_model_cache(
    model_cache_dir: &std::path::Path,
) -> std::path::PathBuf {
    if model_cache_dir.file_name().and_then(|name| name.to_str()) == Some("semantic-model-cache") {
        return model_cache_dir
            .parent()
            .map(|parent| parent.join("runtime"))
            .unwrap_or_else(|| model_cache_dir.join("semantic-runtime"));
    }
    model_cache_dir.join("semantic-runtime")
}

#[cfg(test)]
mod cache_dir_contract_tests {
    use super::semantic_runtime_cache_dir_for_model_cache;
    use std::path::Path;

    #[test]
    fn semantic_runtime_cache_tracks_the_selected_model_cache() {
        assert_eq!(
            semantic_runtime_cache_dir_for_model_cache(Path::new("/data/semantic-model-cache")),
            Path::new("/data/runtime")
        );
        assert_eq!(
            semantic_runtime_cache_dir_for_model_cache(Path::new("/override/cache")),
            Path::new("/override/cache/semantic-runtime")
        );
        assert_eq!(
            semantic_runtime_cache_dir_for_model_cache(Path::new("/override/semantic-model-cache")),
            Path::new("/override/runtime")
        );
    }
}
mod indexing;
#[cfg(test)]
use indexing::*;
#[cfg(all(test, ctx_semantic_fastembed))]
mod fastembed_policy_tests;
#[cfg(test)]
mod query_service_transport_tests;
#[cfg(all(test, ctx_semantic_fastembed))]
mod tests;
#[cfg(all(test, not(ctx_semantic_fastembed)))]
mod unsupported_platform_tests;
