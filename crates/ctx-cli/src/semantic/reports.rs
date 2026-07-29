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
            vector_path: source_backed_semantic_vector_path(data_root),
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

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::{compact_json, config::AppConfig};

use super::{
    health_search::{
        semantic_embed_policy_status_json, semantic_model_acquisition_status_json,
        semantic_model_cache_available, semantic_worker_cache_dir,
    },
    model_contract::semantic_model_key,
    paths_status::{semantic_worker_lock_path, semantic_worker_status_path},
    query_service::semantic_query_service_supported,
    vector_store::source_backed_semantic_vector_path,
};
