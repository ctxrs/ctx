use std::{
    collections::{HashMap, HashSet},
    env, fmt, fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant, SystemTime},
};

#[cfg(unix)]
use std::net::Shutdown;
#[cfg(unix)]
use std::os::unix::{
    ffi::OsStrExt,
    fs::{OpenOptionsExt, PermissionsExt},
};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(ctx_sqlite_vec)]
use std::os::raw::c_char;
#[cfg(ctx_sqlite_vec)]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Once,
};

use anyhow::{anyhow, Context, Result};
use rusqlite::{
    params, params_from_iter, types::Value as SqlValue, Connection, OpenFlags, OptionalExtension,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use ctx_history_core::{database_path, default_data_root, utc_now};
use ctx_history_store::{EventEmbeddingDocument, Store};

use crate::commands::{
    import::{error_summary, import_totals_json, ImportTotals},
    search::{
        refresh_sources_for_search, search_refresh_plugin_sources, search_refresh_sources,
        RefreshArg,
    },
};
use crate::config::{self, AppConfig, CONFIG_FILE};
use crate::output::{compact_json, print_json};
use crate::store_util::open_existing_store_read_only;
use crate::{
    DaemonArgs, DaemonCommand, DaemonRunArgs, DaemonStartModeArg, DaemonTriggerCommandArg,
    JsonArgs, SearchBackendArg,
};

const SEMANTIC_BACKEND: &str = "multilingual-e5";
const SEMANTIC_MODEL_KEY: &str = "e5-small-v1:mean-pool:l2:query-passage";
const SEMANTIC_MODEL_ID: &str = "intfloat/multilingual-e5-small";
const SEMANTIC_MODEL_REVISION: &str = "614241f622f53c4eeff9890bdc4f31cfecc418b3";
const SEMANTIC_HF_MODEL_CACHE_DIR: &str = "models--intfloat--multilingual-e5-small";
const SEMANTIC_MANAGED_MODEL_CACHE_DIR: &str = "ctx-semantic-models";
const SEMANTIC_REQUIRED_MODEL_FILES: &[SemanticModelFile] = &[
    SemanticModelFile::new(
        "onnx/model.onnx",
        470_268_510,
        "ca456c06b3a9505ddfd9131408916dd79290368331e7d76bb621f1cba6bc8665",
    ),
    SemanticModelFile::new(
        "tokenizer.json",
        17_082_730,
        "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
    ),
    SemanticModelFile::new(
        "config.json",
        655,
        "69137736cab8b8903a07fe8afaafdda25aac55415a12a55d1bffa9f581abf959",
    ),
    SemanticModelFile::new(
        "special_tokens_map.json",
        167,
        "d05497f1da52c5e09554c0cd874037a083e1dc1b9cfd48034d1c717f1afc07a7",
    ),
    SemanticModelFile::new(
        "tokenizer_config.json",
        443,
        "a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b",
    ),
];
const SEMANTIC_DIMENSIONS: usize = 384;
const SEMANTIC_PASSAGE_PREFIX: &str = "passage: ";
const SEMANTIC_QUERY_PREFIX: &str = "query: ";

#[derive(Clone, Copy, Debug)]
struct SemanticModelFile {
    path: &'static str,
    size: u64,
    sha256: &'static str,
}

impl SemanticModelFile {
    const fn new(path: &'static str, size: u64, sha256: &'static str) -> Self {
        Self { path, size, sha256 }
    }
}

#[derive(Debug)]
struct SemanticCpuModelIntegrityError(String);

impl fmt::Display for SemanticCpuModelIntegrityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SemanticCpuModelIntegrityError {}

#[derive(Debug)]
struct SemanticCpuModelCacheMissing(String);

impl fmt::Display for SemanticCpuModelCacheMissing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SemanticCpuModelCacheMissing {}

#[derive(Debug)]
struct SemanticModelLoadDeferred {
    available_memory_bytes: u64,
    required_available_memory_bytes: u64,
}

impl fmt::Display for SemanticModelLoadDeferred {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "semantic CPU model load deferred: {} bytes available, {} required",
            self.available_memory_bytes, self.required_available_memory_bytes
        )
    }
}

impl std::error::Error for SemanticModelLoadDeferred {}

