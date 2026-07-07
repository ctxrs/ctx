use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    thread,
    time::{Duration as StdDuration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(ctx_sqlite_vec)]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Once,
};

use anyhow::{anyhow, Context, Result};
#[cfg(ctx_semantic_fastembed)]
use fastembed::{
    EmbeddingModel, InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};
use rusqlite::{
    params, params_from_iter, types::Value as SqlValue, Connection, OpenFlags, OptionalExtension,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use ctx_history_core::{database_path, utc_now};
use ctx_history_store::{EventEmbeddingDocument, Store};

use crate::commands::{
    import::{error_summary, import_totals_json, ImportTotals},
    search::{refresh_sources_for_search, search_refresh_sources, RefreshArg},
};
use crate::config::{self, AppConfig, CONFIG_FILE};
use crate::output::{compact_json, print_json};
use crate::store_util::{open_existing_store_read_only, open_existing_store_snapshot_read_only};
use crate::{
    DaemonArgs, DaemonCommand, DaemonRunArgs, DaemonStartModeArg, DaemonTriggerCommandArg,
    JsonArgs, SearchBackendArg,
};

const SEMANTIC_BACKEND: &str = "fastembed";
const SEMANTIC_MODEL_KEY: &str = "fastembed:all-MiniLM-L6-v2:semantic-payload-chunk-1200-200-v2";
const SEMANTIC_MODEL_ID: &str = "sentence-transformers/all-MiniLM-L6-v2";
const SEMANTIC_HF_MODEL_CACHE_DIR: &str = "models--Qdrant--all-MiniLM-L6-v2-onnx";
const SEMANTIC_REQUIRED_MODEL_FILES: &[&str] = &[
    "model.onnx",
    "tokenizer.json",
    "config.json",
    "special_tokens_map.json",
    "tokenizer_config.json",
];
const SEMANTIC_DIMENSIONS: usize = 384;
const SEMANTIC_SEARCH_CANDIDATES: usize = 200;
const SEMANTIC_FILTERED_SEARCH_CANDIDATES: usize = 1_000;
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
const SEMANTIC_EMBED_THREADS_DEFAULT: usize = 2;
#[cfg(ctx_semantic_fastembed)]
const SEMANTIC_EMBED_THREADS_MAX: usize = 8;
#[cfg(ctx_semantic_fastembed)]
const SEMANTIC_EMBED_BATCH_DEFAULT: usize = 16;
#[cfg(ctx_semantic_fastembed)]
const SEMANTIC_EMBED_BATCH_MAX: usize = 512;
const SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT: usize = 512;
const SEMANTIC_WORKER_LOCK_FILE: &str = "semantic-worker.lock";
const SEMANTIC_WORKER_STATUS_FILE: &str = "semantic-worker.json";
const SEMANTIC_WORKER_BATCH_DEFAULT: usize = 128;
pub(crate) const SEMANTIC_WORKER_BATCH_MAX: usize = 5_000;
const SEMANTIC_WORKER_MAX_SECONDS_DEFAULT: u64 = 60;
pub(crate) const SEMANTIC_WORKER_MAX_SECONDS_CAP: u64 = 3_600;
const SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS: u64 = 15;
const SEMANTIC_VECTOR_BUSY_TIMEOUT_MS: u64 = 30_000;
const SEMANTIC_PRUNE_EVENT_BATCH: usize = 1_000;
const SEMANTIC_DEADLINE_CHUNKS_PER_SECOND: usize = 3;
const SEMANTIC_DEADLINE_MIN_CHUNK_BATCH: usize = 16;
const DAEMON_DIR: &str = "daemon";
const DAEMON_JOBS_DIR: &str = "jobs";
const DAEMON_LOCK_FILE: &str = "daemon.lock";
const DAEMON_STATUS_FILE: &str = "status.json";
const DAEMON_HISTORY_REFRESH_JOB_FILE: &str = "history-refresh.json";
const DAEMON_SEMANTIC_JOB_FILE: &str = "semantic-index.json";
const DAEMON_CLOUD_SYNC_JOB_FILE: &str = "cloud-sync.json";
const DAEMON_MAX_RUNTIME_SECONDS_DEFAULT: u64 = 300;
const DAEMON_IDLE_EXIT_SECONDS_DEFAULT: u64 = 30;
const DAEMON_LOOP_INTERVAL_SECONDS_DEFAULT: u64 = 5;
pub(crate) const DAEMON_RUNTIME_SECONDS_CAP: u64 = 24 * 60 * 60;
const DAEMON_AUTOSTART_MAX_RUNTIME_SECONDS_DEFAULT: u64 = 45;
const DAEMON_AUTOSTART_IDLE_EXIT_SECONDS_DEFAULT: u64 = 5;
const DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS_DEFAULT: u64 = 5;
const DAEMON_BACKGROUND_CHILD_ENV: &str = "CTX_DAEMON_BACKGROUND_CHILD";
const DAEMON_AUTOSTART_OFF_ENV: &str = "CTX_DAEMON_AUTOSTART_OFF";
const DAEMON_LOCK_STALE_AFTER_MS: i64 = 25 * 60 * 60 * 1_000;
const DAEMON_SEMANTIC_RESERVE_GRACE_SECS: u64 = 10;
const DAEMON_MIN_REMAINING_FOR_JOB_SECS: u64 = 2;
const SEMANTIC_HYBRID_MIN_EMBEDDED_ITEMS: usize = 1_000;
const SEMANTIC_HYBRID_MIN_COVERAGE_RATIO: f64 = 0.01;

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
    embedded_items: usize,
    embedded_chunks: usize,
    dirty_items: usize,
    queued_items_estimate: usize,
    model_cache_available: bool,
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
            embedded_items: 0,
            embedded_chunks: 0,
            dirty_items: 0,
            queued_items_estimate: 0,
            model_cache_available: semantic_model_cache_available(&semantic_worker_cache_dir(
                data_root,
            )),
            vector_path: semantic_vector_path(data_root),
            lock_path: semantic_worker_lock_path(data_root),
            status_path: semantic_worker_status_path(data_root),
        }
    }

    fn coverage_ratio(&self) -> Option<f64> {
        if self.searchable_items == 0 {
            None
        } else {
            Some((self.embedded_items as f64 / self.searchable_items as f64).min(1.0))
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        compact_json(json!({
            "status": self.status,
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
                "embedded_items": self.embedded_items,
                "embedded_chunks": self.embedded_chunks,
                "dirty_items": self.dirty_items,
                "queued_items_estimate": self.queued_items_estimate,
                "coverage_ratio": self.coverage_ratio(),
            },
            "model_cache_available": self.model_cache_available,
            "vector_path": self.vector_path.display().to_string(),
            "lock_path": self.lock_path.display().to_string(),
            "status_path": self.status_path.display().to_string(),
        }))
    }
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
    if worker.searchable_items == 0 || worker.embedded_items == 0 {
        "unavailable"
    } else if semantic_worker_coverage_ready(worker) {
        "ready"
    } else {
        "partial"
    }
}

