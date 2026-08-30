//! Product daemon service orchestration behind host-owned authority ports.

#[cfg(test)]
use std::{
    fs,
    path::Path,
    process,
    sync::Arc,
    time::{Duration as StdDuration, Instant},
};
#[cfg(all(unix, test))]
use std::{
    io::Write,
    net::Shutdown,
    os::{unix::ffi::OsStrExt, unix::net::UnixStream},
};

#[cfg(test)]
use anyhow::{anyhow, Context, Result};
#[cfg(test)]
use ctx_semantic_model::SharedSemanticRuntime;
#[cfg(test)]
use serde_json::{json, Value};

mod daemon;
mod daemon_process_signal;
mod daemon_retry;
mod daemon_scheduler;
mod daemon_wakeup;
mod daemon_worker;
mod paths_status;
mod ports;
mod query_service;
mod resource_policy;
mod runtime_limits;
mod source_backed_refresh_adapter;
mod source_backed_refresh_coordinator;
#[cfg(test)]
mod test_support;

pub(crate) mod analytics {
    pub(crate) use ctx_client_observability::analytics::*;
}

pub(crate) mod config {
    pub(crate) use crate::{DaemonConfigSnapshot as AppConfig, DaemonMode};
    pub(crate) const CONFIG_FILE: &str = crate::CONFIG_FILE;
}

#[cfg(all(test, unix))]
use ctx_daemon_runtime::read_daemon_query_response_unix;
#[cfg(test)]
use ctx_daemon_runtime::{
    daemon_query_roundtrip, daemon_query_unix_io_error_is_pre_submission_unavailable,
    daemon_query_windows_io_error_is_pre_submission_unavailable,
    read_bounded_daemon_request as read_daemon_query_request, DaemonQueryResponseTooLarge,
};
#[cfg(test)]
use paths_status::*;
#[cfg(test)]
use query_service::*;
#[cfg(test)]
use source_backed_refresh_coordinator::SourceBackedRefreshPublication;

#[cfg(test)]
mod query_service_transport_tests;

pub use daemon::run_daemon;
pub use daemon_wakeup::daemon_wakeup_report;
pub use paths_status::{
    daemon_core_refresh_job_path, daemon_semantic_job_path, daemon_source_backed_refresh_job_path,
};
pub use ports::*;
pub(crate) use ports::{
    DaemonStartMode as DaemonStartModeArg, DaemonTrigger as DaemonTriggerCommandArg,
};
pub use query_service::{
    daemon_query_request, daemon_service_endpoint_path, daemon_source_refresh_request,
    read_daemon_service_endpoint_identity, DaemonIpcService, DaemonQueryEndpoint,
    DaemonQueryServiceUnavailable, DaemonSourceRefreshServiceUnavailable,
    DAEMON_SEMANTIC_QUERY_SCHEMA_VERSION,
};
pub use runtime_limits::SEMANTIC_WORKER_BATCH_MAX;
pub use source_backed_refresh_coordinator::{
    coordinate_import_source_backed_refresh_with_progress,
    coordinate_setup_source_backed_refresh_with_progress, coordinate_source_backed_refresh,
    coordinate_source_backed_refresh_with_progress, pin_active_verified_generation,
    published_explicit_source_relocation_authority, PinnedSourceBackedGeneration, RefreshSelection,
    RefreshStatus, SourceBackedCurrentSourceProgress, SourceBackedRefreshDaemonUnavailable,
    SourceBackedRefreshMode, SourceBackedRefreshObservation, SourceBackedRefreshPendingPublication,
    SourceBackedRefreshTerminalError,
};

#[cfg(feature = "test-support")]
pub mod testing {
    pub use crate::source_backed_refresh_coordinator::{
        recover_wait_refresh_request_for_test, SourceRefreshObservationRecoveryFailed,
    };

    pub fn write_daemon_lifecycle_status(
        data_root: &std::path::Path,
        status: &serde_json::Value,
    ) -> anyhow::Result<()> {
        super::paths_status::write_daemon_status(data_root, status)
    }

    pub fn write_core_refresh_status(
        data_root: &std::path::Path,
        status: &serde_json::Value,
    ) -> anyhow::Result<()> {
        super::paths_status::write_daemon_job_status(
            &super::paths_status::daemon_core_refresh_job_path(data_root),
            status,
        )
    }

    pub fn write_daemon_service_endpoint(
        data_root: &std::path::Path,
        service: crate::DaemonIpcService,
        endpoint: &crate::DaemonQueryEndpoint,
    ) -> anyhow::Result<()> {
        super::query_service::write_daemon_service_endpoint(data_root, service, endpoint)
    }
}

fn compact_json(mut value: serde_json::Value) -> serde_json::Value {
    prune_null_json(&mut value);
    value
}

const CONFIG_FILE: &str = "config.toml";

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

fn prune_null_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.retain(|_, nested| {
                prune_null_json(nested);
                !nested.is_null()
            });
        }
        serde_json::Value::Array(items) => {
            for item in items {
                prune_null_json(item);
            }
        }
        _ => {}
    }
}