fn semantic_model_key() -> &'static str {
    SEMANTIC_MODEL_KEY
}
const SEMANTIC_SEARCH_CANDIDATES: usize = 200;
const SEMANTIC_SOFT_FILTER_SEARCH_CANDIDATES: usize = 1_000;
const SEMANTIC_CHUNK_TARGET_CHARS: usize = 1_200;
pub(crate) const SEMANTIC_CHUNK_OVERLAP_CHARS: usize = 200;
const SEMANTIC_SOURCE_MAX_CHARS: usize = 64 * 1024;
const SEMANTIC_VECTOR_OVERFETCH: usize = 4;
const SEMANTIC_FULL_SCAN_MAX_CHUNKS: usize = 250_000;
const SEMANTIC_FULL_SCAN_MAX_VECTOR_BYTES: usize = 512 * 1024 * 1024;
const SEMANTIC_VECTOR_BACKEND_RUST: &str = "rust_blob_scan";
const SEMANTIC_VECTOR_BACKEND_SQLITE_VEC: &str = "sqlite_vec0";
const SEMANTIC_SQLITE_VEC0_MAX_K: usize = 4_096;
#[cfg(ctx_semantic_fastembed)]
const SEMANTIC_EMBED_THREADS_MAX: usize = 8;
#[cfg(ctx_semantic_fastembed)]
const SEMANTIC_EMBED_BATCH_MAX: usize = 512;
const SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT: usize = 512;
const SEMANTIC_WORKER_LOCK_FILE: &str = "semantic-worker.lock";
const SEMANTIC_WORKER_STATUS_FILE: &str = "semantic-worker.json";
const SEMANTIC_WORKER_BATCH_DEFAULT: usize = 5_000;
pub(crate) const SEMANTIC_WORKER_BATCH_MAX: usize = 1_000_000;
const SEMANTIC_WORKER_MAX_SECONDS_DEFAULT: u64 = 60;
pub(crate) const SEMANTIC_WORKER_MAX_SECONDS_CAP: u64 = 86_400;
const SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS: u64 = 15;
const SEMANTIC_VECTOR_BUSY_TIMEOUT_MS: u64 = 30_000;
const SEMANTIC_PRUNE_EVENTS_PER_PASS: usize = 256;
const SEMANTIC_PRUNE_EVENT_BATCH: usize = 1_000;
const SEMANTIC_DEADLINE_CHUNKS_PER_SECOND: usize = 3;
const SEMANTIC_DEADLINE_MIN_CHUNK_BATCH: usize = 16;
const DAEMON_DIR: &str = "daemon";
const DAEMON_JOBS_DIR: &str = "jobs";
const DAEMON_LOCK_FILE: &str = "daemon.lock";
const DAEMON_STATUS_FILE: &str = "status.json";
#[cfg(unix)]
const DAEMON_QUERY_SOCKET_FILE: &str = "query.sock";
const DAEMON_QUERY_ENDPOINT_FILE: &str = "query-endpoint.json";
const DAEMON_HISTORY_REFRESH_JOB_FILE: &str = "history-refresh.json";
const DAEMON_SEMANTIC_JOB_FILE: &str = "semantic-index.json";
const DAEMON_CLOUD_SYNC_JOB_FILE: &str = "cloud-sync.json";
const DAEMON_IDLE_EXIT_SECONDS_DEFAULT: u64 = 30;
pub(crate) const DAEMON_IDLE_EXIT_SECONDS_CAP: u64 = 24 * 60 * 60;
const DAEMON_LOOP_INTERVAL_SECONDS_DEFAULT: u64 = 5;
const DAEMON_AUTOSTART_IDLE_EXIT_SECONDS_DEFAULT: u64 = 5;
const DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS_DEFAULT: u64 = 5;
const DAEMON_BACKGROUND_CHILD_ENV: &str = "CTX_DAEMON_BACKGROUND_CHILD";
const DAEMON_AUTOSTART_OFF_ENV: &str = "CTX_DAEMON_AUTOSTART_OFF";
const DAEMON_SEMANTIC_BOOTSTRAP_PASSES_BEFORE_REFRESH: usize = 1;
const DAEMON_LOCK_STALE_AFTER_MS: i64 = 25 * 60 * 60 * 1_000;
const PID_LOCK_INCOMPLETE_GRACE: StdDuration = StdDuration::from_secs(30);
const PID_LOCK_PROTOCOL: &str = "advisory-v1";
const PID_LOCK_ACQUIRE_ATTEMPTS: usize = 20;
const PID_LOCK_ACQUIRE_RETRY: StdDuration = StdDuration::from_millis(2);
const DAEMON_SEMANTIC_RESERVE_GRACE_SECS: u64 = 10;
const DAEMON_MIN_REMAINING_FOR_JOB_SECS: u64 = 2;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SemanticReportCountMode {
    ExactOnCacheMiss,
    CachedOrStatusFile,
}