fn semantic_worker_coverage_ready(worker: &SemanticWorkerReport) -> bool {
    worker.searchable_items > 0
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
    auto_candidate_count: Option<usize>,
    auto_embedded_candidate_count: Option<usize>,
    auto_hybrid_skipped: Option<&'static str>,
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
            "auto_candidate_count": self.auto_candidate_count,
            "auto_embedded_candidate_count": self.auto_embedded_candidate_count,
            "auto_hybrid_skipped": self.auto_hybrid_skipped,
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
    semantic_weight: f32,
    refresh_mode: RefreshArg,
    emit_warnings: bool,
) -> Result<(ctx_history_search::SearchPacket, SemanticRetrievalReport)> {
    let uses_composed_terms = terms.iter().any(|term| !term.trim().is_empty());
    let semantic_text = semantic_query_text(query, terms);
    let semantic_cache_dir = semantic_worker_cache_dir(data_root);
    let vector_path = semantic_vector_path(data_root);
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
    if filters_require_semantic_fallback || terms_require_semantic_fallback {
        effective_backend = SearchBackendArg::Lexical;
    }

    let worker_report = if requested_backend == SearchBackendArg::Auto
        || matches!(
            effective_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        ) {
        semantic_worker_report(data_root, Some(store))?
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

    let lexical_search_packet = || -> Result<ctx_history_search::SearchPacket> {
        if uses_composed_terms {
            ctx_history_search::search_packet_terms(store, query, terms, options)
                .map_err(Into::into)
        } else {
            ctx_history_search::search_packet(store, query, options).map_err(Into::into)
        }
    };

    let packet = if requested_backend == SearchBackendArg::Auto {
        auto_search_packet(
            store,
            options,
            &lexical_search_packet,
            &mut retrieval,
            &worker_report,
            &vector_path,
            &semantic_cache_dir,
            &semantic_text,
            semantic_weight,
            refresh_mode,
        )?
    } else if matches!(
        effective_backend,
        SearchBackendArg::Semantic | SearchBackendArg::Hybrid
    ) {
        semantic_or_hybrid_search_packet(
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
fn auto_search_packet(
    store: &Store,
    options: &ctx_history_search::PacketOptions,
    lexical_search_packet: &dyn Fn() -> Result<ctx_history_search::SearchPacket>,
    retrieval: &mut SemanticRetrievalReport,
    worker_report: &SemanticWorkerReport,
    vector_path: &Path,
    semantic_cache_dir: &Path,
    semantic_text: &str,
    semantic_weight: f32,
    _refresh_mode: RefreshArg,
) -> Result<ctx_history_search::SearchPacket> {
    let lexical_packet = lexical_search_packet()?;
    let mut auto_diagnostics = SemanticRetrievalDiagnostics::default();
    let auto_skip_reason = if semantic_text.trim().is_empty() {
        Some("empty_semantic_query")
    } else if semantic_filters_require_lexical_fallback(&options.filters) {
        Some("unsupported_filter_or_terms")
    } else {
        None
    };
    if let Some(reason) = auto_skip_reason {
        auto_diagnostics.auto_hybrid_skipped = Some(reason);
        retrieval.diagnostics = Some(auto_diagnostics);
        return Ok(lexical_packet);
    }

    let auto_candidate_ids = semantic_auto_candidate_event_ids_from_packet(&lexical_packet);
    auto_diagnostics.auto_candidate_count = Some(auto_candidate_ids.len());
    if auto_candidate_ids.len() == 1 {
        auto_diagnostics.auto_hybrid_skipped = Some("candidate_count_too_small");
        retrieval.diagnostics = Some(auto_diagnostics);
        return Ok(lexical_packet);
    }

    let vector_store = match SemanticVectorStore::open_read_only(vector_path) {
        Ok(Some(vector_store)) => vector_store,
        Ok(None) => {
            auto_diagnostics.auto_hybrid_skipped = Some("semantic_index_unavailable");
            retrieval.diagnostics = Some(auto_diagnostics);
            return Ok(lexical_packet);
        }
        Err(_) => {
            auto_diagnostics.auto_hybrid_skipped = Some("semantic_index_open_error");
            retrieval.semantic_status = "unavailable";
            retrieval.diagnostics = Some(auto_diagnostics);
            return Ok(lexical_packet);
        }
    };

    if auto_candidate_ids.is_empty() && !semantic_worker_coverage_ready(worker_report) {
        auto_diagnostics.auto_hybrid_skipped = Some("semantic_coverage_not_ready");
        retrieval.diagnostics = Some(auto_diagnostics);
        return Ok(lexical_packet);
    }

    if !worker_report.model_cache_available || !semantic_model_cache_available(semantic_cache_dir) {
        auto_diagnostics.auto_hybrid_skipped = Some("model_cache_missing");
        retrieval.diagnostics = Some(auto_diagnostics);
        return Ok(lexical_packet);
    }

    if auto_candidate_ids.is_empty() {
        *retrieval = SemanticRetrievalReport {
            requested_mode: SearchBackendArg::Auto,
            effective_mode: SearchBackendArg::Semantic,
            semantic_weight: 1.0,
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
        let semantic_candidate_limit =
            SEMANTIC_SEARCH_CANDIDATES.max(options.limit.saturating_mul(8));
        return match semantic_hits_for_text_query(
            store,
            &vector_store,
            semantic_cache_dir,
            semantic_text,
            semantic_candidate_limit,
            None,
        ) {
            Ok((semantic_hits, mut diagnostics)) if !semantic_hits.is_empty() => {
                diagnostics.auto_candidate_count = auto_diagnostics.auto_candidate_count;
                retrieval.diagnostics = Some(diagnostics);
                ctx_history_search::semantic_event_search_packet(
                    store,
                    semantic_text,
                    options,
                    &semantic_hits,
                    1.0,
                    false,
                )
                .map_err(Into::into)
            }
            Ok((_semantic_hits, mut diagnostics)) => {
                diagnostics.auto_candidate_count = auto_diagnostics.auto_candidate_count;
                diagnostics.auto_hybrid_skipped = Some("no_semantic_candidates");
                retrieval.effective_mode = SearchBackendArg::Lexical;
                retrieval.semantic_weight = 0.0;
                retrieval.embedding_model = None;
                retrieval.vector_path = None;
                retrieval.diagnostics = Some(diagnostics);
                Ok(lexical_packet)
            }
            Err(_) => {
                auto_diagnostics.auto_hybrid_skipped = Some("semantic_retrieval_failed");
                retrieval.effective_mode = SearchBackendArg::Lexical;
                retrieval.semantic_weight = 0.0;
                retrieval.embedding_model = None;
                retrieval.semantic_status = "unavailable";
                retrieval.diagnostics = Some(auto_diagnostics);
                Ok(lexical_packet)
            }
        };
    }

    let embedded_candidate_count = vector_store.embedded_event_id_count(&auto_candidate_ids)?;
    auto_diagnostics.auto_embedded_candidate_count = Some(embedded_candidate_count);
    if !semantic_auto_candidate_coverage_ready(embedded_candidate_count, auto_candidate_ids.len()) {
        auto_diagnostics.auto_hybrid_skipped = Some("candidate_coverage_not_ready");
        retrieval.diagnostics = Some(auto_diagnostics);
        return Ok(lexical_packet);
    }

    *retrieval = SemanticRetrievalReport {
        requested_mode: SearchBackendArg::Auto,
        effective_mode: SearchBackendArg::Hybrid,
        semantic_weight,
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
    match semantic_hits_for_text_query(
        store,
        &vector_store,
        semantic_cache_dir,
        semantic_text,
        auto_candidate_ids.len().max(options.limit),
        Some(&auto_candidate_ids),
    ) {
        Ok((semantic_hits, mut diagnostics)) if !semantic_hits.is_empty() => {
            diagnostics.auto_candidate_count = auto_diagnostics.auto_candidate_count;
            diagnostics.auto_embedded_candidate_count =
                auto_diagnostics.auto_embedded_candidate_count;
            retrieval.diagnostics = Some(diagnostics);
            Ok(semantic_auto_rerank_packet(
                lexical_packet,
                &semantic_hits,
                semantic_weight,
            ))
        }
        Ok((_semantic_hits, mut diagnostics)) => {
            diagnostics.auto_hybrid_skipped = Some("no_semantic_candidates");
            retrieval.effective_mode = SearchBackendArg::Lexical;
            retrieval.semantic_weight = 0.0;
            retrieval.embedding_model = None;
            retrieval.diagnostics = Some(diagnostics);
            Ok(lexical_packet)
        }
        Err(_) => {
            auto_diagnostics.auto_hybrid_skipped = Some("semantic_retrieval_failed");
            retrieval.effective_mode = SearchBackendArg::Lexical;
            retrieval.semantic_weight = 0.0;
            retrieval.embedding_model = None;
            retrieval.semantic_status = "unavailable";
            retrieval.diagnostics = Some(auto_diagnostics);
            Ok(lexical_packet)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn semantic_or_hybrid_search_packet(
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
                            "semantic index has no embedded event chunks and semantic model is not available in the local cache; strict semantic search will not initialize or download {SEMANTIC_MODEL_ID} during search"
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
                && !semantic_hybrid_coverage_ready(
                    worker_report.embedded_items,
                    worker_report.searchable_items,
                )
            {
                retrieval.effective_mode = SearchBackendArg::Lexical;
                retrieval.semantic_weight = 0.0;
                retrieval.embedding_model = None;
                retrieval.set_semantic_fallback(
                    "semantic_coverage_not_ready",
                    format!(
                        "semantic coverage is too low for hybrid ranking ({}/{} events embedded)",
                        worker_report.embedded_items, worker_report.searchable_items
                    ),
                );
                warn_if(
                    emit_warnings,
                    "warning: semantic coverage is too low for hybrid ranking; falling back to lexical search",
                );
                return lexical_search_packet();
            }

            if !worker_report.model_cache_available
                || !semantic_model_cache_available(semantic_cache_dir)
            {
                if effective_backend == SearchBackendArg::Semantic {
                    return Err(anyhow!(
                        "semantic model is not available in the local cache; strict semantic search will not initialize or download {SEMANTIC_MODEL_ID} during search"
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

            let semantic_candidate_limit = if semantic_filters_need_overfetch(&options.filters) {
                SEMANTIC_FILTERED_SEARCH_CANDIDATES.max(options.limit.saturating_mul(100))
            } else {
                SEMANTIC_SEARCH_CANDIDATES.max(options.limit.saturating_mul(8))
            };
            match semantic_hits_for_text_query(
                store,
                &vector_store,
                semantic_cache_dir,
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
                    if effective_backend == SearchBackendArg::Semantic {
                        return Err(anyhow!("semantic search failed: {error:#}"));
                    }
                    retrieval.effective_mode = SearchBackendArg::Lexical;
                    retrieval.semantic_weight = 0.0;
                    retrieval.embedding_model = None;
                    retrieval.semantic_status = "unavailable";
                    retrieval.diagnostics = None;
                    retrieval.set_semantic_fallback(
                        "semantic_retrieval_failed",
                        format!("semantic retrieval failed: {error:#}"),
                    );
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
                        "semantic index is not available yet and semantic model is not available in the local cache; strict semantic search will not initialize or download {SEMANTIC_MODEL_ID} during search"
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
                    *mut *mut i8,
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

impl SemanticVectorStore {
    fn open(path: &Path) -> Result<Self> {
        let _ = register_sqlite_vec_auto_extension();
        if let Some(parent) = path.parent() {
            create_private_dir_all(parent)?;
        }
        if !path.exists() {
            drop(
                private_create_new_file(path)
                    .with_context(|| format!("create semantic vector store {}", path.display()))?,
            );
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open semantic vector store {}", path.display()))?;
        conn.busy_timeout(StdDuration::from_millis(SEMANTIC_VECTOR_BUSY_TIMEOUT_MS))?;
        conn.execute_batch("PRAGMA secure_delete = ON;")?;
        let mut store = Self { conn };
        store.ensure_schema()?;
        secure_semantic_vector_permissions(path)?;
        Ok(store)
    }

    fn open_read_only(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let _ = register_sqlite_vec_auto_extension();
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("open semantic vector store read-only {}", path.display()))?;
        conn.busy_timeout(StdDuration::from_millis(SEMANTIC_VECTOR_BUSY_TIMEOUT_MS))?;
        let store = Self { conn };
        store.ensure_readable_schema()?;
        Ok(Some(store))
    }

    fn ensure_readable_schema(&self) -> Result<()> {
        let user_version = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap_or(0);
        if user_version > 5 {
            return Err(anyhow!(
                "semantic vector store schema version {user_version} is newer than this ctx supports"
            ));
        }
        if !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Err(anyhow!(
                "semantic vector store is missing event_embedding_chunks"
            ));
        }
        if !sqlite_table_has_columns(
            &self.conn,
            "event_embedding_chunks",
            &[
                "event_id",
                "model_key",
                "source_text_sha256",
                "start_char",
                "end_char",
                "dimensions",
                "embedding_f32",
            ],
        )? {
            return Err(anyhow!(
                "semantic vector store event_embedding_chunks schema is incomplete"
            ));
        }
        Ok(())
    }

    fn ensure_schema(&mut self) -> Result<()> {
        let user_version = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap_or(0);
        if user_version > 5 {
            return Err(anyhow!(
                "semantic vector store schema version {user_version} is newer than this ctx supports"
            ));
        }
        let mut compact_after_schema = false;
        if sqlite_table_exists(&self.conn, "event_embedding_chunks")?
            && !sqlite_table_has_columns(
                &self.conn,
                "event_embedding_chunks",
                &[
                    "event_id",
                    "model_key",
                    "history_record_id",
                    "session_id",
                    "event_seq",
                    "chunk_index",
                    "chunk_count",
                    "source_text_sha256",
                    "chunk_text_sha256",
                    "chunk_text",
                    "start_char",
                    "end_char",
                    "dimensions",
                    "embedding_f32",
                    "embedded_at_ms",
                ],
            )?
        {
            self.conn.execute("DROP TABLE event_embedding_chunks", [])?;
            compact_after_schema = true;
        }
        self.conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS embedding_models (
                model_key TEXT PRIMARY KEY,
                backend TEXT NOT NULL,
                model_id TEXT NOT NULL,
                dimensions INTEGER NOT NULL,
                distance TEXT NOT NULL,
                normalized INTEGER NOT NULL,
                created_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS event_embeddings (
                event_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                history_record_id TEXT,
                session_id TEXT,
                event_seq INTEGER NOT NULL,
                text_sha256 TEXT NOT NULL,
                preview_text TEXT NOT NULL DEFAULT '',
                dimensions INTEGER NOT NULL,
                embedding_f32 BLOB NOT NULL,
                embedded_at_ms INTEGER NOT NULL,
                PRIMARY KEY (event_id, model_key)
            );
            CREATE INDEX IF NOT EXISTS idx_event_embeddings_model_seq
                ON event_embeddings(model_key, event_seq);
            CREATE INDEX IF NOT EXISTS idx_event_embeddings_model_session
                ON event_embeddings(model_key, session_id);
            CREATE TABLE IF NOT EXISTS event_embedding_chunks (
                event_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                history_record_id TEXT,
                session_id TEXT,
                event_seq INTEGER NOT NULL,
                chunk_index INTEGER NOT NULL,
                chunk_count INTEGER NOT NULL,
                source_text_sha256 TEXT NOT NULL,
                chunk_text_sha256 TEXT NOT NULL,
                chunk_text TEXT NOT NULL DEFAULT '',
                start_char INTEGER NOT NULL,
                end_char INTEGER NOT NULL,
                dimensions INTEGER NOT NULL,
                embedding_f32 BLOB NOT NULL,
                embedded_at_ms INTEGER NOT NULL,
                PRIMARY KEY (event_id, model_key, chunk_index)
            );
            CREATE INDEX IF NOT EXISTS idx_event_embedding_chunks_model_seq
                ON event_embedding_chunks(model_key, event_seq);
            CREATE INDEX IF NOT EXISTS idx_event_embedding_chunks_model_session
                ON event_embedding_chunks(model_key, session_id);
            CREATE INDEX IF NOT EXISTS idx_event_embedding_chunks_model_event
                ON event_embedding_chunks(model_key, event_id);
            CREATE TABLE IF NOT EXISTS semantic_index_stats (
                model_key TEXT PRIMARY KEY,
                embedded_items INTEGER NOT NULL,
                embedded_chunks INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS semantic_dirty_events (
                event_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                queued_at_ms INTEGER NOT NULL,
                priority_seq INTEGER,
                reason TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (event_id, model_key)
            );
            CREATE INDEX IF NOT EXISTS idx_semantic_dirty_events_model_priority
                ON semantic_dirty_events(model_key, priority_seq, queued_at_ms);
            PRAGMA user_version = 5;
            "#,
        )?;
        if !sqlite_column_exists(&self.conn, "event_embeddings", "preview_text")? {
            self.conn.execute(
                "ALTER TABLE event_embeddings ADD COLUMN preview_text TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        let deleted_legacy_embeddings = self.conn.execute("DELETE FROM event_embeddings", [])?;
        let scrubbed_chunk_text = self.conn.execute(
            "UPDATE event_embedding_chunks SET chunk_text = '' WHERE chunk_text != ''",
            [],
        )?;
        self.conn.execute(
            r#"
            INSERT OR IGNORE INTO embedding_models
                (model_key, backend, model_id, dimensions, distance, normalized, created_at_ms)
            VALUES (?1, ?2, ?3, ?4, 'cosine', 1, ?5)
            "#,
            params![
                SEMANTIC_MODEL_KEY,
                SEMANTIC_BACKEND,
                SEMANTIC_MODEL_ID,
                SEMANTIC_DIMENSIONS as i64,
                utc_now().timestamp_millis()
            ],
        )?;
        if compact_after_schema || deleted_legacy_embeddings > 0 || scrubbed_chunk_text > 0 {
            self.compact_after_plaintext_scrub()?;
        }
        self.ensure_sqlite_vec0_schema()?;
        Ok(())
    }

    fn compact_after_plaintext_scrub(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            PRAGMA wal_checkpoint(TRUNCATE);
            VACUUM;
            "#,
        )?;
        Ok(())
    }

    fn sqlite_vec0_runtime_available(&self) -> bool {
        if !register_sqlite_vec_auto_extension() {
            return false;
        }
        self.conn
            .query_row("SELECT vec_version()", [], |row| row.get::<_, String>(0))
            .is_ok()
    }

    fn ensure_sqlite_vec0_schema(&mut self) -> Result<()> {
        if !self.sqlite_vec0_runtime_available() {
            return Ok(());
        }
        if !self.sqlite_vec0_schema_compatible()? {
            self.drop_sqlite_vec0_schema()?;
        }
        self.create_sqlite_vec0_schema()?;
        self.sync_sqlite_vec0_from_chunks_if_needed()
    }

    fn sqlite_vec0_schema_compatible(&self) -> Result<bool> {
        let meta_exists = sqlite_table_exists(&self.conn, "event_embedding_vec0_meta")?;
        let vec_exists = sqlite_table_exists(&self.conn, "event_embedding_vec0")?;
        if !meta_exists && !vec_exists {
            return Ok(true);
        }
        if meta_exists != vec_exists {
            return Ok(false);
        }
        if !sqlite_table_has_columns(
            &self.conn,
            "event_embedding_vec0_meta",
            &[
                "rowid",
                "event_id",
                "model_key",
                "history_record_id",
                "session_id",
                "event_seq",
                "chunk_index",
                "source_text_sha256",
                "start_char",
                "end_char",
            ],
        )? {
            return Ok(false);
        }
        let Some(sql) = sqlite_table_sql(&self.conn, "event_embedding_vec0")? else {
            return Ok(false);
        };
        let sql = sql.to_ascii_lowercase();
        Ok(sql.contains("using vec0")
            && sql.contains(&format!("embedding float[{SEMANTIC_DIMENSIONS}]")))
    }

    fn create_sqlite_vec0_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS event_embedding_vec0_meta (
                rowid INTEGER PRIMARY KEY,
                event_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                history_record_id TEXT,
                session_id TEXT,
                event_seq INTEGER NOT NULL,
                chunk_index INTEGER NOT NULL,
                source_text_sha256 TEXT NOT NULL,
                start_char INTEGER NOT NULL,
                end_char INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_event_embedding_vec0_meta_model_event
                ON event_embedding_vec0_meta(model_key, event_id);
            CREATE INDEX IF NOT EXISTS idx_event_embedding_vec0_meta_model_seq
                ON event_embedding_vec0_meta(model_key, event_seq);
            "#,
        )?;
        self.conn.execute_batch(&format!(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS event_embedding_vec0
            USING vec0(embedding float[{SEMANTIC_DIMENSIONS}] distance_metric=cosine);
            "#
        ))?;
        Ok(())
    }

    fn sqlite_vec0_mismatch_count(&self) -> Result<usize> {
        if !self.sqlite_vec0_runtime_available()
            || !sqlite_table_exists(&self.conn, "event_embedding_vec0")?
            || !sqlite_table_exists(&self.conn, "event_embedding_vec0_meta")?
        {
            return Ok(0);
        }
        let missing_or_stale_meta = self
            .conn
            .query_row(
                r#"
	                SELECT COUNT(*)
	                FROM event_embedding_chunks AS c
	                LEFT JOIN event_embedding_vec0_meta AS m
	                  ON m.rowid = c.rowid
	                 AND m.model_key = c.model_key
	                WHERE c.model_key = ?1
	                  AND c.dimensions = ?2
	                  AND (
	                        m.rowid IS NULL
	                     OR m.event_id != c.event_id
	                     OR COALESCE(m.history_record_id, '') != COALESCE(c.history_record_id, '')
	                     OR COALESCE(m.session_id, '') != COALESCE(c.session_id, '')
	                     OR m.event_seq != c.event_seq
	                     OR m.chunk_index != c.chunk_index
	                     OR m.source_text_sha256 != c.source_text_sha256
	                     OR m.start_char != c.start_char
	                     OR m.end_char != c.end_char
	                  )
	                "#,
                params![SEMANTIC_MODEL_KEY, SEMANTIC_DIMENSIONS as i64],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as usize;
        let orphan_meta = self
            .conn
            .query_row(
                r#"
	                SELECT COUNT(*)
	                FROM event_embedding_vec0_meta AS m
	                LEFT JOIN event_embedding_chunks AS c
	                  ON c.rowid = m.rowid
	                 AND c.model_key = m.model_key
	                 AND c.dimensions = ?2
	                WHERE m.model_key = ?1
	                  AND c.rowid IS NULL
	                "#,
                params![SEMANTIC_MODEL_KEY, SEMANTIC_DIMENSIONS as i64],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as usize;
        let missing_or_stale_vector = self
            .conn
            .query_row(
                r#"
	                SELECT COUNT(*)
	                FROM event_embedding_chunks AS c
	                LEFT JOIN event_embedding_vec0 AS v
	                  ON v.rowid = c.rowid
	                WHERE c.model_key = ?1
	                  AND c.dimensions = ?2
	                  AND (
	                        v.rowid IS NULL
	                     OR v.embedding != c.embedding_f32
	                  )
	                "#,
                params![SEMANTIC_MODEL_KEY, SEMANTIC_DIMENSIONS as i64],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as usize;
        Ok(missing_or_stale_meta
            .saturating_add(orphan_meta)
            .saturating_add(missing_or_stale_vector))
    }

    fn drop_sqlite_vec0_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            DROP TABLE IF EXISTS event_embedding_vec0;
            DROP TABLE IF EXISTS event_embedding_vec0_meta;
            "#,
        )?;
        Ok(())
    }

    fn sqlite_vec0_counts(&self) -> Result<Option<(usize, usize, usize)>> {
        if !self.sqlite_vec0_runtime_available()
            || !sqlite_table_exists(&self.conn, "event_embedding_vec0")?
            || !sqlite_table_exists(&self.conn, "event_embedding_vec0_meta")?
        {
            return Ok(None);
        }
        let canonical_chunks = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_embedding_chunks WHERE model_key = ?1 AND dimensions = ?2",
                params![SEMANTIC_MODEL_KEY, SEMANTIC_DIMENSIONS as i64],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as usize;
        let meta_rows = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_embedding_vec0_meta WHERE model_key = ?1",
                params![SEMANTIC_MODEL_KEY],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as usize;
        let vec_rows = self
            .conn
            .query_row("SELECT COUNT(*) FROM event_embedding_vec0", [], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?
            .unwrap_or(0)
            .max(0) as usize;
        Ok(Some((canonical_chunks, meta_rows, vec_rows)))
    }

    fn sqlite_vec0_ready(&self) -> Result<bool> {
        let Some((canonical_chunks, meta_rows, vec_rows)) = self.sqlite_vec0_counts()? else {
            return Ok(false);
        };
        if canonical_chunks == 0 || meta_rows != canonical_chunks || vec_rows != canonical_chunks {
            return Ok(false);
        }
        Ok(self.sqlite_vec0_mismatch_count()? == 0)
    }

    fn sync_sqlite_vec0_from_chunks_if_needed(&mut self) -> Result<()> {
        let Some((canonical_chunks, meta_rows, vec_rows)) = self.sqlite_vec0_counts()? else {
            return Ok(());
        };
        if meta_rows == canonical_chunks
            && vec_rows == canonical_chunks
            && self.sqlite_vec0_mismatch_count()? == 0
        {
            return Ok(());
        }
        self.rebuild_sqlite_vec0_from_chunks()
    }

    fn rebuild_sqlite_vec0_from_chunks(&mut self) -> Result<()> {
        if !self.sqlite_vec0_runtime_available() {
            return Ok(());
        }
        self.drop_sqlite_vec0_schema()?;
        self.create_sqlite_vec0_schema()?;
        let tx = self.conn.transaction()?;
        {
            let mut rows = tx.prepare(
                r#"
	                SELECT rowid, event_id, history_record_id, session_id, event_seq, chunk_index,
	                       source_text_sha256, start_char, end_char, embedding_f32
                FROM event_embedding_chunks
                WHERE model_key = ?1
                  AND dimensions = ?2
                ORDER BY event_seq DESC, chunk_index ASC
                "#,
            )?;
            let mut meta_stmt = tx.prepare(
                r#"
                INSERT INTO event_embedding_vec0_meta
	                    (rowid, event_id, model_key, history_record_id, session_id, event_seq,
	                     chunk_index, source_text_sha256, start_char, end_char)
	                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
            )?;
            let mut vec_stmt =
                tx.prepare("INSERT INTO event_embedding_vec0(rowid, embedding) VALUES (?1, ?2)")?;
            let mut rows = rows.query(params![SEMANTIC_MODEL_KEY, SEMANTIC_DIMENSIONS as i64])?;
            while let Some(row) = rows.next()? {
                let rowid: i64 = row.get(0)?;
                let event_id: String = row.get(1)?;
                let history_record_id: Option<String> = row.get(2)?;
                let session_id: Option<String> = row.get(3)?;
                let event_seq: i64 = row.get(4)?;
                let chunk_index: i64 = row.get(5)?;
                let source_text_sha256: String = row.get(6)?;
                let start_char: i64 = row.get(7)?;
                let end_char: i64 = row.get(8)?;
                let embedding: Vec<u8> = row.get(9)?;
                meta_stmt.execute(params![
                    rowid,
                    event_id,
                    SEMANTIC_MODEL_KEY,
                    history_record_id,
                    session_id,
                    event_seq,
                    chunk_index,
                    source_text_sha256,
                    start_char,
                    end_char,
                ])?;
                vec_stmt.execute(params![rowid, embedding])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn cached_stats(&self) -> Result<Option<SemanticSidecarStats>> {
        if !sqlite_table_exists(&self.conn, "semantic_index_stats")? {
            return Ok(None);
        }
        let stats = self
            .conn
            .query_row(
                r#"
                SELECT embedded_items, embedded_chunks
                FROM semantic_index_stats
                WHERE model_key = ?1
                "#,
                params![SEMANTIC_MODEL_KEY],
                |row| {
                    let embedded_items = row.get::<_, i64>(0)?.max(0) as usize;
                    let embedded_chunks = row.get::<_, i64>(1)?.max(0) as usize;
                    Ok(SemanticSidecarStats {
                        embedded_items,
                        embedded_chunks,
                    })
                },
            )
            .optional()?;
        Ok(stats)
    }

    fn exact_stats(&self) -> Result<SemanticSidecarStats> {
        if !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(SemanticSidecarStats::default());
        }
        let embedded_chunks = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_embedding_chunks WHERE model_key = ?1",
                params![SEMANTIC_MODEL_KEY],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        let embedded_items = self
            .conn
            .query_row(
                "SELECT COUNT(DISTINCT event_id) FROM event_embedding_chunks WHERE model_key = ?1",
                params![SEMANTIC_MODEL_KEY],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(SemanticSidecarStats {
            embedded_items: embedded_items.max(0) as usize,
            embedded_chunks: embedded_chunks.max(0) as usize,
        })
    }

    fn cached_or_exact_stats(&self) -> Result<SemanticSidecarStats> {
        if let Some(stats) = self.cached_stats()? {
            return Ok(stats);
        }
        self.exact_stats()
    }

    fn refresh_cached_stats(&self) -> Result<SemanticSidecarStats> {
        let stats = self.exact_stats()?;
        self.conn.execute(
            r#"
            INSERT INTO semantic_index_stats
                (model_key, embedded_items, embedded_chunks, updated_at_ms)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(model_key) DO UPDATE SET
                embedded_items = excluded.embedded_items,
                embedded_chunks = excluded.embedded_chunks,
                updated_at_ms = excluded.updated_at_ms
            "#,
            params![
                SEMANTIC_MODEL_KEY,
                stats.embedded_items as i64,
                stats.embedded_chunks as i64,
                utc_now().timestamp_millis()
            ],
        )?;
        Ok(stats)
    }

    fn dirty_event_count(&self) -> Result<usize> {
        if !sqlite_table_exists(&self.conn, "semantic_dirty_events")? {
            return Ok(0);
        }
        let count = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM semantic_dirty_events WHERE model_key = ?1",
                params![SEMANTIC_MODEL_KEY],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(count.max(0) as usize)
    }

    fn enqueue_dirty_documents(
        &mut self,
        docs: &[EventEmbeddingDocument],
        reason: &str,
    ) -> Result<usize> {
        if docs.is_empty() {
            return Ok(0);
        }
        let reason = reason.chars().take(64).collect::<String>();
        let queued_at_ms = utc_now().timestamp_millis();
        let tx = self.conn.transaction()?;
        let mut changed = 0_usize;
        {
            let mut stmt = tx.prepare(
                r#"
                INSERT INTO semantic_dirty_events
                    (event_id, model_key, queued_at_ms, priority_seq, reason, attempts)
                VALUES (?1, ?2, ?3, ?4, ?5, 0)
                ON CONFLICT(event_id, model_key) DO UPDATE SET
                    queued_at_ms = excluded.queued_at_ms,
                    priority_seq = COALESCE(excluded.priority_seq, semantic_dirty_events.priority_seq),
                    reason = excluded.reason
                "#,
            )?;
            for doc in docs {
                changed = changed.saturating_add(stmt.execute(params![
                    doc.event_id.to_string(),
                    SEMANTIC_MODEL_KEY,
                    queued_at_ms,
                    doc.seq as i64,
                    reason
                ])?);
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    fn queued_dirty_event_ids(&self, limit: usize) -> Result<Vec<Uuid>> {
        if limit == 0 || !sqlite_table_exists(&self.conn, "semantic_dirty_events")? {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            r#"
            SELECT event_id
            FROM semantic_dirty_events
            WHERE model_key = ?1
            ORDER BY priority_seq IS NULL, priority_seq DESC, queued_at_ms ASC
            LIMIT ?2
            "#,
        )?;
        let mut rows = stmt.query(params![SEMANTIC_MODEL_KEY, limit as i64])?;
        let mut event_ids = Vec::new();
        while let Some(row) = rows.next()? {
            let event_id_text = row.get::<_, String>(0)?;
            let event_id = Uuid::parse_str(&event_id_text)
                .context("invalid dirty event id in semantic vector store")?;
            event_ids.push(event_id);
        }
        Ok(event_ids)
    }

    fn dequeue_dirty_events(&mut self, event_ids: &[Uuid]) -> Result<usize> {
        if event_ids.is_empty() || !sqlite_table_exists(&self.conn, "semantic_dirty_events")? {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut deleted = 0_usize;
        {
            let mut stmt = tx.prepare(
                "DELETE FROM semantic_dirty_events WHERE model_key = ?1 AND event_id = ?2",
            )?;
            for event_id in event_ids {
                deleted = deleted.saturating_add(
                    stmt.execute(params![SEMANTIC_MODEL_KEY, event_id.to_string()])?,
                );
            }
        }
        tx.commit()?;
        Ok(deleted)
    }

    fn plaintext_value_count(&self) -> Result<usize> {
        let mut count = 0_usize;
        if sqlite_column_exists(&self.conn, "event_embeddings", "preview_text")? {
            let rows = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM event_embeddings WHERE preview_text != ''",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .unwrap_or(0);
            count = count.saturating_add(rows.max(0) as usize);
        }
        if sqlite_column_exists(&self.conn, "event_embedding_chunks", "chunk_text")? {
            let rows = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM event_embedding_chunks WHERE chunk_text != ''",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .unwrap_or(0);
            count = count.saturating_add(rows.max(0) as usize);
        }
        Ok(count)
    }

    fn existing_hashes_for_event_ids(&self, event_ids: &[Uuid]) -> Result<HashMap<Uuid, String>> {
        if event_ids.is_empty() || !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(HashMap::new());
        }
        let placeholders = (0..event_ids.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            r#"
            SELECT event_id, source_text_sha256
            FROM event_embedding_chunks
            WHERE model_key = ?
              AND event_id IN ({placeholders})
            GROUP BY event_id, source_text_sha256
            "#
        );
        let mut query_params = vec![SqlValue::from(SEMANTIC_MODEL_KEY.to_owned())];
        query_params.extend(
            event_ids
                .iter()
                .map(|event_id| SqlValue::from(event_id.to_string())),
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(query_params))?;
        let mut hashes = HashMap::new();
        while let Some(row) = rows.next()? {
            let event_id = Uuid::parse_str(&row.get::<_, String>(0)?)
                .context("invalid event id in semantic vector store")?;
            hashes.insert(event_id, row.get(1)?);
        }
        Ok(hashes)
    }

    fn upsert_chunk_embeddings(
        &mut self,
        items: &[(SemanticChunkDocument, Vec<f32>)],
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let maintain_sqlite_vec0 = self.sqlite_vec0_runtime_available()
            && sqlite_table_exists(&self.conn, "event_embedding_vec0")?
            && sqlite_table_exists(&self.conn, "event_embedding_vec0_meta")?;
        let tx = self.conn.transaction()?;
        {
            if maintain_sqlite_vec0 {
                let mut delete_vec_stmt = tx.prepare(
                    r#"
                    DELETE FROM event_embedding_vec0
                    WHERE rowid IN (
                        SELECT rowid
                        FROM event_embedding_vec0_meta
                        WHERE model_key = ?1 AND event_id = ?2
                    )
                    "#,
                )?;
                let mut delete_meta_stmt = tx.prepare(
                    "DELETE FROM event_embedding_vec0_meta WHERE model_key = ?1 AND event_id = ?2",
                )?;
                let mut deleted_events = std::collections::HashSet::new();
                for (doc, _) in items {
                    if deleted_events.insert(doc.event_id) {
                        let event_id = doc.event_id.to_string();
                        delete_vec_stmt.execute(params![SEMANTIC_MODEL_KEY, &event_id])?;
                        delete_meta_stmt.execute(params![SEMANTIC_MODEL_KEY, &event_id])?;
                    }
                }
            }
            let mut delete_stmt = tx.prepare(
                "DELETE FROM event_embedding_chunks WHERE event_id = ?1 AND model_key = ?2",
            )?;
            let mut deleted_events = std::collections::HashSet::new();
            for (doc, _) in items {
                if deleted_events.insert(doc.event_id) {
                    delete_stmt.execute(params![doc.event_id.to_string(), SEMANTIC_MODEL_KEY])?;
                }
            }
            drop(delete_stmt);

            let mut stmt = tx.prepare(
                r#"
                INSERT INTO event_embedding_chunks
                    (event_id, model_key, history_record_id, session_id, event_seq,
                     chunk_index, chunk_count, source_text_sha256, chunk_text_sha256,
                     chunk_text, start_char, end_char, dimensions, embedding_f32, embedded_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                "#,
            )?;
            let mut vec0_meta_stmt = if maintain_sqlite_vec0 {
                Some(tx.prepare(
                    r#"
	                    INSERT INTO event_embedding_vec0_meta
	                        (rowid, event_id, model_key, history_record_id, session_id, event_seq,
	                         chunk_index, source_text_sha256, start_char, end_char)
	                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
	                    "#,
                )?)
            } else {
                None
            };
            let mut vec0_stmt = if maintain_sqlite_vec0 {
                Some(tx.prepare(
                    "INSERT INTO event_embedding_vec0(rowid, embedding) VALUES (?1, ?2)",
                )?)
            } else {
                None
            };
            let embedded_at_ms = utc_now().timestamp_millis();
            for (doc, embedding) in items {
                let event_id = doc.event_id.to_string();
                let history_record_id = doc.history_record_id.map(|id| id.to_string());
                let session_id = doc.session_id.map(|id| id.to_string());
                let blob = serialize_f32_blob(embedding);
                stmt.execute(params![
                    &event_id,
                    SEMANTIC_MODEL_KEY,
                    &history_record_id,
                    &session_id,
                    doc.seq as i64,
                    doc.chunk_index as i64,
                    doc.chunk_count as i64,
                    doc.source_text_hash,
                    doc.chunk_text_hash,
                    "",
                    doc.start_char as i64,
                    doc.end_char as i64,
                    SEMANTIC_DIMENSIONS as i64,
                    &blob,
                    embedded_at_ms
                ])?;
                let rowid = tx.last_insert_rowid();
                if let (Some(meta_stmt), Some(vec_stmt)) =
                    (vec0_meta_stmt.as_mut(), vec0_stmt.as_mut())
                {
                    meta_stmt.execute(params![
                        rowid,
                        &event_id,
                        SEMANTIC_MODEL_KEY,
                        &history_record_id,
                        &session_id,
                        doc.seq as i64,
                        doc.chunk_index as i64,
                        &doc.source_text_hash,
                        doc.start_char as i64,
                        doc.end_char as i64,
                    ])?;
                    vec_stmt.execute(params![rowid, &blob])?;
                }
            }
        }
        tx.commit()?;
        self.refresh_cached_stats()?;
        Ok(())
    }

    fn prune_ineligible_events(&mut self, store: &Store) -> Result<SemanticPruneOutcome> {
        if !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(SemanticPruneOutcome::default());
        }
        let mut stmt = self.conn.prepare(
            r#"
            SELECT event_id, MIN(source_text_sha256), COUNT(DISTINCT source_text_sha256)
            FROM event_embedding_chunks
            WHERE model_key = ?1
            GROUP BY event_id
            ORDER BY MAX(event_seq) DESC
            "#,
        )?;
        let mut rows = stmt.query(params![SEMANTIC_MODEL_KEY])?;
        let mut sidecar_events = Vec::<(Uuid, String, bool)>::new();
        while let Some(row) = rows.next()? {
            let event_id_text = row.get::<_, String>(0)?;
            if let Ok(event_id) = Uuid::parse_str(&event_id_text) {
                let source_text_hash = row.get::<_, String>(1)?;
                let hash_versions = row.get::<_, i64>(2)?.max(0);
                sidecar_events.push((event_id, source_text_hash, hash_versions == 1));
            }
        }
        drop(rows);
        drop(stmt);

        let mut outcome = SemanticPruneOutcome::default();
        for chunk in sidecar_events.chunks(SEMANTIC_PRUNE_EVENT_BATCH) {
            let event_ids = chunk
                .iter()
                .map(|(event_id, _, _)| *event_id)
                .collect::<Vec<_>>();
            let eligible_event_ids = store.semantic_eligible_event_ids(&event_ids)?;
            let current_docs = store.event_embedding_documents_by_ids(&event_ids)?;
            let current_by_id = current_docs
                .into_iter()
                .map(|doc| (doc.event_id, doc))
                .collect::<HashMap<_, _>>();
            let mut delete_event_ids = Vec::new();
            let mut stale_docs = Vec::new();
            for (event_id, stored_hash, single_hash) in chunk {
                let Some(doc) = current_by_id.get(event_id) else {
                    delete_event_ids.push(*event_id);
                    continue;
                };
                if !eligible_event_ids.contains(event_id) {
                    delete_event_ids.push(*event_id);
                    continue;
                }
                let source_text = semantic_source_text(&doc.text);
                let current_hash = semantic_document_hash(doc, &source_text);
                if !*single_hash || current_hash != *stored_hash {
                    delete_event_ids.push(*event_id);
                    stale_docs.push(doc.clone());
                }
            }
            outcome.deleted_chunks = outcome
                .deleted_chunks
                .saturating_add(self.delete_embedding_chunks_for_event_ids(&delete_event_ids)?);
            if !stale_docs.is_empty() {
                outcome.queued_stale_events = outcome
                    .queued_stale_events
                    .saturating_add(self.enqueue_dirty_documents(&stale_docs, "stale_hash")?);
            }
        }

        let scrubbed_chunk_text = self.conn.execute(
            "UPDATE event_embedding_chunks SET chunk_text = '' WHERE model_key = ?1 AND chunk_text != ''",
            params![SEMANTIC_MODEL_KEY],
        )?;
        self.refresh_cached_stats()?;
        if scrubbed_chunk_text > 0 {
            self.compact_after_plaintext_scrub()?;
        }
        Ok(outcome)
    }

    fn delete_embedding_chunks_for_event_ids(&mut self, event_ids: &[Uuid]) -> Result<usize> {
        if event_ids.is_empty() || !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(0);
        }
        let maintain_sqlite_vec0 = self.sqlite_vec0_runtime_available()
            && sqlite_table_exists(&self.conn, "event_embedding_vec0")?
            && sqlite_table_exists(&self.conn, "event_embedding_vec0_meta")?;
        let tx = self.conn.transaction()?;
        let mut deleted = 0_usize;
        {
            if maintain_sqlite_vec0 {
                let mut delete_vec_stmt = tx.prepare(
                    r#"
                    DELETE FROM event_embedding_vec0
                    WHERE rowid IN (
                        SELECT rowid
                        FROM event_embedding_vec0_meta
                        WHERE model_key = ?1 AND event_id = ?2
                    )
                    "#,
                )?;
                let mut delete_meta_stmt = tx.prepare(
                    "DELETE FROM event_embedding_vec0_meta WHERE model_key = ?1 AND event_id = ?2",
                )?;
                for event_id in event_ids {
                    let event_id = event_id.to_string();
                    delete_vec_stmt.execute(params![SEMANTIC_MODEL_KEY, &event_id])?;
                    delete_meta_stmt.execute(params![SEMANTIC_MODEL_KEY, &event_id])?;
                }
            }
            let mut stmt = tx.prepare(
                "DELETE FROM event_embedding_chunks WHERE model_key = ?1 AND event_id = ?2",
            )?;
            for event_id in event_ids {
                deleted = deleted.saturating_add(
                    stmt.execute(params![SEMANTIC_MODEL_KEY, event_id.to_string()])?,
                );
            }
        }
        tx.commit()?;
        Ok(deleted)
    }

    fn search(&self, query_embedding: &[f32], limit: usize) -> Result<SemanticVectorSearch> {
        self.search_with_event_filter(query_embedding, limit, None)
    }

    fn search_event_ids(
        &self,
        query_embedding: &[f32],
        event_ids: &[Uuid],
        limit: usize,
    ) -> Result<SemanticVectorSearch> {
        if event_ids.is_empty() {
            return Ok(SemanticVectorSearch::default());
        }
        self.search_with_event_filter(query_embedding, limit, Some(event_ids))
    }

    fn embedded_event_id_count(&self, event_ids: &[Uuid]) -> Result<usize> {
        if event_ids.is_empty() || !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(0);
        }
        let placeholders = (0..event_ids.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            r#"
            SELECT COUNT(DISTINCT event_id)
            FROM event_embedding_chunks
            WHERE model_key = ?
              AND dimensions = ?
              AND event_id IN ({placeholders})
            "#
        );
        let mut query_params = vec![
            SqlValue::from(SEMANTIC_MODEL_KEY.to_owned()),
            SqlValue::from(SEMANTIC_DIMENSIONS as i64),
        ];
        query_params.extend(
            event_ids
                .iter()
                .map(|event_id| SqlValue::from(event_id.to_string())),
        );
        let count = self
            .conn
            .query_row(&sql, params_from_iter(query_params), |row| {
                row.get::<_, i64>(0)
            })
            .optional()?
            .unwrap_or(0);
        Ok(count.max(0) as usize)
    }

    fn search_with_event_filter(
        &self,
        query_embedding: &[f32],
        limit: usize,
        event_ids: Option<&[Uuid]>,
    ) -> Result<SemanticVectorSearch> {
        if event_ids.is_none() && self.sqlite_vec0_ready()? {
            if let Ok(search) = self.search_sqlite_vec0(query_embedding, limit) {
                return Ok(search);
            }
        }

        let scan_started = Instant::now();
        if !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(SemanticVectorSearch {
                hits: Vec::new(),
                stats: SemanticVectorSearchStats {
                    backend: Some(SEMANTIC_VECTOR_BACKEND_RUST),
                    scan_ms: scan_started.elapsed().as_millis() as u64,
                    ..SemanticVectorSearchStats::default()
                },
            });
        }
        let mut sql = r#"
            SELECT event_id, source_text_sha256, start_char, end_char, embedding_f32
            FROM event_embedding_chunks
            WHERE model_key = ?1
              AND dimensions = ?2
            "#
        .to_owned();
        let mut query_params = vec![
            SqlValue::from(SEMANTIC_MODEL_KEY.to_owned()),
            SqlValue::from(SEMANTIC_DIMENSIONS as i64),
        ];
        if let Some(event_ids) = event_ids {
            let placeholders = (0..event_ids.len())
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(" AND event_id IN (");
            sql.push_str(&placeholders);
            sql.push(')');
            query_params.extend(
                event_ids
                    .iter()
                    .map(|event_id| SqlValue::from(event_id.to_string())),
            );
        } else {
            sql.push_str(" ORDER BY event_seq DESC LIMIT ?");
            query_params.push(SqlValue::from(SEMANTIC_FULL_SCAN_MAX_CHUNKS as i64));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(query_params))?;
        let mut best_by_event = HashMap::<Uuid, SemanticVectorHit>::new();
        let limit = limit.max(1);
        let mut chunks_scanned = 0_usize;
        let mut vector_bytes_read = 0_usize;
        while let Some(row) = rows.next()? {
            let event_id = Uuid::parse_str(&row.get::<_, String>(0)?)
                .context("invalid event id in semantic vector store")?;
            let source_text_hash = row.get::<_, String>(1)?;
            let start_char = row.get::<_, i64>(2)?.max(0) as usize;
            let end_char = row.get::<_, i64>(3)?.max(0) as usize;
            let blob: Vec<u8> = row.get(4)?;
            chunks_scanned = chunks_scanned.saturating_add(1);
            vector_bytes_read = vector_bytes_read.saturating_add(blob.len());
            if event_ids.is_none() && vector_bytes_read > SEMANTIC_FULL_SCAN_MAX_VECTOR_BYTES {
                break;
            }
            let Some(similarity) = dot_product_f32_blob(query_embedding, &blob)? else {
                continue;
            };
            match best_by_event.get_mut(&event_id) {
                Some(existing) if similarity > existing.similarity => {
                    *existing = SemanticVectorHit {
                        event_id,
                        similarity,
                        source_text_hash,
                        start_char,
                        end_char,
                    };
                }
                None => {
                    best_by_event.insert(
                        event_id,
                        SemanticVectorHit {
                            event_id,
                            similarity,
                            source_text_hash,
                            start_char,
                            end_char,
                        },
                    );
                }
                _ => {}
            }
        }
        let events_scored = best_by_event.len();
        let mut top = best_by_event.into_values().collect::<Vec<_>>();
        if top.len() > limit {
            top.select_nth_unstable_by(limit - 1, compare_semantic_hits_desc);
            top.truncate(limit);
        }
        top.sort_by(compare_semantic_hits_desc);
        Ok(SemanticVectorSearch {
            hits: top,
            stats: SemanticVectorSearchStats {
                backend: Some(SEMANTIC_VECTOR_BACKEND_RUST),
                scan_ms: scan_started.elapsed().as_millis() as u64,
                chunks_scanned,
                vector_bytes_read,
                events_scored,
            },
        })
    }

    fn search_sqlite_vec0(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<SemanticVectorSearch> {
        let scan_started = Instant::now();
        let query_blob = serialize_f32_blob(query_embedding);
        let stats = self.cached_or_exact_stats()?;
        let limit = limit.clamp(1, SEMANTIC_SQLITE_VEC0_MAX_K);
        let max_k = stats
            .embedded_chunks
            .max(limit)
            .clamp(1, SEMANTIC_SQLITE_VEC0_MAX_K);
        let mut k = limit.min(max_k);
        let mut best_by_event = HashMap::<Uuid, SemanticVectorHit>::new();
        let mut rows_returned: usize;
        loop {
            best_by_event.clear();
            rows_returned = 0;
            let mut stmt = self.conn.prepare(
                r#"
                SELECT m.event_id, m.source_text_sha256, m.start_char, m.end_char, v.distance
                FROM event_embedding_vec0 AS v
                JOIN event_embedding_vec0_meta AS m ON m.rowid = v.rowid
	                WHERE v.embedding MATCH ?1
	                  AND v.k = ?2
	                  AND m.model_key = ?3
	                ORDER BY v.distance
	                "#,
            )?;
            let mut rows = stmt.query(params![&query_blob, k as i64, SEMANTIC_MODEL_KEY])?;
            while let Some(row) = rows.next()? {
                rows_returned = rows_returned.saturating_add(1);
                let event_id = Uuid::parse_str(&row.get::<_, String>(0)?)
                    .context("invalid event id in semantic vec0 store")?;
                let source_text_hash = row.get::<_, String>(1)?;
                let start_char = row.get::<_, i64>(2)?.max(0) as usize;
                let end_char = row.get::<_, i64>(3)?.max(0) as usize;
                let distance = row.get::<_, f64>(4)? as f32;
                let similarity = (1.0 - distance).clamp(-1.0, 1.0);
                match best_by_event.get_mut(&event_id) {
                    Some(existing) if similarity > existing.similarity => {
                        *existing = SemanticVectorHit {
                            event_id,
                            similarity,
                            source_text_hash,
                            start_char,
                            end_char,
                        };
                    }
                    None => {
                        best_by_event.insert(
                            event_id,
                            SemanticVectorHit {
                                event_id,
                                similarity,
                                source_text_hash,
                                start_char,
                                end_char,
                            },
                        );
                    }
                    _ => {}
                }
            }
            if best_by_event.len() >= limit || rows_returned < k || k >= max_k {
                break;
            }
            k = k.saturating_mul(2).min(max_k);
        }
        if best_by_event.len() < limit
            && rows_returned >= k
            && k >= max_k
            && max_k < stats.embedded_chunks
        {
            return Err(anyhow!(
                "sqlite vec0 top-k cap reached before enough unique semantic events"
            ));
        }
        let mut hits = best_by_event.into_values().collect::<Vec<_>>();
        if hits.len() > limit {
            hits.select_nth_unstable_by(limit - 1, compare_semantic_hits_desc);
            hits.truncate(limit);
        }
        hits.sort_by(compare_semantic_hits_desc);
        Ok(SemanticVectorSearch {
            hits,
            stats: SemanticVectorSearchStats {
                backend: Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC),
                scan_ms: scan_started.elapsed().as_millis() as u64,
                chunks_scanned: stats.embedded_chunks,
                vector_bytes_read: stats
                    .embedded_chunks
                    .saturating_mul(SEMANTIC_DIMENSIONS)
                    .saturating_mul(std::mem::size_of::<f32>()),
                events_scored: stats.embedded_items,
            },
        })
    }
}

fn semantic_vector_path(data_root: &Path) -> PathBuf {
    data_root.join("vectors.sqlite")
}

fn semantic_worker_lock_path(data_root: &Path) -> PathBuf {
    data_root.join(SEMANTIC_WORKER_LOCK_FILE)
}

fn semantic_worker_status_path(data_root: &Path) -> PathBuf {
    data_root.join(SEMANTIC_WORKER_STATUS_FILE)
}

fn daemon_root_path(data_root: &Path) -> PathBuf {
    data_root.join(DAEMON_DIR)
}

fn daemon_jobs_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_JOBS_DIR)
}

fn daemon_lock_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_LOCK_FILE)
}

fn daemon_status_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_STATUS_FILE)
}

fn daemon_history_refresh_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(DAEMON_HISTORY_REFRESH_JOB_FILE)
}

fn daemon_semantic_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(DAEMON_SEMANTIC_JOB_FILE)
}

fn daemon_cloud_sync_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(DAEMON_CLOUD_SYNC_JOB_FILE)
}

struct DaemonLock {
    path: PathBuf,
}

impl DaemonLock {
    fn acquire(data_root: &Path) -> Result<Option<Self>> {
        create_private_dir_all(data_root)?;
        let root = daemon_root_path(data_root);
        create_private_dir_all(&root)?;
        let path = daemon_lock_path(data_root);
        for attempt in 0..2 {
            match private_create_new_file(&path) {
                Ok(mut file) => {
                    let payload = json!({
                        "pid": process::id(),
                        "started_at_ms": utc_now().timestamp_millis(),
                        "binary": env::current_exe().ok(),
                        "data_root": data_root,
                    });
                    writeln!(file, "{}", serde_json::to_string(&payload)?)?;
                    return Ok(Some(Self { path }));
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if attempt == 0 && daemon_lock_is_stale(&path) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    return Ok(None);
                }
                Err(err) => {
                    return Err(err)
                        .with_context(|| format!("create ctx daemon lock {}", path.display()));
                }
            }
        }
        Ok(None)
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct SemanticWorkerLock {
    path: PathBuf,
}

impl SemanticWorkerLock {
    fn acquire(data_root: &Path) -> Result<Option<Self>> {
        create_private_dir_all(data_root)?;
        let path = semantic_worker_lock_path(data_root);
        for attempt in 0..2 {
            match private_create_new_file(&path) {
                Ok(mut file) => {
                    let payload = json!({
                        "pid": process::id(),
                        "started_at_ms": utc_now().timestamp_millis(),
                    });
                    writeln!(file, "{}", serde_json::to_string(&payload)?)?;
                    return Ok(Some(Self { path }));
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if attempt == 0 && semantic_worker_lock_is_stale(&path) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    return Ok(None);
                }
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!("create semantic worker lock {}", path.display())
                    });
                }
            }
        }
        Ok(None)
    }
}

impl Drop for SemanticWorkerLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn semantic_worker_lock_is_stale(path: &Path) -> bool {
    pid_lock_file_is_stale(path)
}

fn daemon_lock_is_stale(path: &Path) -> bool {
    pid_lock_file_is_stale(path)
}

fn pid_lock_file_is_stale(path: &Path) -> bool {
    let Some(value) = read_pid_lock_json(path) else {
        return path.exists();
    };
    if lock_started_at_is_stale(&value) {
        return true;
    }
    let Some(pid) = pid_from_lock_json(&value) else {
        return true;
    };
    !pid_is_running(pid)
}

fn read_pid_lock_file(path: &Path) -> Option<u32> {
    read_pid_lock_json(path).and_then(|value| pid_from_lock_json(&value))
}

fn read_pid_lock_json(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn pid_from_lock_json(value: &Value) -> Option<u32> {
    value
        .get("pid")
        .and_then(|value| value.as_u64())
        .and_then(|pid| u32::try_from(pid).ok())
}

fn lock_started_at_is_stale(value: &Value) -> bool {
    let Some(started_at_ms) = json_i64(value, "started_at_ms") else {
        return false;
    };
    utc_now().timestamp_millis().saturating_sub(started_at_ms) > DAEMON_LOCK_STALE_AFTER_MS
}

#[cfg(unix)]
fn pid_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn pid_is_running(pid: u32) -> bool {
    pid != 0
}

#[cfg(unix)]
fn lower_semantic_worker_priority() {
    unsafe {
        let _ = libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

#[cfg(not(unix))]
fn lower_semantic_worker_priority() {}

fn write_private_json_file(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent)?;
    }
    let tmp_path = path.with_extension(format!("json.{}.tmp", process::id()));
    if tmp_path.exists() {
        let _ = fs::remove_file(&tmp_path);
    }
    let mut file = private_create_new_file(&tmp_path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("write private status file {}", tmp_path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("write private status file {}", tmp_path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync private status file {}", tmp_path.display()))?;
    drop(file);
    fs::rename(&tmp_path, path)
        .with_context(|| format!("replace private status file {}", path.display()))?;
    secure_private_file_permissions(path)?;
    Ok(())
}

fn write_semantic_worker_status(data_root: &Path, value: &Value) -> Result<()> {
    write_private_json_file(&semantic_worker_status_path(data_root), value)
}

fn read_semantic_worker_status(data_root: &Path) -> Option<Value> {
    let text = fs::read_to_string(semantic_worker_status_path(data_root)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_daemon_status(data_root: &Path, value: &Value) -> Result<()> {
    write_private_json_file(&daemon_status_path(data_root), value)
}

fn read_daemon_status(data_root: &Path) -> Option<Value> {
    let text = fs::read_to_string(daemon_status_path(data_root)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_daemon_job_status(path: &Path, value: &Value) -> Result<()> {
    write_private_json_file(path, value)
}

fn read_daemon_job_status(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn semantic_status_file_stats(status_value: Option<&Value>) -> SemanticSidecarStats {
    SemanticSidecarStats {
        embedded_items: status_value
            .and_then(|value| json_usize(value, "embedded_items"))
            .unwrap_or(0),
        embedded_chunks: status_value
            .and_then(|value| json_usize(value, "embedded_chunks"))
            .unwrap_or(0),
    }
}

pub(crate) fn semantic_worker_report(
    data_root: &Path,
    store: Option<&Store>,
) -> Result<SemanticWorkerReport> {
    let status_value = read_semantic_worker_status(data_root);
    let searchable_items = match store {
        Some(store) => store.event_embedding_document_count_cached_or_exact()?,
        None => status_value
            .as_ref()
            .and_then(|value| json_usize(value, "searchable_items"))
            .unwrap_or(0),
    };
    let vector_path = semantic_vector_path(data_root);
    let model_cache_available =
        semantic_model_cache_available(&semantic_worker_cache_dir(data_root));
    let sidecar_state_result = (|| -> Result<(SemanticSidecarStats, usize)> {
        if let Some(vector_store) = SemanticVectorStore::open_read_only(&vector_path)? {
            let dirty_items = vector_store.dirty_event_count()?;
            let mut stats = vector_store.cached_or_exact_stats()?;
            if semantic_status_needs_exact_sidecar_stats(searchable_items, dirty_items, stats) {
                stats = vector_store.exact_stats()?;
            }
            Ok((stats, dirty_items))
        } else if store.is_some() {
            Ok((SemanticSidecarStats::default(), 0))
        } else {
            Ok((semantic_status_file_stats(status_value.as_ref()), 0))
        }
    })();
    let (sidecar_stats, dirty_items, sidecar_error) = match sidecar_state_result {
        Ok((stats, dirty_items)) => (stats, dirty_items, None),
        Err(error) => (
            SemanticSidecarStats {
                embedded_items: 0,
                embedded_chunks: 0,
            },
            0,
            Some(format!("{error:#}")),
        ),
    };
    let embedded_items = sidecar_stats.embedded_items;
    let embedded_chunks = sidecar_stats.embedded_chunks;
    let status_path = semantic_worker_status_path(data_root);
    let lock_path = semantic_worker_lock_path(data_root);
    let lock_pid = read_pid_lock_file(&lock_path);
    let running = lock_pid.is_some_and(pid_is_running);
    let pid = if running {
        lock_pid
    } else {
        status_value
            .as_ref()
            .and_then(|value| json_u32(value, "pid"))
    };
    let queued_items_estimate = searchable_items
        .saturating_sub(embedded_items)
        .max(dirty_items);
    let mut status = status_value
        .as_ref()
        .and_then(|value| json_string(value, "status"))
        .unwrap_or_else(|| {
            if store.is_none() {
                "unknown".to_owned()
            } else if searchable_items == 0 {
                "empty".to_owned()
            } else if queued_items_estimate == 0 {
                "ready".to_owned()
            } else {
                "pending".to_owned()
            }
        });
    if store.is_some() {
        let live_status = if searchable_items == 0 {
            "empty".to_owned()
        } else if sidecar_error.is_some() {
            "unavailable".to_owned()
        } else if queued_items_estimate == 0 {
            "ready".to_owned()
        } else {
            "pending".to_owned()
        };
        let preserve_status = (status == "budget_exhausted" && queued_items_estimate > 0)
            || (status == "failed"
                && sidecar_error.is_none()
                && embedded_items == 0
                && queued_items_estimate > 0);
        status = if preserve_status { status } else { live_status };
    }
    if running {
        status = "running".to_owned();
    } else if lock_path.exists() && semantic_worker_lock_is_stale(&lock_path) {
        status = "stale_lock".to_owned();
    }
    Ok(SemanticWorkerReport {
        status,
        running,
        pid,
        started_at_ms: status_value
            .as_ref()
            .and_then(|value| json_i64(value, "started_at_ms")),
        heartbeat_at_ms: status_value
            .as_ref()
            .and_then(|value| json_i64(value, "heartbeat_at_ms")),
        finished_at_ms: status_value
            .as_ref()
            .and_then(|value| json_i64(value, "finished_at_ms")),
        indexed_chunks: status_value
            .as_ref()
            .and_then(|value| json_usize(value, "indexed_chunks")),
        model_init_ms: status_value
            .as_ref()
            .and_then(|value| json_usize(value, "model_init_ms")),
        last_error: sidecar_error.or_else(|| {
            status_value
                .as_ref()
                .and_then(|value| json_string(value, "last_error"))
        }),
        searchable_items,
        embedded_items,
        embedded_chunks,
        dirty_items,
        queued_items_estimate,
        model_cache_available,
        vector_path,
        lock_path,
        status_path,
    })
}

pub(crate) fn semantic_worker_report_best_effort(data_root: &Path) -> SemanticWorkerReport {
    semantic_worker_report(data_root, None)
        .unwrap_or_else(|error| SemanticWorkerReport::unavailable(data_root, format!("{error:#}")))
}

pub(crate) fn daemon_report(data_root: &Path, semantic_report: &SemanticWorkerReport) -> Value {
    daemon_report_with_disabled_status(data_root, semantic_report, true)
}

fn daemon_report_with_disabled_status(
    data_root: &Path,
    semantic_report: &SemanticWorkerReport,
    disabled_overrides_lifecycle: bool,
) -> Value {
    let enabled = daemon_enabled_for_status(data_root);
    let status_value = read_daemon_status(data_root);
    let lock_path = daemon_lock_path(data_root);
    let status_path = daemon_status_path(data_root);
    let lock_pid = read_pid_lock_file(&lock_path);
    let running = lock_pid.is_some_and(pid_is_running);
    let mut status = status_value
        .as_ref()
        .and_then(|value| json_string(value, "status"))
        .unwrap_or_else(|| "unknown".to_owned());
    if running {
        status = "running".to_owned();
    } else if lock_path.exists() && daemon_lock_is_stale(&lock_path) {
        status = "stale_lock".to_owned();
    } else if !enabled && (disabled_overrides_lifecycle || status == "unknown") {
        status = "disabled".to_owned();
    }
    let pid = if running {
        lock_pid
    } else {
        status_value
            .as_ref()
            .and_then(|value| json_u32(value, "pid"))
    };
    compact_json(json!({
        "status": status,
        "enabled": enabled,
        "running": running,
        "pid": pid,
        "started_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "started_at_ms")),
        "heartbeat_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "heartbeat_at_ms")),
        "finished_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "finished_at_ms")),
        "start_mode": status_value
            .as_ref()
            .and_then(|value| json_string(value, "start_mode")),
        "trigger_command": status_value
            .as_ref()
            .and_then(|value| json_string(value, "trigger_command")),
        "last_error": status_value.as_ref().and_then(|value| json_string(value, "last_error")),
        "lock_path": lock_path,
        "status_path": status_path,
        "jobs": {
            "history_refresh": daemon_history_refresh_job_report(
                data_root,
                disabled_overrides_lifecycle
            ),
            "semantic_index": daemon_semantic_job_report(
                data_root,
                semantic_report,
                disabled_overrides_lifecycle
            ),
            "cloud_sync": daemon_cloud_sync_job_report(data_root),
        },
    }))
}

fn daemon_history_refresh_job_report(
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
) -> Value {
    let daemon_enabled = daemon_enabled_for_status(data_root);
    let status_value = read_daemon_job_status(&daemon_history_refresh_job_path(data_root));
    let disabled = !daemon_enabled && disabled_overrides_lifecycle;
    let current_status = if disabled {
        "disabled".to_owned()
    } else {
        status_value
            .as_ref()
            .and_then(|value| json_string(value, "status"))
            .unwrap_or_else(|| "unknown".to_owned())
    };
    let reason = if disabled {
        Some("daemon_disabled".to_owned())
    } else {
        status_value
            .as_ref()
            .and_then(|value| json_string(value, "reason"))
    };
    compact_json(json!({
        "status": current_status,
        "enabled": daemon_enabled,
        "reason": reason,
        "mode": status_value
            .as_ref()
            .and_then(|value| json_string(value, "mode"))
            .unwrap_or_else(|| RefreshArg::Auto.as_str().to_owned()),
        "last_run_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "last_run_at_ms")),
        "source_count": status_value.as_ref().and_then(|value| value.get("source_count").cloned()),
        "source_fingerprint": status_value
            .as_ref()
            .and_then(|value| json_string(value, "source_fingerprint")),
        "passes": status_value.as_ref().and_then(|value| json_usize(value, "passes")),
        "totals": status_value.as_ref().and_then(|value| value.get("totals").cloned()),
        "budget_reasons": status_value
            .as_ref()
            .and_then(|value| value.get("budget_reasons").cloned()),
        "last_error": status_value
            .as_ref()
            .and_then(|value| json_string(value, "last_error")),
    }))
}

fn daemon_enabled_for_status(data_root: &Path) -> bool {
    AppConfig::load(data_root)
        .map(|config| config.daemon.enabled)
        .unwrap_or_else(|_| AppConfig::default().daemon.enabled)
}

fn daemon_semantic_job_report(
    data_root: &Path,
    semantic_report: &SemanticWorkerReport,
    disabled_overrides_lifecycle: bool,
) -> Value {
    let daemon_enabled = daemon_enabled_for_status(data_root);
    let status_value = read_daemon_job_status(&daemon_semantic_job_path(data_root));
    let disabled = !daemon_enabled && disabled_overrides_lifecycle && !semantic_report.running;
    let current_status = if disabled {
        "disabled"
    } else if semantic_report.running {
        "running"
    } else if semantic_report.status == "stale_lock" {
        "stale_lock"
    } else if semantic_report.status == "unavailable" {
        "unavailable"
    } else if semantic_report.searchable_items == 0 {
        "empty"
    } else if semantic_report.queued_items_estimate == 0 {
        "ready"
    } else if !semantic_report.model_cache_available {
        "skipped"
    } else if semantic_report.status == "failed" {
        "failed"
    } else {
        "pending"
    };
    let derived_reason = if disabled {
        Some("daemon_disabled".to_owned())
    } else if semantic_report.status == "stale_lock" {
        Some("worker_lock_stale".to_owned())
    } else if semantic_report.status == "unavailable" {
        Some("sidecar_unavailable".to_owned())
    } else if semantic_report.searchable_items == 0 {
        Some("no_searchable_items".to_owned())
    } else if semantic_report.queued_items_estimate > 0 && !semantic_report.model_cache_available {
        Some("model_cache_missing".to_owned())
    } else if semantic_report.status == "failed" {
        Some("worker_failed".to_owned())
    } else {
        None
    };
    compact_json(json!({
        "status": current_status,
        "enabled": daemon_enabled,
        "reason": derived_reason,
        "last_run_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "last_run_at_ms")),
        "last_run_status": status_value
            .as_ref()
            .and_then(|value| json_string(value, "status")),
        "last_run_reason": status_value
            .as_ref()
            .and_then(|value| json_string(value, "reason")),
        "last_error": status_value
            .as_ref()
            .and_then(|value| json_string(value, "last_error"))
            .or_else(|| semantic_report.last_error.clone()),
        "indexed_chunks": status_value.as_ref().and_then(|value| json_usize(value, "indexed_chunks")),
        "model_cache_available": semantic_report.model_cache_available,
        "worker_status": semantic_report.status,
        "coverage": {
            "searchable_items": semantic_report.searchable_items,
            "completed_items": semantic_report.embedded_items,
            "embedded_items": semantic_report.embedded_items,
            "embedded_chunks": semantic_report.embedded_chunks,
            "dirty_items": semantic_report.dirty_items,
            "queued_items_estimate": semantic_report.queued_items_estimate,
        },
    }))
}

fn daemon_cloud_sync_job_report(data_root: &Path) -> Value {
    let status_value = read_daemon_job_status(&daemon_cloud_sync_job_path(data_root));
    compact_json(json!({
        "status": "disabled",
        "enabled": false,
        "reason": "not_configured",
        "network_allowed": false,
        "last_run_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "last_run_at_ms")),
        "last_upload_at_ms": Value::Null,
        "queued_items_estimate": 0,
        "last_error": Value::Null,
    }))
}

#[derive(Debug)]
struct DaemonIteration {
    did_work: bool,
    failed: bool,
}

#[derive(Default)]
struct DaemonRuntime {
    semantic_embedder: Option<SemanticEmbedder>,
    recent_semantic_work_enqueued: bool,
}

#[derive(Debug, Clone)]
struct SemanticWorkerArgs {
    max_chunks: Option<usize>,
    max_seconds: Option<u64>,
}

pub(crate) fn run_daemon_command(
    args: DaemonArgs,
    data_root: PathBuf,
    config: &AppConfig,
) -> Result<()> {
    match args.command {
        DaemonCommand::Run(args) => run_daemon(args, data_root, config),
        DaemonCommand::Status(args) => run_daemon_status(args, data_root),
        DaemonCommand::Enable(args) => run_daemon_enabled_update(args, data_root, true),
        DaemonCommand::Disable(args) => run_daemon_enabled_update(args, data_root, false),
    }
}

fn run_daemon_status(args: JsonArgs, data_root: PathBuf) -> Result<()> {
    let semantic_report = semantic_worker_report_for_daemon(&data_root);
    let daemon = daemon_report(&data_root, &semantic_report);
    if args.json {
        print_json(json!({
            "schema_version": 1,
            "daemon": daemon,
            "local_only": true,
        }))?;
    } else {
        print_daemon_status_human(&daemon);
    }
    Ok(())
}

fn run_daemon_enabled_update(args: JsonArgs, data_root: PathBuf, enabled: bool) -> Result<()> {
    config::set_daemon_enabled(&data_root, enabled)?;
    if args.json {
        print_json(json!({
            "schema_version": 1,
            "daemon_enabled": enabled,
            "config_path": data_root.join(CONFIG_FILE),
            "local_only": true,
        }))?;
    } else {
        println!("daemon_enabled: {enabled}");
        println!("config_path: {}", data_root.join(CONFIG_FILE).display());
    }
    Ok(())
}

fn print_daemon_status_human(daemon: &Value) {
    println!(
        "daemon_enabled: {}",
        daemon
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    );
    println!(
        "daemon_status: {}",
        daemon
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "daemon_running: {}",
        daemon
            .get("running")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    );
    println!(
        "history_refresh_status: {}",
        daemon
            .get("jobs")
            .and_then(|jobs| jobs.get("history_refresh"))
            .and_then(|job| job.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "semantic_index_status: {}",
        daemon
            .get("jobs")
            .and_then(|jobs| jobs.get("semantic_index"))
            .and_then(|job| job.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "cloud_sync_status: {}",
        daemon
            .get("jobs")
            .and_then(|jobs| jobs.get("cloud_sync"))
            .and_then(|cloud| cloud.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("disabled")
    );
}

fn run_daemon(args: DaemonRunArgs, data_root: PathBuf, config: &AppConfig) -> Result<()> {
    lower_semantic_worker_priority();
    let report = match run_daemon_inner(args.clone(), &data_root, config.daemon.enabled) {
        Ok(report) => report,
        Err(error) => {
            let message = format!("{error:#}");
            let now = utc_now().timestamp_millis();
            let _ = write_daemon_status(
                &data_root,
                &json!({
                    "schema_version": 1,
                    "status": "failed",
                    "pid": process::id(),
                    "heartbeat_at_ms": now,
                    "finished_at_ms": now,
                    "start_mode": daemon_run_start_mode(&args).as_str(),
                    "trigger_command": args.trigger_command.map(DaemonTriggerCommandArg::as_str),
                    "last_error": message,
                }),
            );
            return Err(error);
        }
    };
    if args.json {
        print_json(report)?;
    } else {
        print_daemon_status_human(&report);
    }
    Ok(())
}

fn run_daemon_inner(args: DaemonRunArgs, data_root: &Path, daemon_enabled: bool) -> Result<Value> {
    if !daemon_enabled && !args.force {
        let semantic_report = semantic_worker_report_for_daemon(data_root);
        return Ok(daemon_report(data_root, &semantic_report));
    }
    let Some(_lock) = DaemonLock::acquire(data_root)? else {
        let semantic_report = semantic_worker_report_for_daemon(data_root);
        return Ok(daemon_report(data_root, &semantic_report));
    };

    let run_once = args.once;
    let max_runtime = StdDuration::from_secs(
        args.max_runtime_seconds
            .unwrap_or(DAEMON_MAX_RUNTIME_SECONDS_DEFAULT),
    );
    let idle_exit = StdDuration::from_secs(
        args.idle_exit_seconds
            .unwrap_or(DAEMON_IDLE_EXIT_SECONDS_DEFAULT),
    );
    let loop_interval = StdDuration::from_secs(
        args.loop_interval_seconds
            .unwrap_or(DAEMON_LOOP_INTERVAL_SECONDS_DEFAULT),
    );
    let started = Instant::now();
    let deadline = started + max_runtime;
    let started_at_ms = utc_now().timestamp_millis();
    let mut failed = false;
    write_daemon_lifecycle_status(data_root, &args, "running", started_at_ms, None, None)?;

    let mut runtime = DaemonRuntime::default();
    let mut idle_since: Option<Instant> = None;
    loop {
        if !daemon_deadline_has_min_budget(Some(deadline), DAEMON_MIN_REMAINING_FOR_JOB_SECS) {
            break;
        }
        let iteration = run_daemon_once(&args, data_root, &mut runtime, Some(deadline))?;
        write_daemon_lifecycle_status(data_root, &args, "running", started_at_ms, None, None)?;
        if iteration.failed {
            failed = true;
            break;
        }
        if run_once {
            break;
        }
        if iteration.did_work {
            idle_since = None;
        } else if idle_since.is_none() {
            idle_since = Some(Instant::now());
        }
        if idle_since.is_some_and(|idle| idle.elapsed() >= idle_exit) {
            break;
        }
        let sleep_for = daemon_deadline_remaining(Some(deadline))
            .map(|remaining| loop_interval.min(remaining))
            .unwrap_or(loop_interval);
        if sleep_for.is_zero() {
            break;
        }
        std::thread::sleep(sleep_for);
    }

    write_daemon_lifecycle_status(
        data_root,
        &args,
        if failed { "failed" } else { "completed" },
        started_at_ms,
        Some(utc_now().timestamp_millis()),
        failed.then_some("one or more daemon jobs failed".to_owned()),
    )?;
    drop(_lock);
    let semantic_report = semantic_worker_report_for_daemon(data_root);
    Ok(daemon_report_with_disabled_status(
        data_root,
        &semantic_report,
        !args.force,
    ))
}

fn run_daemon_once(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
) -> Result<DaemonIteration> {
    let history_refresh_job =
        if daemon_deadline_has_min_budget(deadline, DAEMON_MIN_REMAINING_FOR_JOB_SECS) {
            run_daemon_history_refresh_job(data_root)
        } else {
            Ok(daemon_history_refresh_skipped_job("daemon_deadline"))
        };
    let history_refresh_job = match history_refresh_job {
        Ok(value) => value,
        Err(error) => daemon_history_refresh_failed_job(format!("{error:#}")),
    };
    let history_refresh_did_work = daemon_history_refresh_job_did_work(&history_refresh_job);
    write_daemon_job_status_unless_deadline_skip(
        &daemon_history_refresh_job_path(data_root),
        &history_refresh_job,
    )?;

    let semantic_job = if daemon_run_is_autostart(args) {
        Ok(daemon_semantic_autostart_skipped_job(data_root))
    } else if daemon_deadline_has_min_budget(deadline, DAEMON_MIN_REMAINING_FOR_JOB_SECS) {
        run_daemon_semantic_job(args, data_root, runtime, deadline)
    } else {
        Ok(daemon_semantic_deadline_skipped_job(data_root))
    };
    let semantic_job = match semantic_job {
        Ok(value) => value,
        Err(error) => daemon_semantic_failed_job(data_root, format!("{error:#}")),
    };
    let semantic_did_work = semantic_job
        .get("indexed_chunks")
        .and_then(Value::as_u64)
        .is_some_and(|chunks| chunks > 0);
    write_daemon_job_status_unless_deadline_skip(
        &daemon_semantic_job_path(data_root),
        &semantic_job,
    )?;

    let cloud_sync_job = daemon_cloud_sync_disabled_job(Some(utc_now().timestamp_millis()));
    write_daemon_job_status(&daemon_cloud_sync_job_path(data_root), &cloud_sync_job)?;

    Ok(DaemonIteration {
        did_work: history_refresh_did_work || semantic_did_work,
        failed: daemon_job_failed(&history_refresh_job) || daemon_job_failed(&semantic_job),
    })
}

fn daemon_run_start_mode(args: &DaemonRunArgs) -> DaemonStartModeArg {
    args.start_mode.unwrap_or(DaemonStartModeArg::Manual)
}

fn daemon_run_is_autostart(args: &DaemonRunArgs) -> bool {
    matches!(args.start_mode, Some(DaemonStartModeArg::Auto)) || args.trigger_command.is_some()
}

fn daemon_job_failed(value: &Value) -> bool {
    value.get("status").and_then(Value::as_str) == Some("failed")
}

fn write_daemon_job_status_unless_deadline_skip(path: &Path, value: &Value) -> Result<()> {
    if daemon_job_skipped_for_deadline(value) && path.exists() {
        return Ok(());
    }
    write_daemon_job_status(path, value)
}

fn daemon_job_skipped_for_deadline(value: &Value) -> bool {
    value.get("status").and_then(Value::as_str) == Some("skipped")
        && value.get("reason").and_then(Value::as_str) == Some("daemon_deadline")
}

fn daemon_deadline_remaining(deadline: Option<Instant>) -> Option<StdDuration> {
    deadline.and_then(|deadline| deadline.checked_duration_since(Instant::now()))
}

fn daemon_deadline_has_min_budget(deadline: Option<Instant>, min_secs: u64) -> bool {
    let Some(remaining) = daemon_deadline_remaining(deadline) else {
        return deadline.is_none();
    };
    remaining >= StdDuration::from_secs(min_secs)
}

fn run_daemon_history_refresh_job(data_root: &Path) -> Result<Value> {
    let last_run_at_ms = utc_now().timestamp_millis();
    let sources = search_refresh_sources(None);
    let plugin_sources = Vec::new();
    let source_count = sources.len();
    if source_count == 0 {
        return Ok(daemon_history_refresh_job_json(
            "skipped",
            0,
            ImportTotals::default(),
            last_run_at_ms,
            Some("no_sources"),
            None,
        ));
    }
    let source_fingerprint = search_refresh_source_fingerprint(&sources);
    let mut job = match refresh_sources_for_search(
        data_root,
        sources,
        plugin_sources,
        RefreshArg::Auto,
        true,
    ) {
        Ok(totals) => daemon_history_refresh_job_json(
            "completed",
            source_count,
            totals,
            last_run_at_ms,
            None,
            None,
        ),
        Err(error) => daemon_history_refresh_job_json(
            "failed",
            source_count,
            ImportTotals::default(),
            last_run_at_ms,
            None,
            Some(error_summary(&error)),
        ),
    };
    if let Some(map) = job.as_object_mut() {
        map.insert("source_fingerprint".to_owned(), json!(source_fingerprint));
        map.insert("passes".to_owned(), json!(1));
    }
    Ok(job)
}

fn daemon_history_refresh_skipped_job(reason: &str) -> Value {
    daemon_history_refresh_job_json(
        "skipped",
        0,
        ImportTotals::default(),
        utc_now().timestamp_millis(),
        Some(reason),
        None,
    )
}

fn daemon_history_refresh_failed_job(message: String) -> Value {
    daemon_history_refresh_job_json(
        "failed",
        0,
        ImportTotals::default(),
        utc_now().timestamp_millis(),
        None,
        Some(message),
    )
}

fn daemon_history_refresh_job_json(
    status: &str,
    source_count: usize,
    totals: ImportTotals,
    last_run_at_ms: i64,
    reason: Option<&str>,
    last_error: Option<String>,
) -> Value {
    compact_json(json!({
        "mode": RefreshArg::Auto.as_str(),
        "status": status,
        "source_count": source_count,
        "totals": import_totals_json(&totals),
        "reason": reason,
        "last_run_at_ms": last_run_at_ms,
        "last_error": last_error,
    }))
}

fn daemon_history_refresh_job_did_work(value: &Value) -> bool {
    let Some(totals) = value.get("totals") else {
        return false;
    };
    ["imported_sessions", "imported_events", "imported_edges"]
        .into_iter()
        .any(|key| totals.get(key).and_then(Value::as_u64).unwrap_or(0) > 0)
}

fn search_refresh_source_fingerprint(sources: &[crate::provider_sources::SourceInfo]) -> String {
    let mut items = sources
        .iter()
        .map(|source| {
            format!(
                "{}|{}|{}",
                source.provider.as_str(),
                source.source_format,
                source.path.display()
            )
        })
        .collect::<Vec<_>>();
    items.sort();
    semantic_text_hash(&items.join("\n"))
}

fn run_daemon_semantic_job(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
) -> Result<Value> {
    let last_run_at_ms = utc_now().timestamp_millis();
    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        let report = semantic_worker_report_best_effort(data_root);
        return Ok(daemon_semantic_job_json(
            "skipped",
            Some("store_missing"),
            last_run_at_ms,
            &report,
            None,
            None,
        ));
    }

    let store = open_existing_store_read_only(&db_path, "ctx daemon semantic job")?;
    if !runtime.recent_semantic_work_enqueued {
        let _ = queue_recent_semantic_work(data_root, &store, "daemon_recent");
        runtime.recent_semantic_work_enqueued = true;
    }
    let before = semantic_worker_report(data_root, Some(&store))?;
    if before.searchable_items == 0 {
        return Ok(daemon_semantic_job_json(
            "empty",
            Some("no_searchable_items"),
            last_run_at_ms,
            &before,
            None,
            None,
        ));
    }
    if before.queued_items_estimate == 0 {
        return Ok(daemon_semantic_job_json(
            "ready",
            None,
            last_run_at_ms,
            &before,
            None,
            None,
        ));
    }
    if !before.model_cache_available && runtime.semantic_embedder.is_none() {
        return Ok(daemon_semantic_job_json(
            "skipped",
            Some("model_cache_missing"),
            last_run_at_ms,
            &before,
            None,
            None,
        ));
    }
    let min_remaining_secs = if runtime.semantic_embedder.is_some() {
        DAEMON_MIN_REMAINING_FOR_JOB_SECS
    } else {
        SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS
    }
    .saturating_add(DAEMON_SEMANTIC_RESERVE_GRACE_SECS);
    if !daemon_deadline_has_min_budget(deadline, min_remaining_secs) {
        return Ok(daemon_semantic_job_json(
            "skipped",
            Some("daemon_deadline"),
            last_run_at_ms,
            &before,
            None,
            None,
        ));
    }
    drop(store);

    let worker_max_seconds = daemon_semantic_worker_seconds_budget(args, deadline);
    if worker_max_seconds == 0 {
        let report = semantic_worker_report_for_daemon(data_root);
        return Ok(daemon_semantic_job_json(
            "skipped",
            Some("daemon_deadline"),
            last_run_at_ms,
            &report,
            None,
            None,
        ));
    }
    let worker_args = SemanticWorkerArgs {
        max_chunks: args.max_chunks,
        max_seconds: Some(worker_max_seconds),
    };
    if let Err(error) = run_semantic_worker_inner_with_embedder(
        worker_args,
        data_root,
        None,
        &mut runtime.semantic_embedder,
    ) {
        let message = format!("{error:#}");
        let _ = write_semantic_worker_failure_status(data_root, message.clone());
        let report = semantic_worker_report_for_daemon(data_root);
        return Ok(daemon_semantic_job_json(
            "failed",
            None,
            last_run_at_ms,
            &report,
            None,
            Some(message),
        ));
    }
    let report = semantic_worker_report_for_daemon(data_root);
    let indexed_chunks = report.indexed_chunks;
    let status = if report.running {
        "running"
    } else if report.queued_items_estimate == 0 {
        "ready"
    } else if indexed_chunks.unwrap_or(0) > 0 {
        "budget_exhausted"
    } else {
        report.status.as_str()
    };
    Ok(daemon_semantic_job_json(
        status,
        None,
        last_run_at_ms,
        &report,
        indexed_chunks,
        None,
    ))
}

fn daemon_semantic_requested_seconds(args: &DaemonRunArgs) -> u64 {
    semantic_worker_seconds_budget(&SemanticWorkerArgs {
        max_chunks: args.max_chunks,
        max_seconds: args.max_seconds,
    })
}

fn daemon_semantic_worker_seconds_budget(args: &DaemonRunArgs, deadline: Option<Instant>) -> u64 {
    let requested = daemon_semantic_requested_seconds(args);
    let Some(remaining) = daemon_deadline_remaining(deadline) else {
        return if deadline.is_none() { requested } else { 0 };
    };
    let remaining_secs = remaining
        .as_secs()
        .saturating_sub(DAEMON_SEMANTIC_RESERVE_GRACE_SECS);
    requested.min(remaining_secs)
}

fn daemon_semantic_deadline_skipped_job(data_root: &Path) -> Value {
    let report = semantic_worker_report_for_daemon(data_root);
    daemon_semantic_job_json(
        "skipped",
        Some("daemon_deadline"),
        utc_now().timestamp_millis(),
        &report,
        None,
        None,
    )
}

fn daemon_semantic_autostart_skipped_job(data_root: &Path) -> Value {
    let report = semantic_worker_report_for_daemon(data_root);
    daemon_semantic_job_json(
        "skipped",
        Some("autostart_history_only"),
        utc_now().timestamp_millis(),
        &report,
        None,
        None,
    )
}

fn daemon_semantic_failed_job(data_root: &Path, message: String) -> Value {
    let report = semantic_worker_report_for_daemon(data_root);
    daemon_semantic_job_json(
        "failed",
        None,
        utc_now().timestamp_millis(),
        &report,
        None,
        Some(message),
    )
}

fn daemon_semantic_job_json(
    status: &str,
    reason: Option<&str>,
    last_run_at_ms: i64,
    report: &SemanticWorkerReport,
    indexed_chunks: Option<usize>,
    last_error: Option<String>,
) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "status": status,
        "enabled": true,
        "reason": reason,
        "last_run_at_ms": last_run_at_ms,
        "last_error": last_error,
        "indexed_chunks": indexed_chunks,
        "model_cache_available": report.model_cache_available,
        "worker_status": report.status,
        "coverage": {
            "searchable_items": report.searchable_items,
            "completed_items": report.embedded_items,
            "embedded_items": report.embedded_items,
            "embedded_chunks": report.embedded_chunks,
            "dirty_items": report.dirty_items,
            "queued_items_estimate": report.queued_items_estimate,
        },
    }))
}

fn daemon_cloud_sync_disabled_job(last_run_at_ms: Option<i64>) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "status": "disabled",
        "enabled": false,
        "reason": "not_configured",
        "network_allowed": false,
        "last_run_at_ms": last_run_at_ms,
        "last_upload_at_ms": Value::Null,
        "queued_items_estimate": 0,
        "last_error": Value::Null,
    }))
}

fn write_daemon_lifecycle_status(
    data_root: &Path,
    args: &DaemonRunArgs,
    status: &str,
    started_at_ms: i64,
    finished_at_ms: Option<i64>,
    last_error: Option<String>,
) -> Result<()> {
    write_daemon_status(
        data_root,
        &compact_json(json!({
            "schema_version": 1,
            "status": status,
            "pid": process::id(),
            "started_at_ms": started_at_ms,
            "heartbeat_at_ms": utc_now().timestamp_millis(),
            "finished_at_ms": finished_at_ms,
            "start_mode": daemon_run_start_mode(args).as_str(),
            "trigger_command": args.trigger_command.map(DaemonTriggerCommandArg::as_str),
            "last_error": last_error,
        })),
    )
}

fn semantic_worker_report_for_daemon(data_root: &Path) -> SemanticWorkerReport {
    let db_path = database_path(data_root.to_path_buf());
    if db_path.exists() {
        match open_existing_store_snapshot_read_only(&db_path, "ctx daemon status") {
            Ok(store) => {
                return semantic_worker_report(data_root, Some(&store)).unwrap_or_else(|error| {
                    SemanticWorkerReport::unavailable(data_root, format!("{error:#}"))
                });
            }
            Err(error) => {
                return SemanticWorkerReport::unavailable(data_root, format!("{error:#}"));
            }
        }
    }
    semantic_worker_report_best_effort(data_root)
}

fn write_semantic_worker_failure_status(data_root: &Path, message: String) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": "failed",
            "pid": process::id(),
            "heartbeat_at_ms": now,
            "finished_at_ms": now,
            "last_error": message,
        }),
    )
}

fn run_semantic_worker_inner_with_embedder(
    args: SemanticWorkerArgs,
    data_root: &Path,
    query_hint: Option<String>,
    embedder: &mut Option<SemanticEmbedder>,
) -> Result<()> {
    let Some(_lock) = SemanticWorkerLock::acquire(data_root)? else {
        return Ok(());
    };

    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        return Err(anyhow!(
            "ctx index does not exist yet; run `ctx import --all` or `ctx setup` first"
        ));
    }
    let cache_dir = semantic_worker_cache_dir(data_root);
    if embedder.is_none() && !semantic_model_cache_available(&cache_dir) {
        return Err(anyhow!(
            "semantic model is not available in the local cache; background indexing will not initialize or download {SEMANTIC_MODEL_ID}"
        ));
    }
    let store = open_existing_store_read_only(&db_path, "ctx semantic worker")?;
    let vector_path = semantic_vector_path(data_root);
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    let prune_outcome = vector_store.prune_ineligible_events(&store)?;
    let started_at_ms = utc_now().timestamp_millis();
    let initial_stats = vector_store
        .cached_stats()?
        .unwrap_or_else(SemanticSidecarStats::default);
    let initial_dirty_items = vector_store.dirty_event_count()?;
    let searchable_items = store.event_embedding_document_count_cached_or_exact()?;
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": "running",
            "pid": process::id(),
            "started_at_ms": started_at_ms,
            "heartbeat_at_ms": started_at_ms,
            "indexed_chunks": 0,
            "pruned_chunks": prune_outcome.deleted_chunks,
            "stale_events_queued": prune_outcome.queued_stale_events,
            "searchable_items": searchable_items,
            "embedded_items": initial_stats.embedded_items,
            "embedded_chunks": initial_stats.embedded_chunks,
            "dirty_items": initial_dirty_items,
            "last_error": null,
        }),
    )?;
    let max_chunks = semantic_worker_chunk_budget(&args);
    let max_seconds = semantic_worker_seconds_budget(&args);
    let started = Instant::now();
    let deadline = started + StdDuration::from_secs(max_seconds);
    let mut model_init_ms = None;
    let indexed_chunks = if Instant::now() >= deadline {
        0
    } else {
        backfill_semantic_embeddings(
            &store,
            &mut vector_store,
            embedder,
            &mut model_init_ms,
            &cache_dir,
            query_hint.as_deref(),
            max_chunks,
            true,
            true,
            Some(deadline),
        )?
    };
    let elapsed = started.elapsed();
    let elapsed_ms = elapsed.as_millis() as u64;
    let final_stats = vector_store
        .cached_stats()?
        .unwrap_or_else(SemanticSidecarStats::default);
    let final_dirty_items = vector_store.dirty_event_count()?;
    let searchable_items = store.event_embedding_document_count_cached_or_exact()?;
    let status = if searchable_items > 0
        && final_stats.embedded_items >= searchable_items
        && final_dirty_items == 0
    {
        "ready"
    } else if elapsed >= StdDuration::from_secs(max_seconds) {
        "budget_exhausted"
    } else {
        "completed"
    };
    let finished_at_ms = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": status,
            "pid": process::id(),
            "started_at_ms": started_at_ms,
            "heartbeat_at_ms": finished_at_ms,
            "finished_at_ms": finished_at_ms,
            "indexed_chunks": indexed_chunks,
            "pruned_chunks": prune_outcome.deleted_chunks,
            "stale_events_queued": prune_outcome.queued_stale_events,
            "elapsed_ms": elapsed_ms,
            "model_init_ms": model_init_ms,
            "searchable_items": searchable_items,
            "embedded_items": final_stats.embedded_items,
            "embedded_chunks": final_stats.embedded_chunks,
            "dirty_items": final_dirty_items,
            "last_error": null,
        }),
    )?;
    drop(_lock);
    Ok(())
}

fn semantic_worker_chunk_budget(args: &SemanticWorkerArgs) -> usize {
    args.max_chunks
        .or_else(|| env_usize("CTX_SEMANTIC_WORKER_MAX_CHUNKS"))
        .map(|value| value.min(SEMANTIC_WORKER_BATCH_MAX))
        .unwrap_or(SEMANTIC_WORKER_BATCH_DEFAULT)
}

fn semantic_worker_seconds_budget(args: &SemanticWorkerArgs) -> u64 {
    args.max_seconds
        .or_else(|| {
            env::var("CTX_SEMANTIC_WORKER_MAX_SECONDS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
        })
        .map(|value| value.min(SEMANTIC_WORKER_MAX_SECONDS_CAP))
        .unwrap_or(SEMANTIC_WORKER_MAX_SECONDS_DEFAULT)
}

fn queue_recent_semantic_work(data_root: &Path, store: &Store, reason: &str) -> Result<usize> {
    let vector_path = semantic_vector_path(data_root);
    if !vector_path.exists()
        && !semantic_model_cache_available(&semantic_worker_cache_dir(data_root))
    {
        return Ok(0);
    }
    let docs = store.recent_event_embedding_documents(None, SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT)?;
    if docs.is_empty() {
        return Ok(0);
    }
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    let existing_hashes = vector_store
        .existing_hashes_for_event_ids(&docs.iter().map(|doc| doc.event_id).collect::<Vec<_>>())?;
    let docs = docs
        .into_iter()
        .filter(|doc| {
            let source_text = semantic_source_text(&doc.text);
            let hash = semantic_document_hash(doc, &source_text);
            existing_hashes
                .get(&doc.event_id)
                .map(|existing| existing != &hash)
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    vector_store.enqueue_dirty_documents(&docs, reason)
}

pub(crate) fn maybe_autostart_daemon(
    data_root: &Path,
    config: &AppConfig,
    trigger: DaemonTriggerCommandArg,
    json_output: bool,
) {
    if json_output
        || !config.daemon.enabled
        || semantic_env_flag(DAEMON_BACKGROUND_CHILD_ENV)
        || semantic_env_flag(DAEMON_AUTOSTART_OFF_ENV)
        || semantic_env_flag("CI")
        || !database_path(data_root.to_path_buf()).exists()
    {
        return;
    }
    let lock_path = daemon_lock_path(data_root);
    if lock_path.exists() && !daemon_lock_is_stale(&lock_path) {
        return;
    }
    let Ok(exe) = env::current_exe() else {
        return;
    };
    let max_runtime = daemon_autostart_u64_env(
        "CTX_DAEMON_AUTOSTART_MAX_RUNTIME_SECONDS",
        DAEMON_AUTOSTART_MAX_RUNTIME_SECONDS_DEFAULT,
        DAEMON_RUNTIME_SECONDS_CAP,
    );
    let idle_exit = daemon_autostart_u64_env(
        "CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS",
        DAEMON_AUTOSTART_IDLE_EXIT_SECONDS_DEFAULT,
        DAEMON_RUNTIME_SECONDS_CAP,
    );
    let loop_interval = daemon_autostart_u64_env(
        "CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS",
        DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS_DEFAULT,
        3_600,
    );
    let _ = Command::new(exe)
        .arg("--data-root")
        .arg(data_root)
        .arg("daemon")
        .arg("run")
        .arg("--once")
        .arg("--max-runtime-seconds")
        .arg(max_runtime.to_string())
        .arg("--idle-exit-seconds")
        .arg(idle_exit.to_string())
        .arg("--loop-interval-seconds")
        .arg(loop_interval.to_string())
        .arg("--start-mode")
        .arg(DaemonStartModeArg::Auto.as_str())
        .arg("--trigger-command")
        .arg(trigger.as_str())
        .arg("--json")
        .env(DAEMON_BACKGROUND_CHILD_ENV, "1")
        .env("CTX_ANALYTICS_OFF", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn daemon_autostart_u64_env(name: &str, default: u64, max: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(max))
        .unwrap_or(default)
}

fn semantic_env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

pub(crate) fn semantic_health_findings(data_root: &Path) -> Vec<String> {
    let mut findings = Vec::new();
    let semantic_lock = semantic_worker_lock_path(data_root);
    if semantic_lock.exists() && semantic_worker_lock_is_stale(&semantic_lock) {
        findings.push(format!(
            "semantic worker lock is stale: {}",
            semantic_lock.display()
        ));
    }
    let daemon_lock = daemon_lock_path(data_root);
    if daemon_lock.exists() && daemon_lock_is_stale(&daemon_lock) {
        findings.push(format!("daemon lock is stale: {}", daemon_lock.display()));
    }
    if let Some(status) = read_semantic_worker_status(data_root) {
        if status.get("status").and_then(Value::as_str) == Some("failed") {
            let error = json_string(&status, "last_error").unwrap_or_else(|| "unknown".to_owned());
            findings.push(format!("semantic worker last failed: {error}"));
        }
    }
    if let Some(status) = read_daemon_status(data_root) {
        if status.get("status").and_then(Value::as_str) == Some("failed") {
            let error = json_string(&status, "last_error").unwrap_or_else(|| "unknown".to_owned());
            findings.push(format!("daemon last failed: {error}"));
        }
    }
    let vector_path = semantic_vector_path(data_root);
    if vector_path.exists() {
        match SemanticVectorStore::open_read_only(&vector_path) {
            Ok(Some(vector_store)) => match vector_store.plaintext_value_count() {
                Ok(0) => {}
                Ok(count) => findings.push(format!(
                    "semantic vector sidecar contains {count} plaintext value(s); run daemon maintenance to scrub it"
                )),
                Err(error) => findings.push(format!(
                    "semantic vector sidecar plaintext check failed: {error:#}"
                )),
            },
            Ok(None) => {}
            Err(error) => findings.push(format!(
                "semantic vector sidecar is unreadable at {}: {error:#}",
                vector_path.display()
            )),
        }
    }
    findings
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

fn json_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|value| value.as_i64())
}

fn json_u32(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
}

fn json_usize(value: &Value, key: &str) -> Option<usize> {
    value
        .get(key)
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
}

fn create_private_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("create private directory {}", path.display()))?;
    secure_private_dir_permissions(path)?;
    Ok(())
}

fn private_create_new_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

#[cfg(unix)]
fn secure_private_dir_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("secure private directory {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_private_dir_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn secure_private_file_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("secure private file {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_private_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn secure_semantic_vector_permissions(path: &Path) -> Result<()> {
    for candidate in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        if candidate.exists() {
            fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("secure semantic vector file {}", candidate.display()))?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_semantic_vector_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn sqlite_column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sqlite_table_has_columns(conn: &Connection, table: &str, columns: &[&str]) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    let mut existing = std::collections::HashSet::new();
    while let Some(row) = rows.next()? {
        existing.insert(row.get::<_, String>(1)?);
    }
    Ok(columns.iter().all(|column| existing.contains(*column)))
}

fn sqlite_table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok(exists)
}

fn sqlite_table_sql(conn: &Connection, table: &str) -> Result<Option<String>> {
    let sql = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(sql)
}

fn semantic_query_text(query: &str, terms: &[String]) -> String {
    let mut parts = Vec::new();
    if !query.trim().is_empty() {
        parts.push(query.trim().to_owned());
    }
    parts.extend(
        terms
            .iter()
            .map(|term| term.trim())
            .filter(|term| !term.is_empty())
            .map(str::to_owned),
    );
    parts.join(" ")
}

fn semantic_filters_need_overfetch(filters: &ctx_history_search::SearchFilters) -> bool {
    semantic_filters_require_lexical_fallback(filters)
        || !filters.include_subagents
        || filters.exclude_provider_session.is_some()
}

fn semantic_filters_require_lexical_fallback(filters: &ctx_history_search::SearchFilters) -> bool {
    filters.session.is_some()
        || filters.provider.is_some()
        || filters.history_source.is_some()
        || filters.provider_key.is_some()
        || filters.source_id.is_some()
        || filters.source_format.is_some()
        || filters
            .repo
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
        || filters.since.is_some()
        || filters.primary_only
        || filters.event_type.is_some()
        || filters
            .file
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
}

fn semantic_hybrid_coverage_ready(embedded_items: usize, searchable_items: usize) -> bool {
    if embedded_items == 0 {
        return false;
    }
    if searchable_items == 0 {
        return true;
    }
    embedded_items >= SEMANTIC_HYBRID_MIN_EMBEDDED_ITEMS
        || (embedded_items as f64 / searchable_items as f64) >= SEMANTIC_HYBRID_MIN_COVERAGE_RATIO
}

fn semantic_status_needs_exact_sidecar_stats(
    searchable_items: usize,
    dirty_items: usize,
    stats: SemanticSidecarStats,
) -> bool {
    if searchable_items == 0 || dirty_items > 0 {
        return false;
    }
    stats.embedded_items >= searchable_items
        || !semantic_hybrid_coverage_ready(stats.embedded_items, searchable_items)
}

fn semantic_auto_candidate_event_ids_from_packet(
    packet: &ctx_history_search::SearchPacket,
) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    let mut event_ids = Vec::new();
    for result in &packet.results {
        if let Some(event_id) = result.event_id {
            if seen.insert(event_id) {
                event_ids.push(event_id);
            }
        }
    }
    event_ids
}

fn semantic_auto_candidate_coverage_ready(
    embedded_candidates: usize,
    total_candidates: usize,
) -> bool {
    total_candidates > 0 && embedded_candidates == total_candidates
}

fn reciprocal_rank(rank: usize) -> f32 {
    1.0 / (60.0 + rank.max(1) as f32)
}

fn push_unique_reason(reasons: &mut Vec<String>, reason: &str) {
    if !reasons.iter().any(|value| value == reason) {
        reasons.push(reason.to_owned());
    }
}

fn normalize_packet_result_ranks(results: &mut [ctx_history_search::SearchPacketResult]) {
    let max_rank = results
        .iter()
        .map(|result| result.rank)
        .fold(0.0_f32, f32::max);
    if max_rank <= 0.0 {
        return;
    }
    for result in results {
        result.rank = (result.rank / max_rank).clamp(0.0, 1.0);
        if result.result_scope == ctx_history_search::SearchResultScope::Session {
            result.session_importance =
                session_importance(result.rank, result.more_matches_in_session);
        } else {
            result.session_importance = 0.0;
        }
    }
}

fn session_importance(rank: f32, more_matches_in_session: usize) -> f32 {
    let coverage_boost = ((more_matches_in_session as f32).ln_1p() * 0.08).min(0.24);
    (rank + coverage_boost).clamp(0.0, 1.0)
}

fn compare_packet_results(
    left: &ctx_history_search::SearchPacketResult,
    right: &ctx_history_search::SearchPacketResult,
) -> std::cmp::Ordering {
    right
        .rank
        .partial_cmp(&left.rank)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| right.timestamp.cmp(&left.timestamp))
        .then_with(|| left.record_id.cmp(&right.record_id))
}

fn semantic_auto_rerank_packet(
    mut packet: ctx_history_search::SearchPacket,
    semantic_hits: &[ctx_history_search::SemanticEventHit],
    semantic_weight: f32,
) -> ctx_history_search::SearchPacket {
    let semantic_weight = semantic_weight.clamp(0.0, 1.0);
    let mut semantic_by_event = HashMap::<Uuid, f32>::new();
    let mut semantic_by_session = HashMap::<Uuid, f32>::new();
    for (index, semantic_hit) in semantic_hits.iter().enumerate() {
        let score = reciprocal_rank(index + 1);
        semantic_by_event
            .entry(semantic_hit.hit.event_id)
            .and_modify(|existing| *existing = existing.max(score))
            .or_insert(score);
        if let Some(session_id) = semantic_hit.hit.session_id {
            semantic_by_session
                .entry(session_id)
                .and_modify(|existing| *existing = existing.max(score))
                .or_insert(score);
        }
    }

    for (index, result) in packet.results.iter_mut().enumerate() {
        let lexical = reciprocal_rank(index + 1);
        let semantic = result
            .event_id
            .and_then(|event_id| semantic_by_event.get(&event_id).copied())
            .or_else(|| {
                result
                    .session_id
                    .and_then(|session_id| semantic_by_session.get(&session_id).copied())
            })
            .unwrap_or(0.0);
        result.rank = ((1.0 - semantic_weight) * lexical) + (semantic_weight * semantic);
        if semantic > 0.0 {
            push_unique_reason(&mut result.why_matched, "semantic_similarity");
            push_unique_reason(&mut result.why_matched, "semantic:auto_rerank");
        }
    }
    packet.results.sort_by(compare_packet_results);
    normalize_packet_result_ranks(&mut packet.results);
    packet
}

fn semantic_hits_for_text_query(
    store: &Store,
    vector_store: &SemanticVectorStore,
    cache_dir: &Path,
    semantic_text: &str,
    limit: usize,
    event_filter: Option<&[Uuid]>,
) -> Result<(
    Vec<ctx_history_search::SemanticEventHit>,
    SemanticRetrievalDiagnostics,
)> {
    let query_embed_started = Instant::now();
    let mut embedder = new_semantic_embedder(cache_dir)?;
    let mut embeddings = embed_texts(&mut embedder, vec![semantic_text.to_owned()])?;
    let query_embed_ms = query_embed_started.elapsed().as_millis() as u64;
    let query_embedding = embeddings
        .pop()
        .ok_or_else(|| anyhow!("semantic query embedding was empty"))?;
    let semantic_hit_search =
        semantic_hits_for_query(store, vector_store, &query_embedding, limit, event_filter)?;
    let mut diagnostics = semantic_hit_search.diagnostics;
    diagnostics.query_embed_ms = Some(query_embed_ms);
    Ok((semantic_hit_search.hits, diagnostics))
}

#[cfg(ctx_semantic_fastembed)]
struct SemanticEmbedder {
    model: TextEmbedding,
    batch_size: usize,
}

#[cfg(not(ctx_semantic_fastembed))]
struct SemanticEmbedder;

#[cfg(ctx_semantic_fastembed)]
fn new_semantic_embedder(cache_dir: &Path) -> Result<SemanticEmbedder> {
    let snapshot = semantic_model_cache_snapshot_dir(cache_dir).ok_or_else(|| {
        anyhow!(
            "semantic model cache is incomplete at {}",
            cache_dir.display()
        )
    })?;
    let model_info = TextEmbedding::get_model_info(&EmbeddingModel::AllMiniLML6V2)?;
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: fs::read(snapshot.join("tokenizer.json"))
            .with_context(|| format!("read semantic tokenizer.json from {}", snapshot.display()))?,
        config_file: fs::read(snapshot.join("config.json"))
            .with_context(|| format!("read semantic config.json from {}", snapshot.display()))?,
        special_tokens_map_file: fs::read(snapshot.join("special_tokens_map.json")).with_context(
            || {
                format!(
                    "read semantic special_tokens_map.json from {}",
                    snapshot.display()
                )
            },
        )?,
        tokenizer_config_file: fs::read(snapshot.join("tokenizer_config.json")).with_context(
            || {
                format!(
                    "read semantic tokenizer_config.json from {}",
                    snapshot.display()
                )
            },
        )?,
    };
    let mut user_model = UserDefinedEmbeddingModel::new(
        fs::read(snapshot.join(&model_info.model_file)).with_context(|| {
            format!(
                "read semantic model file {} from {}",
                model_info.model_file,
                snapshot.display()
            )
        })?,
        tokenizer_files,
    )
    .with_pooling(
        TextEmbedding::get_default_pooling_method(&EmbeddingModel::AllMiniLML6V2)
            .unwrap_or(Pooling::Mean),
    )
    .with_quantization(TextEmbedding::get_quantization_mode(
        &EmbeddingModel::AllMiniLML6V2,
    ));
    user_model.output_key = model_info.output_key.clone();
    let options = InitOptionsUserDefined::new().with_intra_threads(semantic_embedder_threads());
    let model = TextEmbedding::try_new_from_user_defined(user_model, options)
        .with_context(|| format!("initialize semantic embedding model {SEMANTIC_MODEL_ID}"))?;
    Ok(SemanticEmbedder {
        model,
        batch_size: semantic_embed_batch_size(),
    })
}

#[cfg(not(ctx_semantic_fastembed))]
fn new_semantic_embedder(_cache_dir: &Path) -> Result<SemanticEmbedder> {
    Err(anyhow!(
        "semantic embedding model {SEMANTIC_MODEL_ID} is not supported on this platform"
    ))
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_embedder_threads() -> usize {
    env_usize("CTX_SEMANTIC_THREADS")
        .map(|value| value.min(SEMANTIC_EMBED_THREADS_MAX))
        .or_else(|| {
            std::thread::available_parallelism()
                .ok()
                .map(|threads| threads.get().clamp(1, SEMANTIC_EMBED_THREADS_DEFAULT))
        })
        .unwrap_or(SEMANTIC_EMBED_THREADS_DEFAULT)
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_embed_batch_size() -> usize {
    env_usize("CTX_SEMANTIC_EMBED_BATCH")
        .map(|value| value.min(SEMANTIC_EMBED_BATCH_MAX))
        .unwrap_or(SEMANTIC_EMBED_BATCH_DEFAULT)
}

fn semantic_cache_dir() -> Option<PathBuf> {
    env::var("CTX_SEMANTIC_CACHE_DIR")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn semantic_worker_cache_dir(data_root: &Path) -> PathBuf {
    env::var("HF_HOME")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(semantic_cache_dir)
        .or_else(|| {
            env::var("FASTEMBED_CACHE_DIR")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| data_root.join("semantic-model-cache"))
}

fn semantic_model_cache_available(cache_dir: &Path) -> bool {
    semantic_model_cache_snapshot_dir(cache_dir).is_some()
}

fn semantic_model_cache_snapshot_dir(cache_dir: &Path) -> Option<PathBuf> {
    if !semantic_embedding_supported() {
        return None;
    }
    if cache_dir.as_os_str().is_empty() {
        return None;
    }
    let model_root = cache_dir.join(SEMANTIC_HF_MODEL_CACHE_DIR);
    let snapshot_ref = fs::read_to_string(model_root.join("refs").join("main")).ok()?;
    let snapshot_ref = snapshot_ref.trim();
    if snapshot_ref.is_empty()
        || snapshot_ref.contains('/')
        || snapshot_ref.contains('\\')
        || snapshot_ref == "."
        || snapshot_ref == ".."
    {
        return None;
    }
    let snapshot = model_root.join("snapshots").join(snapshot_ref);
    if !snapshot.is_dir() {
        return None;
    }
    if SEMANTIC_REQUIRED_MODEL_FILES.iter().all(|file| {
        fs::metadata(snapshot.join(file))
            .map(|metadata| metadata.is_file() && metadata.len() > 0)
            .unwrap_or(false)
    }) {
        Some(snapshot)
    } else {
        None
    }
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_embedding_supported() -> bool {
    true
}

#[cfg(not(ctx_semantic_fastembed))]
fn semantic_embedding_supported() -> bool {
    false
}

fn env_usize(name: &str) -> Option<usize> {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
}

#[cfg(ctx_semantic_fastembed)]
fn embed_texts(embedder: &mut SemanticEmbedder, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    let mut embeddings = embedder
        .model
        .embed(texts, Some(embedder.batch_size))
        .with_context(|| format!("embed text with semantic model {SEMANTIC_MODEL_ID}"))?;
    for embedding in &mut embeddings {
        if embedding.len() != SEMANTIC_DIMENSIONS {
            return Err(anyhow!(
                "semantic model returned {} dimensions, expected {}",
                embedding.len(),
                SEMANTIC_DIMENSIONS
            ));
        }
        normalize_embedding(embedding);
    }
    Ok(embeddings)
}

#[cfg(not(ctx_semantic_fastembed))]
fn embed_texts(_embedder: &mut SemanticEmbedder, _texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    Err(anyhow!(
        "semantic embedding model {SEMANTIC_MODEL_ID} is not supported on this platform"
    ))
}

#[allow(clippy::too_many_arguments)]
fn backfill_semantic_embeddings(
    store: &Store,
    vector_store: &mut SemanticVectorStore,
    embedder: &mut Option<SemanticEmbedder>,
    model_init_ms: &mut Option<u64>,
    cache_dir: &Path,
    query_text: Option<&str>,
    max_to_index: usize,
    json_output: bool,
    continue_past_indexed_pages: bool,
    deadline: Option<Instant>,
) -> Result<usize> {
    let mut existing_hashes = HashMap::new();
    let mut before = None;
    let mut indexed = 0_usize;
    let mut scanned = 0_usize;

    let dirty_ids =
        vector_store.queued_dirty_event_ids(max_to_index.min(SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT))?;
    if !dirty_ids.is_empty() && indexed < max_to_index {
        let docs = store.event_embedding_documents_by_ids(&dirty_ids)?;
        extend_existing_hashes_for_docs(vector_store, &mut existing_hashes, &docs)?;
        let found_event_ids = docs.iter().map(|doc| doc.event_id).collect::<HashSet<_>>();
        let mut consumed_event_ids = dirty_ids
            .iter()
            .filter(|event_id| !found_event_ids.contains(event_id))
            .copied()
            .collect::<Vec<_>>();
        scanned = scanned.saturating_add(docs.len());
        let outcome = index_semantic_documents(
            vector_store,
            embedder,
            model_init_ms,
            cache_dir,
            &mut existing_hashes,
            docs,
            max_to_index.saturating_sub(indexed),
            deadline,
        )?;
        indexed = indexed.saturating_add(outcome.indexed_chunks);
        consumed_event_ids.extend(outcome.consumed_event_ids);
        if !consumed_event_ids.is_empty() {
            vector_store.dequeue_dirty_events(&consumed_event_ids)?;
        }
        if indexed > 0 && !json_output {
            eprintln!(
                "semantic index: embedded {indexed} dirty-priority chunks (scanned {scanned} events)"
            );
        }
    }

    if indexed < max_to_index {
        if let Some(query_text) = query_text {
            let terms = semantic_backfill_terms(query_text);
            if !terms.is_empty() {
                let remaining = max_to_index.saturating_sub(indexed);
                let docs = store.event_embedding_documents_matching_terms(&terms, remaining)?;
                extend_existing_hashes_for_docs(vector_store, &mut existing_hashes, &docs)?;
                scanned = scanned.saturating_add(docs.len());
                let outcome = index_semantic_documents(
                    vector_store,
                    embedder,
                    model_init_ms,
                    cache_dir,
                    &mut existing_hashes,
                    docs,
                    remaining,
                    deadline,
                )?;
                indexed = indexed.saturating_add(outcome.indexed_chunks);
                if outcome.indexed_chunks > 0 && !json_output {
                    eprintln!(
                        "semantic index: embedded {indexed} query-directed chunks (scanned {scanned} events)"
                    );
                }
            }
        }
    }

    while indexed < max_to_index {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break;
        }
        let docs = store.recent_event_embedding_documents(before, 512)?;
        if docs.is_empty() {
            break;
        }
        before = docs.last().map(|doc| (doc.occurred_at_ms, doc.seq));
        extend_existing_hashes_for_docs(vector_store, &mut existing_hashes, &docs)?;
        scanned = scanned.saturating_add(docs.len());
        let outcome = index_semantic_documents(
            vector_store,
            embedder,
            model_init_ms,
            cache_dir,
            &mut existing_hashes,
            docs,
            max_to_index.saturating_sub(indexed),
            deadline,
        )?;
        let added = outcome.indexed_chunks;
        indexed = indexed.saturating_add(added);
        if !json_output {
            eprintln!("semantic index: embedded {indexed} chunks (scanned {scanned} events)");
        }
        if added == 0 && !continue_past_indexed_pages {
            break;
        }
    }
    Ok(indexed)
}

fn extend_existing_hashes_for_docs(
    vector_store: &SemanticVectorStore,
    existing_hashes: &mut HashMap<Uuid, String>,
    docs: &[EventEmbeddingDocument],
) -> Result<()> {
    let event_ids = docs
        .iter()
        .map(|doc| doc.event_id)
        .filter(|event_id| !existing_hashes.contains_key(event_id))
        .collect::<Vec<_>>();
    if event_ids.is_empty() {
        return Ok(());
    }
    existing_hashes.extend(vector_store.existing_hashes_for_event_ids(&event_ids)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn index_semantic_documents(
    vector_store: &mut SemanticVectorStore,
    embedder: &mut Option<SemanticEmbedder>,
    model_init_ms: &mut Option<u64>,
    cache_dir: &Path,
    existing_hashes: &mut HashMap<Uuid, String>,
    docs: Vec<EventEmbeddingDocument>,
    limit: usize,
    deadline: Option<Instant>,
) -> Result<SemanticIndexOutcome> {
    let limit = semantic_deadline_chunk_limit(limit, deadline);
    if limit == 0 {
        return Ok(SemanticIndexOutcome::default());
    }
    let mut pending = Vec::<SemanticChunkDocument>::new();
    let mut unchanged_event_ids = Vec::new();
    let mut pending_event_ids = Vec::new();
    for doc in docs {
        let source_text = semantic_source_text(&doc.text);
        let text_hash = semantic_document_hash(&doc, &source_text);
        if existing_hashes
            .get(&doc.event_id)
            .is_some_and(|existing| existing == &text_hash)
        {
            unchanged_event_ids.push(doc.event_id);
            continue;
        }
        let chunks = semantic_chunks_for_document(&doc, &source_text, &text_hash);
        if chunks.len() > limit && pending.is_empty() {
            continue;
        }
        if pending.len().saturating_add(chunks.len()) > limit && !pending.is_empty() {
            break;
        }
        pending_event_ids.push(doc.event_id);
        pending.extend(chunks);
        if pending.len() >= limit {
            break;
        }
    }
    if pending.is_empty() {
        return Ok(SemanticIndexOutcome {
            indexed_chunks: 0,
            consumed_event_ids: unchanged_event_ids,
        });
    }
    let texts = pending
        .iter()
        .map(|doc| doc.text.clone())
        .collect::<Vec<_>>();
    if embedder.is_none() {
        if !semantic_deadline_has_model_init_budget(deadline) {
            return Ok(SemanticIndexOutcome {
                indexed_chunks: 0,
                consumed_event_ids: unchanged_event_ids,
            });
        }
        let model_init_started = Instant::now();
        *embedder = Some(new_semantic_embedder(cache_dir)?);
        *model_init_ms = Some(model_init_started.elapsed().as_millis() as u64);
    }
    let embedder = embedder
        .as_mut()
        .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?;
    let embeddings = embed_texts(embedder, texts)?;
    let items = pending
        .into_iter()
        .zip(embeddings)
        .map(|(doc, embedding)| {
            existing_hashes.insert(doc.event_id, doc.source_text_hash.clone());
            (doc, embedding)
        })
        .collect::<Vec<_>>();
    vector_store.upsert_chunk_embeddings(&items)?;
    unchanged_event_ids.extend(pending_event_ids);
    Ok(SemanticIndexOutcome {
        indexed_chunks: items.len(),
        consumed_event_ids: unchanged_event_ids,
    })
}

fn semantic_deadline_chunk_limit(limit: usize, deadline: Option<Instant>) -> usize {
    let Some(deadline) = deadline else {
        return limit;
    };
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        return 0;
    };
    let seconds = remaining.as_secs() as usize;
    if seconds == 0 {
        return 0;
    }
    let deadline_limit = seconds
        .saturating_mul(SEMANTIC_DEADLINE_CHUNKS_PER_SECOND)
        .max(SEMANTIC_DEADLINE_MIN_CHUNK_BATCH);
    limit.min(deadline_limit)
}

fn semantic_deadline_has_model_init_budget(deadline: Option<Instant>) -> bool {
    let Some(deadline) = deadline else {
        return true;
    };
    deadline
        .checked_duration_since(Instant::now())
        .is_some_and(|remaining| {
            remaining >= StdDuration::from_secs(SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS)
        })
}

fn semantic_source_text(text: &str) -> String {
    text.chars().take(SEMANTIC_SOURCE_MAX_CHARS).collect()
}

fn semantic_chunks_for_document(
    doc: &EventEmbeddingDocument,
    source_text: &str,
    source_text_hash: &str,
) -> Vec<SemanticChunkDocument> {
    let chunks = semantic_text_chunks(source_text);
    let chunk_count = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(
            |(chunk_index, (start_char, end_char, text))| SemanticChunkDocument {
                event_id: doc.event_id,
                history_record_id: doc.history_record_id,
                session_id: doc.session_id,
                seq: doc.seq,
                chunk_index,
                chunk_count,
                source_text_hash: source_text_hash.to_owned(),
                chunk_text_hash: semantic_text_hash(&semantic_embedded_chunk_text(doc, &text)),
                text: semantic_embedded_chunk_text(doc, &text),
                start_char,
                end_char,
            },
        )
        .collect()
}

fn semantic_document_hash(doc: &EventEmbeddingDocument, source_text: &str) -> String {
    semantic_text_hash(&semantic_embedded_document_text(doc, source_text))
}

fn semantic_embedded_document_text(doc: &EventEmbeddingDocument, body: &str) -> String {
    semantic_embedded_chunk_text(doc, body)
}

fn semantic_embedded_chunk_text(doc: &EventEmbeddingDocument, body: &str) -> String {
    let header = semantic_document_header(doc);
    if header.is_empty() {
        body.to_owned()
    } else {
        format!("{header}\n\n{body}")
    }
}

fn semantic_document_header(doc: &EventEmbeddingDocument) -> String {
    let mut lines = vec![
        "semantic_document: v2".to_owned(),
        format!("event_type: {}", doc.event_type.as_str()),
    ];
    if let Some(role) = doc.role {
        lines.push(format!("role: {}", role.as_str()));
    }
    if !doc.rank_bucket.trim().is_empty() {
        lines.push(format!(
            "rank_bucket: {}",
            semantic_header_value(&doc.rank_bucket, 80)
        ));
    }
    if let Some(provider) = doc.provider {
        lines.push(format!("provider: {}", provider.as_str()));
    }
    if let Some(source_format) = doc.source_format.as_deref() {
        lines.push(format!(
            "source_format: {}",
            semantic_header_value(source_format, 120)
        ));
    }
    if let Some(agent_type) = doc.agent_type {
        lines.push(format!("agent_type: {}", agent_type.as_str()));
    }
    if let Some(is_primary) = doc.session_is_primary {
        lines.push(format!(
            "session_scope: {}",
            if is_primary { "primary" } else { "subagent" }
        ));
    }
    if let Some(workspace) = doc.record_workspace.as_deref() {
        lines.push(format!(
            "workspace_hint: {}",
            semantic_header_value(workspace, 160)
        ));
    }
    if let Some(cwd) = doc.cwd.as_deref().and_then(path_basename) {
        lines.push(format!("cwd_hint: {}", semantic_header_value(cwd, 120)));
    }
    if let Some(path) = doc.raw_source_path.as_deref().and_then(path_basename) {
        lines.push(format!(
            "source_file_hint: {}",
            semantic_header_value(path, 120)
        ));
    }
    if let Some(title) = doc.record_title.as_deref() {
        lines.push(format!("title_hint: {}", semantic_header_value(title, 180)));
    }
    if let Some(kind) = doc.record_kind.as_deref() {
        lines.push(format!("record_kind: {}", semantic_header_value(kind, 80)));
    }
    lines.join("\n")
}

fn semantic_header_value(value: &str, max_chars: usize) -> String {
    let sanitized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut output = sanitized.chars().take(max_chars).collect::<String>();
    if sanitized.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

fn path_basename(path: &str) -> Option<&str> {
    Path::new(path).file_name().and_then(|value| value.to_str())
}

fn semantic_text_chunks(text: &str) -> Vec<(usize, usize, String)> {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return Vec::new();
    }
    if chars.len() <= SEMANTIC_CHUNK_TARGET_CHARS {
        return vec![(0, chars.len(), text.to_owned())];
    }

    let mut chunks = Vec::new();
    let mut start = 0_usize;
    while start < chars.len() {
        let mut end = start
            .saturating_add(SEMANTIC_CHUNK_TARGET_CHARS)
            .min(chars.len());
        if end < chars.len() {
            let boundary_floor = end.saturating_sub(150).max(start + 1);
            for index in (boundary_floor..end).rev() {
                if chars[index].is_whitespace() {
                    end = index + 1;
                    break;
                }
            }
        }
        if end <= start {
            end = start
                .saturating_add(SEMANTIC_CHUNK_TARGET_CHARS)
                .min(chars.len());
        }
        let chunk = chars[start..end].iter().collect::<String>();
        chunks.push((start, end, chunk));
        if end >= chars.len() {
            break;
        }
        start = end.saturating_sub(SEMANTIC_CHUNK_OVERLAP_CHARS);
    }
    chunks
}

fn semantic_text_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(ctx_semantic_fastembed)]
fn normalize_embedding(values: &mut [f32]) {
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in values {
            *value /= norm;
        }
    }
}

fn semantic_tokens(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter_map(|token| {
            let token = token.trim().to_lowercase();
            if token.len() < 2 {
                None
            } else {
                Some(stem_semantic_token(&token))
            }
        })
        .collect()
}

fn semantic_backfill_terms(text: &str) -> Vec<String> {
    let mut terms = Vec::<String>::new();
    for token in semantic_tokens(text) {
        push_unique_term(&mut terms, &token);
        match canonical_semantic_token(&token) {
            Some("email") => {
                for term in ["mail", "email", "inbox", "mailbox", "zoho", "smtp"] {
                    push_unique_term(&mut terms, term);
                }
            }
            Some("send_limit") => {
                for term in ["throttle", "limit", "blocked", "bulk", "send", "sending"] {
                    push_unique_term(&mut terms, term);
                }
            }
            Some("agent_memory") => {
                for term in ["agentmemory", "memory", "memories"] {
                    push_unique_term(&mut terms, term);
                }
            }
            Some("outreach") => {
                for term in ["outreach", "lead", "enrich", "campaign", "reply"] {
                    push_unique_term(&mut terms, term);
                }
            }
            Some("hosted_team") => {
                for term in ["hosted", "cloud", "enterprise", "team", "shared"] {
                    push_unique_term(&mut terms, term);
                }
            }
            Some("market") => {
                for term in ["competitor", "pricing", "price", "matrix"] {
                    push_unique_term(&mut terms, term);
                }
            }
            _ => {}
        }
    }
    terms.truncate(20);
    terms
}

fn push_unique_term(terms: &mut Vec<String>, term: &str) {
    if term.len() >= 3 && !terms.iter().any(|existing| existing == term) {
        terms.push(term.to_owned());
    }
}

fn stem_semantic_token(token: &str) -> String {
    for suffix in ["ing", "ed", "es", "s"] {
        if token.len() > suffix.len() + 3 && token.ends_with(suffix) {
            return token[..token.len() - suffix.len()].to_owned();
        }
    }
    token.to_owned()
}

fn canonical_semantic_token(token: &str) -> Option<&'static str> {
    match token {
        "mail" | "email" | "inbox" | "mailbox" | "mx" | "spf" | "dmarc" | "smtp" | "zoho" => {
            Some("email")
        }
        "throttle" | "limit" | "quota" | "blocked" | "bulk" | "spike" | "send" | "sender"
        | "sending" => Some("send_limit"),
        "admin" | "reauth" | "password" | "delete" | "auth" => Some("auth_admin"),
        "agentmemory" | "memory" | "memories" | "remember" => Some("agent_memory"),
        "outreach" | "lead" | "leads" | "enrich" | "campaign" | "reply" | "buyer" => {
            Some("outreach")
        }
        "hosted" | "cloud" | "enterprise" | "team" | "shared" => Some("hosted_team"),
        "competitor" | "competitors" | "pricing" | "price" | "matrix" => Some("market"),
        "privacy" | "private" | "scoped" | "scope" | "governance" | "policy" => Some("governance"),
        "semantic" | "hybrid" | "vector" | "embedding" | "embeddings" => Some("semantic"),
        "subagent" | "subagents" | "worker" | "workers" => Some("subagent"),
        _ => None,
    }
}

fn serialize_f32_blob(values: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(values.len() * 4);
    for value in values {
        blob.extend_from_slice(&value.to_le_bytes());
    }
    blob
}

fn dot_product_f32_blob(left: &[f32], right_blob: &[u8]) -> Result<Option<f32>> {
    if !right_blob.len().is_multiple_of(4) {
        return Err(anyhow!(
            "invalid semantic vector blob length {}",
            right_blob.len()
        ));
    }
    if right_blob.len() / 4 != left.len() {
        return Ok(None);
    }
    let mut sum = 0.0_f32;
    for (value, chunk) in left.iter().zip(right_blob.chunks_exact(4)) {
        sum += value * f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    Ok(Some(sum))
}

fn compare_semantic_hits_desc(
    left: &SemanticVectorHit,
    right: &SemanticVectorHit,
) -> std::cmp::Ordering {
    right
        .similarity
        .partial_cmp(&left.similarity)
        .unwrap_or(std::cmp::Ordering::Equal)
}

fn semantic_hits_for_query(
    store: &Store,
    vector_store: &SemanticVectorStore,
    query_embedding: &[f32],
    limit: usize,
    event_filter: Option<&[Uuid]>,
) -> Result<SemanticHitSearch> {
    let sqlite_vec0_full_scan_ready =
        event_filter.is_none() && vector_store.sqlite_vec0_ready().unwrap_or(false);
    let vector_limit = if sqlite_vec0_full_scan_ready {
        limit.max(1)
    } else {
        limit.saturating_mul(SEMANTIC_VECTOR_OVERFETCH).max(limit)
    };
    let vector_search = if let Some(event_filter) = event_filter {
        vector_store.search_event_ids(query_embedding, event_filter, vector_limit)?
    } else {
        vector_store.search(query_embedding, vector_limit)?
    };
    let mut diagnostics = SemanticRetrievalDiagnostics {
        vector_backend: vector_search.stats.backend,
        vector_scan_ms: Some(vector_search.stats.scan_ms),
        chunks_scanned: Some(vector_search.stats.chunks_scanned),
        vector_bytes_read: Some(vector_search.stats.vector_bytes_read),
        events_scored: Some(vector_search.stats.events_scored),
        ..SemanticRetrievalDiagnostics::default()
    };
    let mut best_by_event = HashMap::<Uuid, SemanticVectorHit>::new();
    for hit in vector_search.hits {
        let replace = best_by_event
            .get(&hit.event_id)
            .map(|existing| hit.similarity > existing.similarity)
            .unwrap_or(true);
        if replace {
            best_by_event.insert(hit.event_id, hit);
        }
    }
    let mut vector_hits = best_by_event.into_values().collect::<Vec<_>>();
    vector_hits.sort_by(compare_semantic_hits_desc);
    let current_hashes = current_semantic_source_hashes(store, &vector_hits)?;
    let before_stale_filter = vector_hits.len();
    vector_hits.retain(|hit| {
        current_hashes
            .get(&hit.event_id)
            .is_some_and(|hash| hash == &hit.source_text_hash)
    });
    diagnostics.stale_events_dropped = Some(before_stale_filter.saturating_sub(vector_hits.len()));
    if vector_hits.len() > limit {
        vector_hits.truncate(limit);
    }
    let chunk_ranges = vector_hits
        .iter()
        .map(|hit| (hit.event_id, (hit.start_char, hit.end_char)))
        .collect::<HashMap<_, _>>();
    let hydration_started = Instant::now();
    let hydrated_hits = store.semantic_event_hits_by_id(&chunk_ranges)?;
    diagnostics.hydration_ms = Some(hydration_started.elapsed().as_millis() as u64);
    let hydrated_by_id = hydrated_hits
        .into_iter()
        .map(|hit| (hit.event_id, hit))
        .collect::<HashMap<_, _>>();
    let mut hits = Vec::new();
    for vector_hit in vector_hits {
        if let Some(hit) = hydrated_by_id.get(&vector_hit.event_id).cloned() {
            hits.push(ctx_history_search::SemanticEventHit {
                hit,
                similarity: vector_hit.similarity,
            });
        }
    }
    diagnostics.semantic_candidates = Some(hits.len());
    Ok(SemanticHitSearch { hits, diagnostics })
}

fn current_semantic_source_hashes(
    store: &Store,
    vector_hits: &[SemanticVectorHit],
) -> Result<HashMap<Uuid, String>> {
    let event_ids = vector_hits
        .iter()
        .map(|hit| hit.event_id)
        .collect::<Vec<_>>();
    let docs = store.event_embedding_documents_by_ids(&event_ids)?;
    Ok(docs
        .into_iter()
        .map(|doc| {
            let source_text = semantic_source_text(&doc.text);
            (doc.event_id, semantic_document_hash(&doc, &source_text))
        })
        .collect())
}

#[cfg(all(test, ctx_sqlite_vec))]
mod tests {
    use super::*;

    fn test_embedding(first: f32, second: f32) -> Vec<f32> {
        let mut embedding = vec![0.0; SEMANTIC_DIMENSIONS];
        embedding[0] = first;
        embedding[1] = second;
        embedding
    }

    fn empty_test_packet(
        query: &str,
        options: &ctx_history_search::PacketOptions,
    ) -> ctx_history_search::SearchPacket {
        ctx_history_search::SearchPacket {
            schema_version: ctx_history_search::SEARCH_PACKET_SCHEMA_VERSION,
            query: query.to_owned(),
            filters: options.filters.clone(),
            generated_at: utc_now(),
            results: Vec::new(),
            pagination: ctx_history_core::ContextPagination::default(),
            truncation: ctx_history_core::ContextTruncation::default(),
        }
    }

    fn test_chunk(event_id: Uuid, seq: u64, source_hash: &str) -> SemanticChunkDocument {
        test_chunk_at(event_id, seq, source_hash, 0, 1)
    }

    fn test_chunk_at(
        event_id: Uuid,
        seq: u64,
        source_hash: &str,
        chunk_index: usize,
        chunk_count: usize,
    ) -> SemanticChunkDocument {
        SemanticChunkDocument {
            event_id,
            history_record_id: None,
            session_id: None,
            seq,
            chunk_index,
            chunk_count,
            source_text_hash: source_hash.to_owned(),
            chunk_text_hash: format!("{source_hash}-chunk-{chunk_index}"),
            text: String::new(),
            start_char: chunk_index.saturating_mul(10),
            end_char: chunk_index.saturating_mul(10).saturating_add(12),
        }
    }

    #[test]
    fn sqlite_vec0_full_scan_matches_rust_scan() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let close_event = Uuid::new_v4();
        let far_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk(close_event, 2, "close"),
                test_embedding(1.0, 0.0),
            ),
            (test_chunk(far_event, 1, "far"), test_embedding(0.0, 1.0)),
        ])?;

        assert!(store.sqlite_vec0_ready()?);

        let query = test_embedding(1.0, 0.0);
        let sqlite_hits = store.search(&query, 2)?;
        let rust_hits = store.search_event_ids(&query, &[close_event, far_event], 2)?;

        assert_eq!(
            sqlite_hits.stats.backend,
            Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC)
        );
        assert_eq!(rust_hits.stats.backend, Some(SEMANTIC_VECTOR_BACKEND_RUST));
        assert_eq!(sqlite_hits.hits.len(), 2);
        assert_eq!(rust_hits.hits.len(), 2);
        assert_eq!(sqlite_hits.hits[0].event_id, close_event);
        assert_eq!(rust_hits.hits[0].event_id, close_event);
        assert_eq!(sqlite_hits.hits[1].event_id, far_event);
        assert_eq!(rust_hits.hits[1].event_id, far_event);
        Ok(())
    }

    #[test]
    fn sqlite_vec0_caps_large_k_without_falling_back() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let close_event = Uuid::new_v4();
        let far_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk(close_event, 2, "close"),
                test_embedding(1.0, 0.0),
            ),
            (test_chunk(far_event, 1, "far"), test_embedding(0.0, 1.0)),
        ])?;

        let search = store.search(&test_embedding(1.0, 0.0), SEMANTIC_SQLITE_VEC0_MAX_K + 1)?;

        assert_eq!(
            search.stats.backend,
            Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC)
        );
        assert_eq!(search.hits.len(), 2);
        assert_eq!(search.hits[0].event_id, close_event);
        Ok(())
    }

    #[test]
    fn sqlite_vec0_overfetches_until_unique_events_match_rust_scan() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let multi_chunk_event = Uuid::new_v4();
        let next_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk_at(multi_chunk_event, 2, "multi", 0, 3),
                test_embedding(1.0, 0.0),
            ),
            (
                test_chunk_at(multi_chunk_event, 2, "multi", 1, 3),
                test_embedding(0.999, 0.044),
            ),
            (
                test_chunk_at(multi_chunk_event, 2, "multi", 2, 3),
                test_embedding(0.995, 0.099),
            ),
            (
                test_chunk_at(next_event, 1, "next", 0, 1),
                test_embedding(0.98, 0.199),
            ),
        ])?;

        let query = test_embedding(1.0, 0.0);
        let sqlite_hits = store.search(&query, 2)?;
        let rust_hits = store.search_event_ids(&query, &[multi_chunk_event, next_event], 2)?;

        assert_eq!(
            sqlite_hits.stats.backend,
            Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC)
        );
        assert_eq!(sqlite_hits.hits.len(), 2);
        assert_eq!(sqlite_hits.hits[0].event_id, multi_chunk_event);
        assert_eq!(sqlite_hits.hits[1].event_id, next_event);
        assert_eq!(
            sqlite_hits
                .hits
                .iter()
                .map(|hit| hit.event_id)
                .collect::<Vec<_>>(),
            rust_hits
                .hits
                .iter()
                .map(|hit| hit.event_id)
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn sqlite_vec0_rebuilds_incompatible_derived_schema() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let vector_path = temp.path().join("vectors.sqlite");
        {
            let conn = Connection::open(&vector_path)?;
            conn.execute_batch(
                r#"
                CREATE TABLE event_embedding_vec0_meta (
                    rowid INTEGER PRIMARY KEY,
                    event_id TEXT NOT NULL
                );
                CREATE TABLE event_embedding_vec0 (
                    rowid INTEGER PRIMARY KEY,
                    embedding BLOB
                );
                "#,
            )?;
        }

        let mut store = SemanticVectorStore::open(&vector_path)?;
        let close_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[(
            test_chunk(close_event, 1, "close"),
            test_embedding(1.0, 0.0),
        )])?;

        assert!(store.sqlite_vec0_ready()?);
        let vec0_sql = sqlite_table_sql(&store.conn, "event_embedding_vec0")?.unwrap_or_default();
        assert!(vec0_sql.to_ascii_lowercase().contains("using vec0"));
        assert!(sqlite_table_has_columns(
            &store.conn,
            "event_embedding_vec0_meta",
            &["model_key", "source_text_sha256", "start_char", "end_char"]
        )?);
        Ok(())
    }

    #[test]
    fn sqlite_vec0_rebuilds_when_same_count_meta_rowids_drift() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let close_event = Uuid::new_v4();
        let far_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk(close_event, 2, "close"),
                test_embedding(1.0, 0.0),
            ),
            (test_chunk(far_event, 1, "far"), test_embedding(0.0, 1.0)),
        ])?;
        assert!(store.sqlite_vec0_ready()?);

        let canonical_rowid = store.conn.query_row(
            "SELECT rowid FROM event_embedding_chunks WHERE event_id = ?1 AND model_key = ?2",
            params![close_event.to_string(), SEMANTIC_MODEL_KEY],
            |row| row.get::<_, i64>(0),
        )?;
        store.conn.execute(
	            "UPDATE event_embedding_vec0_meta SET rowid = rowid + 1000 WHERE event_id = ?1 AND model_key = ?2",
	            params![close_event.to_string(), SEMANTIC_MODEL_KEY],
	        )?;

        assert!(!store.sqlite_vec0_ready()?);
        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        assert!(store.sqlite_vec0_ready()?);

        let repaired_rowid = store.conn.query_row(
            "SELECT rowid FROM event_embedding_vec0_meta WHERE event_id = ?1 AND model_key = ?2",
            params![close_event.to_string(), SEMANTIC_MODEL_KEY],
            |row| row.get::<_, i64>(0),
        )?;
        assert_eq!(repaired_rowid, canonical_rowid);
        Ok(())
    }

    #[test]
    fn sqlite_vec0_payload_drift_falls_back_and_rebuilds() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let close_event = Uuid::new_v4();
        let far_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk(close_event, 2, "close"),
                test_embedding(1.0, 0.0),
            ),
            (test_chunk(far_event, 1, "far"), test_embedding(0.0, 1.0)),
        ])?;
        assert!(store.sqlite_vec0_ready()?);

        let close_rowid = store.conn.query_row(
            "SELECT rowid FROM event_embedding_chunks WHERE event_id = ?1 AND model_key = ?2",
            params![close_event.to_string(), SEMANTIC_MODEL_KEY],
            |row| row.get::<_, i64>(0),
        )?;
        store.conn.execute(
            "DELETE FROM event_embedding_vec0 WHERE rowid = ?1",
            params![close_rowid],
        )?;
        store.conn.execute(
            "INSERT INTO event_embedding_vec0(rowid, embedding) VALUES (?1, ?2)",
            params![close_rowid, serialize_f32_blob(&test_embedding(0.0, 1.0))],
        )?;

        assert!(!store.sqlite_vec0_ready()?);
        let search = store.search(&test_embedding(1.0, 0.0), 2)?;
        assert_eq!(search.stats.backend, Some(SEMANTIC_VECTOR_BACKEND_RUST));
        assert_eq!(search.hits[0].event_id, close_event);

        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        assert!(store.sqlite_vec0_ready()?);
        Ok(())
    }

    #[test]
    fn auto_does_not_use_partial_coverage_as_empty_lexical_veto() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let data_root = temp.path();
        let store = Store::open(data_root.join("work.sqlite"))?;
        let vector_path = data_root.join("vectors.sqlite");
        let _vector_store = SemanticVectorStore::open(&vector_path)?;
        let cache_dir = data_root.join("semantic-model-cache");
        let worker_report = SemanticWorkerReport {
            status: "pending".to_owned(),
            running: false,
            pid: None,
            started_at_ms: None,
            heartbeat_at_ms: None,
            finished_at_ms: None,
            indexed_chunks: None,
            model_init_ms: None,
            last_error: None,
            searchable_items: 10,
            embedded_items: 1,
            embedded_chunks: 1,
            dirty_items: 0,
            queued_items_estimate: 9,
            model_cache_available: true,
            vector_path: vector_path.clone(),
            lock_path: semantic_worker_lock_path(data_root),
            status_path: semantic_worker_status_path(data_root),
        };
        let options = ctx_history_search::PacketOptions::default();
        let mut retrieval = SemanticRetrievalReport::lexical(SearchBackendArg::Auto, 10);
        retrieval.worker = Some(worker_report.clone());
        retrieval.apply_worker_counts(&worker_report);
        let lexical_packet = || Ok(empty_test_packet("semantic-only needle", &options));

        let packet = auto_search_packet(
            &store,
            &options,
            &lexical_packet,
            &mut retrieval,
            &worker_report,
            &vector_path,
            &cache_dir,
            "semantic-only needle",
            0.65,
            RefreshArg::Off,
        )?;

        assert!(packet.results.is_empty());
        assert_eq!(retrieval.effective_mode(), SearchBackendArg::Lexical);
        assert_eq!(retrieval.semantic_weight, 0.0);
        assert_eq!(
            retrieval
                .diagnostics
                .as_ref()
                .and_then(|diagnostics| diagnostics.auto_hybrid_skipped),
            Some("semantic_coverage_not_ready")
        );
        Ok(())
    }

    #[test]
    fn daemon_autostart_skips_semantic_sidecar_work() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let args = DaemonRunArgs {
            foreground: false,
            once: true,
            max_runtime_seconds: None,
            idle_exit_seconds: None,
            loop_interval_seconds: None,
            max_chunks: None,
            max_seconds: None,
            force: false,
            start_mode: Some(DaemonStartModeArg::Auto),
            trigger_command: Some(DaemonTriggerCommandArg::Setup),
            json: true,
        };

        assert!(daemon_run_is_autostart(&args));
        let job = daemon_semantic_autostart_skipped_job(temp.path());
        assert_eq!(job["status"], "skipped");
        assert_eq!(job["reason"], "autostart_history_only");
        assert!(!temp.path().join("vectors.sqlite").exists());

        write_daemon_lifecycle_status(temp.path(), &args, "running", 123, None, None)?;
        let status = read_daemon_status(temp.path()).expect("daemon status");
        assert_eq!(status["start_mode"], "auto");
        assert_eq!(status["trigger_command"], "setup");
        Ok(())
    }
}
