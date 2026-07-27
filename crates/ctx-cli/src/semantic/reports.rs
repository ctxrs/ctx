#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum SemanticReportCountMode {
    ExactOnCacheMiss,
    CachedOrStatusFile,
}

#[derive(Debug, Clone)]
pub(crate) struct SemanticWorkerReport {
    pub(super) status: String,
    pub(super) running: bool,
    pub(super) pid: Option<u32>,
    pub(super) started_at_ms: Option<i64>,
    pub(super) heartbeat_at_ms: Option<i64>,
    pub(super) finished_at_ms: Option<i64>,
    pub(super) indexed_chunks: Option<usize>,
    pub(super) model_init_ms: Option<usize>,
    pub(super) last_error: Option<String>,
    pub(super) searchable_items: usize,
    pub(super) searchable_items_known: bool,
    pub(super) embedded_items: usize,
    pub(super) embedded_chunks: usize,
    pub(super) dirty_items: usize,
    pub(super) queued_items_estimate: usize,
    pub(super) model_cache_available: bool,
    pub(super) model_acquisition: Value,
    pub(super) embed_policy: Option<Value>,
    pub(super) embedding_runtime: Option<Value>,
    pub(super) failure_class: Option<String>,
    pub(super) resource_deferral: Option<Value>,
    pub(super) vector_path: PathBuf,
    pub(super) lock_path: PathBuf,
    pub(super) status_path: PathBuf,
}

impl SemanticWorkerReport {
    pub(super) fn unavailable(data_root: &Path, error: impl ToString) -> Self {
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
            model_acquisition: semantic_model_acquisition_status_json(&semantic_worker_cache_dir(
                data_root,
            )),
            embed_policy: Some(semantic_embed_policy_status_json()),
            embedding_runtime: None,
            failure_class: None,
            resource_deferral: None,
            vector_path: semantic_vector_path(data_root),
            lock_path: semantic_worker_lock_path(data_root),
            status_path: semantic_worker_status_path(data_root),
        }
    }

    pub(super) fn coverage_ratio(&self) -> Option<f64> {
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
            "failure_class": self.failure_class,
            "resource_deferral": self.resource_deferral.clone(),
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
    pub(super) requested_mode: SearchBackendArg,
    pub(super) effective_mode: SearchBackendArg,
    pub(super) semantic_weight: f32,
    pub(super) semantic_status: &'static str,
    pub(super) semantic_fallback_code: Option<&'static str>,
    pub(super) semantic_fallback: Option<String>,
    pub(super) embedding_model: Option<String>,
    pub(super) embedded_items: usize,
    pub(super) embedded_chunks: usize,
    pub(super) searchable_items: usize,
    pub(super) indexed_now: usize,
    pub(super) vector_path: Option<PathBuf>,
    pub(super) worker: Option<SemanticWorkerReport>,
    pub(super) diagnostics: Option<SemanticRetrievalDiagnostics>,
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

    pub(super) fn apply_worker_counts(&mut self, worker: &SemanticWorkerReport) {
        self.searchable_items = worker.searchable_items;
        self.embedded_items = worker.embedded_items;
        self.embedded_chunks = worker.embedded_chunks;
    }

    pub(super) fn apply_worker_coverage(&mut self, worker: &SemanticWorkerReport) {
        self.apply_worker_counts(worker);
        self.semantic_status = semantic_status_from_worker(worker);
    }

    pub(super) fn set_semantic_fallback(&mut self, code: &'static str, message: impl Into<String>) {
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

pub(super) fn semantic_status_from_worker(worker: &SemanticWorkerReport) -> &'static str {
    if !worker.searchable_items_known || worker.searchable_items == 0 || worker.embedded_items == 0
    {
        "unavailable"
    } else if semantic_worker_coverage_ready(worker) {
        "ready"
    } else {
        "partial"
    }
}

pub(super) fn semantic_worker_coverage_ready(worker: &SemanticWorkerReport) -> bool {
    worker.searchable_items_known
        && worker.searchable_items > 0
        && worker.embedded_items >= worker.searchable_items
        && worker.dirty_items == 0
}

#[derive(Debug, Clone, Default)]
pub(super) struct SemanticRetrievalDiagnostics {
    pub(super) vector_backend: Option<&'static str>,
    pub(super) query_embed_ms: Option<u64>,
    pub(super) vector_scan_ms: Option<u64>,
    pub(super) chunks_scanned: Option<usize>,
    pub(super) vector_bytes_read: Option<usize>,
    pub(super) events_scored: Option<usize>,
    pub(super) hydration_ms: Option<u64>,
    pub(super) stale_events_dropped: Option<usize>,
    pub(super) semantic_candidates: Option<usize>,
}

impl SemanticRetrievalDiagnostics {
    pub(super) fn to_json(&self) -> Value {
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
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::{compact_json, config::AppConfig, SearchBackendArg};

use super::{
    health_search::{
        semantic_embed_policy_status_json, semantic_model_acquisition_status_json,
        semantic_model_cache_available, semantic_worker_cache_dir,
    },
    model_contract::semantic_model_key,
    paths_status::{semantic_vector_path, semantic_worker_lock_path, semantic_worker_status_path},
    query_service::semantic_query_service_supported,
};