#[derive(Debug, Clone)]
pub(crate) struct SemanticWorkerReport {
    status: String,
    running: bool,
    pid: Option<u32>,
    started_at_ms: Option<i64>,
    heartbeat_at_ms: Option<i64>,
    finished_at_ms: Option<i64>,
    indexed_chunks: Option<usize>,
    model_init_ms: Option<usize>,
    last_error: Option<String>,
    searchable_items: usize,
    searchable_items_known: bool,
    embedded_items: usize,
    embedded_chunks: usize,
    dirty_items: usize,
    queued_items_estimate: usize,
    model_cache_available: bool,
    model_acquisition: Value,
    embed_policy: Option<Value>,
    embedding_runtime: Option<Value>,
    vector_path: PathBuf,
    lock_path: PathBuf,
    status_path: PathBuf,
}

impl SemanticWorkerReport {
    fn unavailable(data_root: &Path, error: impl ToString) -> Self {
        Self {
            status: "unavailable".to_owned(),
            running: false,
            pid: None,
            started_at_ms: None,
            heartbeat_at_ms: None,
            finished_at_ms: None,
            indexed_chunks: None,
            model_init_ms: None,
            last_error: Some(error.to_string()),
            searchable_items: 0,
            searchable_items_known: false,
            embedded_items: 0,
            embedded_chunks: 0,
            dirty_items: 0,
            queued_items_estimate: 0,
            model_cache_available: semantic_model_cache_available(&semantic_worker_cache_dir(
                data_root,
            )),
            model_acquisition: semantic_model_acquisition_status_json(
                &semantic_worker_cache_dir(data_root),
            ),
            embed_policy: Some(semantic_embed_policy_status_json()),
            embedding_runtime: None,
            vector_path: semantic_vector_path(data_root),
            lock_path: semantic_worker_lock_path(data_root),
            status_path: semantic_worker_status_path(data_root),
        }
    }

    fn coverage_ratio(&self) -> Option<f64> {
        if !self.searchable_items_known || self.searchable_items == 0 {
            None
        } else {
            Some((self.embedded_items as f64 / self.searchable_items as f64).min(1.0))
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        compact_json(json!({
            "status": self.status,
            "model_key": semantic_model_key(),
            "running": self.running,
            "pid": self.pid,
            "started_at_ms": self.started_at_ms,
            "heartbeat_at_ms": self.heartbeat_at_ms,
            "finished_at_ms": self.finished_at_ms,
            "indexed_chunks": self.indexed_chunks,
            "model_init_ms": self.model_init_ms,
            "last_error": self.last_error,
            "coverage": {
                "searchable_items": self.searchable_items,
                "searchable_items_known": self.searchable_items_known,
                "embedded_items": self.embedded_items,
                "embedded_chunks": self.embedded_chunks,
                "dirty_items": self.dirty_items,
                "queued_items_estimate": self.queued_items_estimate,
                "coverage_ratio": self.coverage_ratio(),
            },
            "model_cache_available": self.model_cache_available,
            "model_acquisition": self.model_acquisition.clone(),
            "embed_policy": self.embed_policy.clone(),
            "embedding_runtime": self.embedding_runtime.clone(),
            "vector_path": self.vector_path.display().to_string(),
            "lock_path": self.lock_path.display().to_string(),
            "status_path": self.status_path.display().to_string(),
        }))
    }
}

pub(crate) fn semantic_worker_report_configured_json(
    config: &AppConfig,
    report: &SemanticWorkerReport,
) -> Value {
    let enabled = config.semantic_search_enabled();
    let mut value = report.to_json();
    if let Some(object) = value.as_object_mut() {
        object.insert("enabled".to_owned(), json!(enabled));
        object.insert(
            "config_source".to_owned(),
            json!(config.semantic_search_source()),
        );
        if !enabled {
            object.insert("status".to_owned(), json!("disabled"));
            object.insert("reason".to_owned(), json!("semantic_disabled"));
        } else if !semantic_query_service_supported() {
            object.insert("status".to_owned(), json!("blocked"));
            object.insert("reason".to_owned(), json!("unsupported_platform"));
        } else if !config.daemon.enabled {
            object.insert("status".to_owned(), json!("blocked"));
            object.insert("reason".to_owned(), json!("daemon_disabled"));
        }
    }
    value
}

#[derive(Debug, Clone)]
pub(crate) struct SemanticRetrievalReport {
    requested_mode: SearchBackendArg,
    effective_mode: SearchBackendArg,
    semantic_weight: f32,
    semantic_status: &'static str,
    semantic_fallback_code: Option<&'static str>,
    semantic_fallback: Option<String>,
    embedding_model: Option<String>,
    embedded_items: usize,
    embedded_chunks: usize,
    searchable_items: usize,
    indexed_now: usize,
    vector_path: Option<PathBuf>,
    worker: Option<SemanticWorkerReport>,
    diagnostics: Option<SemanticRetrievalDiagnostics>,
}

impl SemanticRetrievalReport {
    pub(crate) fn lexical(requested_mode: SearchBackendArg, searchable_items: usize) -> Self {
        Self {
            requested_mode,
            effective_mode: SearchBackendArg::Lexical,
            semantic_weight: 0.0,
            semantic_status: "skipped",
            semantic_fallback_code: None,
            semantic_fallback: None,
            embedding_model: None,
            embedded_items: 0,
            embedded_chunks: 0,
            searchable_items,
            indexed_now: 0,
            vector_path: None,
            worker: None,
            diagnostics: None,
        }
    }

