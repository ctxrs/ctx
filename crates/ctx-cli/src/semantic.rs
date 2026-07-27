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
use rusqlite::{params, Connection};
#[cfg(test)]
use serde_json::{json, Value};
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
use ctx_history_core::{database_path, utc_now};
#[cfg(test)]
use ctx_history_store::{EventEmbeddingDocument, Store};

#[cfg(test)]
use crate::commands::{
    import::{import_totals_json, ImportTotals},
    search::RefreshArg,
};
#[cfg(test)]
use crate::config::CONFIG_FILE;
#[cfg(test)]
use crate::output::compact_json;
#[cfg(test)]
use crate::{DaemonRunArgs, DaemonStartModeArg, DaemonTriggerCommandArg, SearchBackendArg};

mod model_contract;
#[cfg(test)]
use model_contract::*;
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
mod paths_status;
mod vector_store_schema;
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
#[cfg(test)]
use daemon_retry::*;
mod daemon_scheduler;
#[cfg(test)]
use daemon_scheduler::*;
mod daemon_worker;
#[cfg(test)]
use daemon_worker::*;
mod daemon_autostart;
pub(crate) use daemon_autostart::{maybe_autostart_daemon, maybe_autostart_daemon_for_search};
mod daemon_history;
#[cfg(test)]
use daemon_history::*;
mod health_search;
pub(crate) use health_search::semantic_health_findings;
#[cfg(test)]
use health_search::*;
mod indexing;
#[cfg(test)]
use indexing::*;
#[cfg(all(test, ctx_semantic_fastembed))]
mod fastembed_policy_tests;
#[cfg(test)]
mod query_service_transport_tests;
#[cfg(all(test, ctx_sqlite_vec))]
mod tests;
#[cfg(all(test, not(ctx_semantic_fastembed)))]
mod unsupported_platform_tests;