    fn apply_worker_counts(&mut self, worker: &SemanticWorkerReport) {
        self.searchable_items = worker.searchable_items;
        self.embedded_items = worker.embedded_items;
        self.embedded_chunks = worker.embedded_chunks;
    }

    fn apply_worker_coverage(&mut self, worker: &SemanticWorkerReport) {
        self.apply_worker_counts(worker);
        self.semantic_status = semantic_status_from_worker(worker);
    }

    fn set_semantic_fallback(&mut self, code: &'static str, message: impl Into<String>) {
        self.semantic_fallback_code = Some(code);
        self.semantic_fallback = Some(message.into());
    }

    pub(crate) fn to_json(&self) -> Value {
        compact_json(json!({
            "requested_mode": self.requested_mode.as_str(),
            "effective_mode": self.effective_mode.as_str(),
            "semantic_weight": self.semantic_weight,
            "semantic_status": self.semantic_status,
            "semantic_fallback_code": self.semantic_fallback_code,
            "semantic_fallback": self.semantic_fallback,
            "embedding_model": self.embedding_model,
            "coverage": {
                "embedded_items": self.embedded_items,
                "embedded_chunks": self.embedded_chunks,
                "searchable_items": self.searchable_items,
                "searchable_items_known": self.worker.as_ref().map(|worker| worker.searchable_items_known),
                "indexed_now": self.indexed_now,
                "dirty_items": self.worker.as_ref().map(|worker| worker.dirty_items),
            },
            "vector_path": self.vector_path.as_ref().map(|path| path.display().to_string()),
            "worker": self.worker.as_ref().map(SemanticWorkerReport::to_json),
            "diagnostics": self.diagnostics.as_ref().map(SemanticRetrievalDiagnostics::to_json),
        }))
    }

    pub(crate) fn effective_mode(&self) -> SearchBackendArg {
        self.effective_mode
    }
}

fn semantic_status_from_worker(worker: &SemanticWorkerReport) -> &'static str {
    if !worker.searchable_items_known || worker.searchable_items == 0 || worker.embedded_items == 0 {
        "unavailable"
    } else if semantic_worker_coverage_ready(worker) {
        "ready"
    } else {
        "partial"
    }
}

fn semantic_worker_coverage_ready(worker: &SemanticWorkerReport) -> bool {
    worker.searchable_items_known
        && worker.searchable_items > 0
        && worker.embedded_items >= worker.searchable_items
        && worker.dirty_items == 0
}

#[derive(Debug, Clone, Default)]
struct SemanticRetrievalDiagnostics {
    vector_backend: Option<&'static str>,
    query_embed_ms: Option<u64>,
    vector_scan_ms: Option<u64>,
    chunks_scanned: Option<usize>,
    vector_bytes_read: Option<usize>,
    events_scored: Option<usize>,
    hydration_ms: Option<u64>,
    stale_events_dropped: Option<usize>,
    semantic_candidates: Option<usize>,
}

impl SemanticRetrievalDiagnostics {
    fn to_json(&self) -> Value {
        compact_json(json!({
            "vector_backend": self.vector_backend,
            "query_embed_ms": self.query_embed_ms,
            "vector_scan_ms": self.vector_scan_ms,
            "chunks_scanned": self.chunks_scanned,
            "vector_bytes_read": self.vector_bytes_read,
            "events_scored": self.events_scored,
            "hydration_ms": self.hydration_ms,
            "stale_events_dropped": self.stale_events_dropped,
            "semantic_candidates": self.semantic_candidates,
        }))
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn search_packet_with_backend(
    store: &Store,
    data_root: &Path,
    query: &str,
    terms: &[String],
    options: &ctx_history_search::PacketOptions,
    requested_backend: SearchBackendArg,
    semantic_enabled: bool,
    semantic_weight: f32,
    _refresh_mode: RefreshArg,
    emit_warnings: bool,
) -> Result<(ctx_history_search::SearchPacket, SemanticRetrievalReport)> {
    let uses_composed_terms = terms.iter().any(|term| !term.trim().is_empty());
    let semantic_text = semantic_query_text(query, terms);
    let mut effective_backend = requested_backend;

    let filters_require_semantic_fallback =
        matches!(
            effective_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        ) && semantic_filters_require_lexical_fallback(&options.filters);
    let terms_require_semantic_fallback = matches!(
        effective_backend,
        SearchBackendArg::Semantic | SearchBackendArg::Hybrid
    ) && uses_composed_terms;
    if filters_require_semantic_fallback && requested_backend == SearchBackendArg::Semantic {
        return Err(anyhow!(
            "semantic search does not yet support these filters; use --backend hybrid or --backend lexical"
        ));
    }
    if terms_require_semantic_fallback && requested_backend == SearchBackendArg::Semantic {
        return Err(anyhow!(
            "semantic search does not yet preserve --term OR semantics; use --backend hybrid or --backend lexical"
        ));
    }
    if filters_require_semantic_fallback || terms_require_semantic_fallback {
        effective_backend = SearchBackendArg::Lexical;
    }

    let lexical_search_packet = || -> Result<ctx_history_search::SearchPacket> {
        if uses_composed_terms {
            ctx_history_search::search_packet_terms(store, query, terms, options)
                .map_err(Into::into)
        } else {
            ctx_history_search::search_packet(store, query, options).map_err(Into::into)
        }
    };

    if !semantic_enabled
        && matches!(
            requested_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        )
    {
        if requested_backend == SearchBackendArg::Semantic {
            return Err(anyhow!(
                "semantic search is disabled. Set [search] semantic = true in ctx config to enable the local semantic preview"
            ));
        }
        let mut retrieval = SemanticRetrievalReport::lexical(requested_backend, 0);
        retrieval.effective_mode = SearchBackendArg::Lexical;
        retrieval.semantic_weight = 0.0;
        retrieval.semantic_status = "disabled";
        retrieval.set_semantic_fallback(
            "semantic_disabled",
            "local semantic search is disabled by configuration",
        );
        warn_if(
            emit_warnings,
            "warning: local semantic search is disabled; falling back to lexical search",
        );
        return Ok((lexical_search_packet()?, retrieval));
    }

    if !semantic_query_service_supported()
        && matches!(
            requested_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        )
    {
        if requested_backend == SearchBackendArg::Semantic {
            return Err(anyhow!(
                "local semantic search is not supported on this platform yet"
            ));
        }
        let mut retrieval = SemanticRetrievalReport::lexical(requested_backend, 0);
        retrieval.effective_mode = SearchBackendArg::Lexical;
        retrieval.semantic_weight = 0.0;
        retrieval.semantic_status = "unavailable";
        retrieval.set_semantic_fallback(
            "unsupported_platform",
            "local semantic search is not supported on this platform yet",
        );
        warn_if(
            emit_warnings,
            "warning: local semantic search is not supported on this platform; falling back to lexical search",
        );
        return Ok((lexical_search_packet()?, retrieval));
    }

    let semantic_cache_dir = semantic_worker_cache_dir(data_root);
    let vector_path = semantic_vector_path(data_root);

    let worker_report = if matches!(
        effective_backend,
        SearchBackendArg::Semantic | SearchBackendArg::Hybrid
    ) {
        semantic_worker_report_cached(data_root, Some(store))?
    } else {
        semantic_worker_report_best_effort(data_root)
    };
    let searchable_items = worker_report.searchable_items;
    let mut retrieval = SemanticRetrievalReport::lexical(requested_backend, searchable_items);
    retrieval.worker = Some(worker_report.clone());
    retrieval.apply_worker_counts(&worker_report);
    if matches!(
        requested_backend,
        SearchBackendArg::Semantic | SearchBackendArg::Hybrid
    ) {
        retrieval.apply_worker_coverage(&worker_report);
    }

    if matches!(
        effective_backend,
        SearchBackendArg::Semantic | SearchBackendArg::Hybrid
    ) && semantic_text.trim().is_empty()
    {
        return Err(anyhow!(
            "semantic search needs a text query; add a query or --term"
        ));
    }

    if filters_require_semantic_fallback
        && matches!(
            requested_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        )
    {
        retrieval.set_semantic_fallback(
            "filtered_vector_lookup_unsupported",
            "semantic search does not yet support filtered vector lookup",
        );
        warn_if(
            emit_warnings,
            "warning: semantic search does not yet support these filters; falling back to lexical search",
        );
    } else if terms_require_semantic_fallback
        && matches!(
            requested_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        )
    {
        retrieval.set_semantic_fallback(
            "term_or_semantics_unsupported",
            "semantic search does not yet preserve --term OR semantics",
        );
        warn_if(
            emit_warnings,
            "warning: semantic search does not yet preserve --term OR semantics; falling back to lexical search",
        );
    }

    let packet = if matches!(
        effective_backend,
        SearchBackendArg::Semantic | SearchBackendArg::Hybrid
    ) {
        semantic_or_hybrid_search_packet(
            data_root,
            store,
            options,
            &lexical_search_packet,
            &mut retrieval,
            &worker_report,
            &vector_path,
            &semantic_cache_dir,
            &semantic_text,
            effective_backend,
            semantic_weight,
            emit_warnings,
        )?
    } else {
        lexical_search_packet()?
    };

    Ok((packet, retrieval))
}

#[allow(clippy::too_many_arguments)]
fn semantic_or_hybrid_search_packet(
    data_root: &Path,
    store: &Store,
    options: &ctx_history_search::PacketOptions,
    lexical_search_packet: &dyn Fn() -> Result<ctx_history_search::SearchPacket>,
    retrieval: &mut SemanticRetrievalReport,
    worker_report: &SemanticWorkerReport,
    vector_path: &Path,
    semantic_cache_dir: &Path,
    semantic_text: &str,
    effective_backend: SearchBackendArg,
    semantic_weight: f32,
    emit_warnings: bool,
) -> Result<ctx_history_search::SearchPacket> {
    match SemanticVectorStore::open_read_only(vector_path) {
        Ok(Some(vector_store)) => {
            *retrieval = SemanticRetrievalReport {
                requested_mode: retrieval.requested_mode,
                effective_mode: effective_backend,
                semantic_weight: if effective_backend == SearchBackendArg::Hybrid {
                    semantic_weight
                } else {
                    1.0
                },
                semantic_status: semantic_status_from_worker(worker_report),
                semantic_fallback_code: None,
                semantic_fallback: None,
                embedding_model: Some(SEMANTIC_MODEL_ID.to_owned()),
                embedded_items: worker_report.embedded_items,
                embedded_chunks: worker_report.embedded_chunks,
                searchable_items: worker_report.searchable_items,
                indexed_now: 0,
                vector_path: Some(vector_path.to_path_buf()),
                worker: Some(worker_report.clone()),
                diagnostics: None,
            };

            if worker_report.embedded_items == 0 {
                if effective_backend == SearchBackendArg::Semantic {
                    if !worker_report.model_cache_available
                        || !semantic_model_cache_available(semantic_cache_dir)
                    {
                        return Err(anyhow!(
                            "semantic index has no embedded event chunks and semantic model is not available in the local cache; semantic-only search will not initialize or download {SEMANTIC_MODEL_ID} during search"
                        ));
                    }
                    return Err(anyhow!(
                        "semantic index has no embedded event chunks yet; ctx search does not start semantic indexing"
                    ));
                }
                retrieval.effective_mode = SearchBackendArg::Lexical;
                retrieval.semantic_weight = 0.0;
                retrieval.embedding_model = None;
                retrieval.set_semantic_fallback(
                    "semantic_index_empty",
                    "semantic index has no embedded event chunks",
                );
                warn_if(
                    emit_warnings,
                    "warning: semantic index is empty; falling back to lexical search",
                );
                return lexical_search_packet();
            }

            if effective_backend == SearchBackendArg::Hybrid
                && (!worker_report.searchable_items_known
                    || !semantic_hybrid_coverage_ready(
                        worker_report.embedded_items,
                        worker_report.searchable_items,
                        worker_report.dirty_items,
                    ))
            {
                retrieval.effective_mode = SearchBackendArg::Lexical;
                retrieval.semantic_weight = 0.0;
                retrieval.embedding_model = None;
                if worker_report.searchable_items_known {
                    retrieval.set_semantic_fallback(
                        "semantic_coverage_not_ready",
                        format!(
                            "semantic coverage is incomplete or dirty for hybrid ranking ({}/{} items embedded, {} dirty)",
                            worker_report.embedded_items,
                            worker_report.searchable_items,
                            worker_report.dirty_items
                        ),
                    );
                } else {
                    retrieval.set_semantic_fallback(
                        "semantic_coverage_unknown",
                        "semantic coverage is not cached yet; wait for the daemon to refresh indexing status",
                    );
                }
                warn_if(
                    emit_warnings,
                    "warning: semantic coverage is incomplete or dirty for hybrid ranking; falling back to lexical search",
                );
                return lexical_search_packet();
            }

            if !worker_report.model_cache_available
                || !semantic_model_cache_available(semantic_cache_dir)
            {
                if effective_backend == SearchBackendArg::Semantic {
                    return Err(anyhow!(
                        "semantic model is not available in the local cache; semantic-only search will not initialize or download {SEMANTIC_MODEL_ID} during search"
                    ));
                }
                retrieval.effective_mode = SearchBackendArg::Lexical;
                retrieval.semantic_weight = 0.0;
                retrieval.embedding_model = None;
                retrieval.set_semantic_fallback(
                    "model_cache_missing",
                    "semantic model is not available in the local cache",
                );
                warn_if(
                    emit_warnings,
                    "warning: semantic model is not available in the local cache; falling back to lexical search",
                );
                return lexical_search_packet();
            }

            if !daemon_query_service_available(data_root) {
                let message = "daemon semantic query service is not available; run `ctx daemon run` or use the default background refresh mode to start it";
                if effective_backend == SearchBackendArg::Semantic {
                    return Err(anyhow!("{message}"));
                }
                retrieval.effective_mode = SearchBackendArg::Lexical;
                retrieval.semantic_weight = 0.0;
                retrieval.embedding_model = None;
                retrieval.semantic_status = "unavailable";
                retrieval.set_semantic_fallback("daemon_query_service_unavailable", message);
                warn_if(
                    emit_warnings,
                    "warning: daemon semantic query service is not available; falling back to lexical search",
                );
                return lexical_search_packet();
            }

            let semantic_candidate_limit = if semantic_filters_need_overfetch(&options.filters) {
                SEMANTIC_SOFT_FILTER_SEARCH_CANDIDATES.max(options.limit.saturating_mul(100))
            } else {
                SEMANTIC_SEARCH_CANDIDATES.max(options.limit.saturating_mul(8))
            };
            match semantic_hits_for_text_query(
                data_root,
                store,
                &vector_store,
                semantic_text,
                semantic_candidate_limit,
                None,
            ) {
                Ok((semantic_hits, diagnostics)) => {
                    retrieval.diagnostics = Some(diagnostics);
                    ctx_history_search::semantic_event_search_packet(
                        store,
                        semantic_text,
                        options,
                        &semantic_hits,
                        semantic_weight,
                        effective_backend == SearchBackendArg::Hybrid,
                    )
                    .map_err(Into::into)
                }
                Err(error) => {
                    let error_message = format!("{error:#}");
                    if effective_backend == SearchBackendArg::Semantic {
                        return Err(anyhow!("semantic search failed: {error_message}"));
                    }
                    retrieval.effective_mode = SearchBackendArg::Lexical;
                    retrieval.semantic_weight = 0.0;
                    retrieval.embedding_model = None;
                    retrieval.semantic_status = "unavailable";
                    retrieval.diagnostics = None;
                    if error_message.contains("daemon query")
                        || error_message.contains("daemon semantic query service")
                    {
                        retrieval.set_semantic_fallback(
                            "daemon_query_service_unavailable",
                            format!("daemon semantic query service failed: {error_message}"),
                        );
                    } else {
                        retrieval.set_semantic_fallback(
                            "semantic_retrieval_failed",
                            format!("semantic retrieval failed: {error_message}"),
                        );
                    }
                    warn_if(
                        emit_warnings,
                        "warning: semantic retrieval failed; falling back to lexical search",
                    );
                    lexical_search_packet()
                }
            }
        }
        Ok(None) => {
            if effective_backend == SearchBackendArg::Semantic {
                if !worker_report.model_cache_available
                    || !semantic_model_cache_available(semantic_cache_dir)
                {
                    return Err(anyhow!(
                        "semantic index is not available yet and semantic model is not available in the local cache; semantic-only search will not initialize or download {SEMANTIC_MODEL_ID} during search"
                    ));
                }
                return Err(anyhow!(
                    "semantic index is not available yet; ctx search does not start semantic indexing"
                ));
            }
            retrieval.effective_mode = SearchBackendArg::Lexical;
            retrieval.semantic_weight = 0.0;
            retrieval.embedding_model = None;
            retrieval.set_semantic_fallback(
                "semantic_index_missing",
                "semantic index is not available yet",
            );
            warn_if(
                emit_warnings,
                "warning: semantic index is not available yet; falling back to lexical search",
            );
            lexical_search_packet()
        }
        Err(error) => {
            let message = format!("semantic index could not be opened: {error:#}");
            if effective_backend == SearchBackendArg::Semantic {
                return Err(anyhow!(message));
            }
            retrieval.effective_mode = SearchBackendArg::Lexical;
            retrieval.semantic_weight = 0.0;
            retrieval.embedding_model = None;
            retrieval.semantic_status = "unavailable";
            retrieval.set_semantic_fallback("semantic_index_open_error", message);
            warn_if(
                emit_warnings,
                "warning: semantic index could not be opened; falling back to lexical search",
            );
            lexical_search_packet()
        }
    }
}

fn warn_if(enabled: bool, message: &str) {
    if enabled {
        eprintln!("{message}");
    }
}

#[cfg(ctx_sqlite_vec)]
fn register_sqlite_vec_auto_extension() -> bool {
    static REGISTER: Once = Once::new();
    static AVAILABLE: AtomicBool = AtomicBool::new(false);

    REGISTER.call_once(|| {
        let rc = unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut c_char,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> i32,
            >(
                sqlite_vec::sqlite3_vec_init as *const ()
            )))
        };
        AVAILABLE.store(rc == rusqlite::ffi::SQLITE_OK, Ordering::Relaxed);
    });

    AVAILABLE.load(Ordering::Relaxed)
}

#[cfg(not(ctx_sqlite_vec))]
fn register_sqlite_vec_auto_extension() -> bool {
    false
}

struct SemanticVectorHit {
    event_id: Uuid,
    similarity: f32,
    source_text_hash: String,
    start_char: usize,
    end_char: usize,
}

#[derive(Debug, Clone, Default)]
struct SemanticVectorSearchStats {
    backend: Option<&'static str>,
    scan_ms: u64,
    chunks_scanned: usize,
    vector_bytes_read: usize,
    events_scored: usize,
}

#[derive(Default)]
struct SemanticVectorSearch {
    hits: Vec<SemanticVectorHit>,
    stats: SemanticVectorSearchStats,
}

struct SemanticHitSearch {
    hits: Vec<ctx_history_search::SemanticEventHit>,
    diagnostics: SemanticRetrievalDiagnostics,
}

#[derive(Debug, Clone)]
struct SemanticChunkDocument {
    event_id: Uuid,
    history_record_id: Option<Uuid>,
    session_id: Option<Uuid>,
    seq: u64,
    chunk_index: usize,
    chunk_count: usize,
    source_text_hash: String,
    chunk_text_hash: String,
    text: String,
    start_char: usize,
    end_char: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct SemanticSidecarStats {
    embedded_items: usize,
    embedded_chunks: usize,
}

#[derive(Debug, Default)]
struct SemanticIndexOutcome {
    indexed_chunks: usize,
    consumed_event_ids: Vec<Uuid>,
}

#[derive(Debug, Default)]
struct SemanticPruneOutcome {
    deleted_chunks: usize,
    queued_stale_events: usize,
}

struct SemanticVectorStore {
    conn: Connection,
}
