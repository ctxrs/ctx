use std::path::Path;

use anyhow::Result;
use ctx_history_index::{
    current_source_generation_policy, current_source_generation_policy_hash, VerifiedIndex,
};
use ctx_history_relational::RelationalProjectionStatus;
use serde_json::{json, Value};

use crate::{
    commands::import::load_explicit_source_catalog_authority,
    compact_json,
    config::AppConfig,
    source_sql::{sql_compatibility_path, SqlCompatibility},
};

use super::{
    paths_status::{
        daemon_jobs_path, daemon_report_with_disabled_status,
        daemon_source_backed_refresh_job_path, read_daemon_job_status,
    },
    source_backed_refresh_coordinator::source_backed_lexical_artifact_is_uncommitted_schema_only,
    vector_store::{source_backed_semantic_vector_path, SemanticVectorStore},
};

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";
const SOURCE_BACKED_PRO_CATCH_UP_STATUS_FILE: &str = "pro-catch-up.json";

pub(crate) struct SourceEpochStatus {
    pub(crate) initialized: bool,
    pub(crate) indexed_items: Option<u64>,
    pub(crate) indexed_sessions: Option<u64>,
    pub(crate) indexed_events: Option<u64>,
    pub(crate) indexed_sources: Option<u64>,
    pub(crate) report: Value,
}

pub(crate) fn source_epoch_status_report(
    data_root: &Path,
    config: &AppConfig,
) -> Result<SourceEpochStatus> {
    let current_policy = current_source_generation_policy();
    let current_policy_hash = current_source_generation_policy_hash()?;
    let refresh_job = read_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root));
    let (lexical, index) = lexical_report(
        data_root,
        refresh_job.as_ref(),
        &current_policy_hash,
        serde_json::to_value(&current_policy)?,
    );
    let history_epoch = history_epoch_report(&lexical, index.as_ref());
    let initialized = index.is_some();
    let generation_id = index.as_ref().map(|index| index.generation_id().to_owned());
    let daemon = source_daemon_report(data_root);
    let catalog = catalog_report(
        data_root,
        generation_id.as_deref(),
        refresh_job.as_ref(),
        index.as_ref(),
    );
    let resolver = resolver_report(generation_id.as_deref(), refresh_job.as_ref(), &daemon);
    let semantic = semantic_report(data_root, config, index.as_ref());
    let (relational, relational_counts) =
        relational_report(data_root, index.as_ref(), &current_policy_hash);
    let pro_projection = pro_projection_report(data_root, generation_id.as_deref());
    let refresh = refresh_report(refresh_job.as_ref(), generation_id.as_deref(), &daemon);

    let indexed_items = index.as_ref().map(VerifiedIndex::document_count);
    let indexed_events = indexed_items;
    let indexed_sources = index
        .as_ref()
        .map(|index| index.manifest().sources.len() as u64);
    let indexed_sessions = relational_counts.map(|counts| counts.sessions);

    Ok(SourceEpochStatus {
        initialized,
        indexed_items,
        indexed_sessions,
        indexed_events,
        indexed_sources,
        report: compact_json(json!({
            "schema_version": 2,
            "initialized": initialized,
            "data_root": data_root,
            "config_path": data_root.join(crate::config::CONFIG_FILE),
            "history_epoch": history_epoch,
            "lexical": lexical,
            "catalog": catalog,
            "resolver": resolver,
            "refresh": refresh,
            "semantic": semantic,
            "relational": relational,
            "pro_projection": pro_projection,
            "daemon": daemon,
            "indexed_items": indexed_items,
            "indexed_sessions": indexed_sessions,
            "indexed_events": indexed_events,
            "indexed_sources": indexed_sources,
            "local_only": true,
            "read_only": true,
        })),
    })
}

fn source_daemon_report(data_root: &Path) -> Value {
    let mut daemon = daemon_report_with_disabled_status(data_root, true);
    if let Some(jobs) = daemon.get_mut("jobs").and_then(Value::as_object_mut) {
        jobs.retain(|name, _| name == "source_backed_refresh");
    }
    daemon
}

fn refresh_report(job: Option<&Value>, generation_id: Option<&str>, daemon: &Value) -> Value {
    let Some(job) = job else {
        return compact_json(json!({
            "status": "unavailable",
            "reason": if daemon.get("running").and_then(Value::as_bool) == Some(true) {
                "refresh_not_observed"
            } else {
                "daemon_unavailable"
            },
        }));
    };
    let request_state = job.get("request_state").and_then(Value::as_str);
    let published_generation = job.get("published_generation").and_then(Value::as_str);
    let generation_matches = generation_id.is_some() && generation_id == published_generation;
    let (status, reason) = match request_state {
        Some("published") if generation_matches => ("ready", None),
        Some("queued" | "running") => ("pending", Some("source_refresh_pending")),
        Some("failed") => ("unavailable", Some("source_refresh_failed")),
        Some("published") => ("stale", Some("published_generation_mismatch")),
        Some(_) => ("unavailable", Some("refresh_state_unrecognized")),
        None => ("unavailable", Some("refresh_state_missing")),
    };
    compact_json(json!({
        "status": status,
        "reason": reason.clone(),
        "request_state": request_state,
        "request_id": job.get("request_id"),
        "published_generation": published_generation,
        "generation_id": generation_id,
        "generation_matches": generation_matches,
        "source_count": job.get("source_count"),
        "certified_source_count": job.get("certified_source_count"),
        "certified_source_bytes": job.get("certified_source_bytes"),
        "scanned_routes": job.get("scanned_routes"),
        "unsupported_routes": job.get("unsupported_routes"),
        "timings_us": job.get("timings_us"),
        "progress": job.get("progress"),
        "trigger": job.get("trigger"),
        "trigger_provenance": job.get("trigger_provenance"),
        "last_error": job.get("last_error"),
    }))
}

fn history_epoch_report(lexical: &Value, index: Option<&VerifiedIndex>) -> Value {
    compact_json(json!({
        "name": "v1_source_backed",
        "status": lexical.get("status"),
        "reason": lexical.get("reason"),
        "activation_authority": "verified_lexical_generation",
        "lexical_generation_id": index.map(VerifiedIndex::generation_id),
        "generation_path": lexical.get("path"),
    }))
}

fn lexical_report(
    data_root: &Path,
    refresh_job: Option<&Value>,
    current_policy_hash: &str,
    current_policy: Value,
) -> (Value, Option<VerifiedIndex>) {
    let path = data_root.join(SEARCH_DIRECTORY).join(LEXICAL_DIRECTORY);
    let request_state = refresh_job
        .and_then(|job| job.get("request_state"))
        .and_then(Value::as_str);
    let published_generation = refresh_job
        .and_then(|job| job.get("published_generation"))
        .and_then(Value::as_str);
    if !path.join("meta.json").is_file() {
        let (status, reason) = match request_state {
            Some("queued" | "running") => ("pending", "generation_not_published"),
            Some("failed") => ("unavailable", "source_refresh_failed"),
            Some("published") => ("unavailable", "published_generation_missing"),
            _ => ("unavailable", "generation_not_published"),
        };
        return (
            compact_json(json!({
                "status": status,
                "reason": reason,
                "path": path,
                "generation_id": Value::Null,
                "request_state": request_state,
                "published_generation": published_generation,
                "generation_matches": Value::Null,
                "policy": {
                    "current_hash": current_policy_hash,
                    "current": current_policy,
                    "published_hash": Value::Null,
                    "matches_current": Value::Null,
                },
            })),
            None,
        );
    }

    match VerifiedIndex::open(&path) {
        Ok(index) => {
            let manifest = index.manifest();
            let policy_matches = manifest.policy_schema_hash == current_policy_hash;
            let generation_matches =
                published_generation.map(|generation| generation == index.generation_id());
            let (status, reason) = lexical_state(policy_matches);
            let value = compact_json(json!({
                "status": status,
                "reason": reason,
                "path": path,
                "generation_id": index.generation_id(),
                "request_state": request_state,
                "published_generation": published_generation,
                "generation_matches": generation_matches,
                "indexed_documents": index.document_count(),
                "certified_sources": manifest.sources.len(),
                "certified_source_bytes": manifest.certified_source_bytes,
                "manifest_version": manifest.manifest_version,
                "identity_version": manifest.identity_version,
                "lexical_schema_version": manifest.lexical_schema_version,
                "lexical_analyzer_version": manifest.lexical_analyzer_version,
                "policy": {
                    "current_hash": current_policy_hash,
                    "current": current_policy,
                    "published_hash": manifest.policy_schema_hash,
                    "matches_current": policy_matches,
                },
            }));
            (value, Some(index))
        }
        Err(error) => {
            let cold_uncommitted =
                if matches!(error, ctx_history_index::IndexError::MissingCommitPayload)
                    && !matches!(request_state, Some("published"))
                {
                    source_backed_lexical_artifact_is_uncommitted_schema_only(&path)
                        .unwrap_or(false)
                } else {
                    false
                };
            let (status, reason) =
                if cold_uncommitted && matches!(request_state, Some("queued" | "running")) {
                    ("pending", "generation_not_published")
                } else if cold_uncommitted {
                    ("unavailable", "generation_not_published")
                } else {
                    ("unavailable", "generation_verification_failed")
                };
            (
                compact_json(json!({
                    "status": status,
                    "reason": reason,
                    "path": path,
                    "generation_id": Value::Null,
                    "request_state": request_state,
                    "published_generation": published_generation,
                    "generation_matches": Value::Null,
                    "policy": {
                        "current_hash": current_policy_hash,
                        "current": current_policy,
                        "published_hash": Value::Null,
                        "matches_current": Value::Null,
                    },
                    "last_error": format!("{error:#}"),
                })),
                None,
            )
        }
    }
}

fn lexical_state(policy_matches: bool) -> (&'static str, Option<&'static str>) {
    if !policy_matches {
        return ("stale", Some("generation_policy_mismatch"));
    }
    ("ready", None)
}

fn catalog_report(
    data_root: &Path,
    generation_id: Option<&str>,
    refresh_job: Option<&Value>,
    index: Option<&VerifiedIndex>,
) -> Value {
    let authority = match load_explicit_source_catalog_authority(data_root) {
        Ok(authority) => authority.to_json(),
        Err(error) => {
            return compact_json(json!({
                "status": "unavailable",
                "reason": "catalog_invalid",
                "last_error": format!("{error:#}"),
            }))
        }
    };
    let published_authority = refresh_job
        .and_then(|job| job.get("published_explicit_source_catalog"))
        .cloned();
    let published_generation = refresh_job
        .and_then(|job| job.get("published_generation"))
        .and_then(Value::as_str);
    let active_request = refresh_job
        .and_then(|job| job.get("request_state"))
        .and_then(Value::as_str)
        .is_some_and(|state| matches!(state, "queued" | "running"));
    let ready = generation_id.is_some()
        && published_generation == generation_id
        && published_authority.as_ref() == Some(&authority);
    let generation_mismatch = generation_id.is_some()
        && published_generation.is_some()
        && published_generation != generation_id;
    let authority_mismatch =
        published_authority.is_some() && published_authority.as_ref() != Some(&authority);
    let (status, reason) = if ready {
        ("ready", None)
    } else if active_request || generation_id.is_none() {
        ("pending", Some("catalog_publication_pending"))
    } else if generation_mismatch {
        ("stale", Some("catalog_generation_mismatch"))
    } else if authority_mismatch {
        ("stale", Some("catalog_authority_mismatch"))
    } else {
        ("unavailable", Some("catalog_publication_unverified"))
    };
    compact_json(json!({
        "status": status,
        "reason": reason,
        "authority": authority,
        "published_authority": published_authority,
        "published_generation": published_generation,
        "generation_id": generation_id,
        "generation_matches": ready,
        "certified_sources": index.map(|index| index.manifest().sources.len()),
    }))
}

fn resolver_report(
    generation_id: Option<&str>,
    refresh_job: Option<&Value>,
    daemon: &Value,
) -> Value {
    let daemon_running = daemon
        .get("running")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let endpoint_available = daemon
        .get("source_refresh_endpoint")
        .and_then(|endpoint| endpoint.get("available"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let request_state = refresh_job
        .and_then(|job| job.get("request_state"))
        .and_then(Value::as_str);
    let published_generation = refresh_job
        .and_then(|job| job.get("published_generation"))
        .and_then(Value::as_str);
    let generation_matches = generation_id.is_some() && published_generation == generation_id;
    if matches!(request_state, Some("queued" | "running")) {
        return compact_json(json!({
            "status": "pending",
            "reason": "source_refresh_pending",
            "generation_id": generation_id,
            "daemon_running": daemon_running,
            "endpoint_available": endpoint_available,
            "published_generation": published_generation,
            "generation_matches": generation_matches,
        }));
    }
    let ready = request_state == Some("published")
        && generation_matches
        && daemon_running
        && endpoint_available;
    let stale = request_state == Some("published") && !generation_matches;
    let reason = if ready {
        None
    } else if stale {
        Some("resolver_generation_mismatch")
    } else if !daemon_running || !endpoint_available {
        Some("daemon_unavailable")
    } else {
        Some("resolver_publication_unverified")
    };
    compact_json(json!({
        "status": if ready {
            "ready"
        } else if stale {
            "stale"
        } else {
            "unavailable"
        },
        "reason": reason,
        "generation_id": generation_id,
        "published_generation": published_generation,
        "generation_matches": generation_matches,
        "daemon_running": daemon_running,
        "endpoint_available": endpoint_available,
    }))
}

fn semantic_report(data_root: &Path, config: &AppConfig, index: Option<&VerifiedIndex>) -> Value {
    let enabled = config.semantic_search_enabled();
    let path = source_backed_semantic_vector_path(data_root);
    let Some(index) = index else {
        return compact_json(json!({
            "status": if enabled { "pending" } else { "disabled" },
            "reason": if enabled {
                "lexical_generation_unavailable"
            } else {
                "semantic_disabled"
            },
            "enabled": enabled,
            "config_source": config.semantic_search_source(),
            "flat_f32": {
                "status": "unavailable",
                "reason": "lexical_generation_unavailable",
                "path": path,
            },
        }));
    };
    if !path.exists() {
        return compact_json(json!({
            "status": if enabled { "pending" } else { "disabled" },
            "reason": if enabled {
                "flat_f32_projection_missing"
            } else {
                "semantic_disabled"
            },
            "enabled": enabled,
            "config_source": config.semantic_search_source(),
            "flat_f32": {
                "status": if enabled { "pending" } else { "disabled" },
                "reason": if enabled {
                    "projection_missing"
                } else {
                    "semantic_disabled"
                },
                "path": path,
                "core_generation_id": index.generation_id(),
            },
        }));
    }

    let flat_f32 = match SemanticVectorStore::open_read_only(&path) {
        Ok(Some(store)) => match index.semantic_eligible_event_count() {
            Ok(semantic_documents) => match store
                .source_backed_generation_ready_exact(index.generation_id(), semantic_documents)
            {
                Ok(true) => match store
                    .pin_source_backed_generation(index.generation_id(), semantic_documents)
                {
                    Ok(pin) => {
                        let stats = pin.as_ref().map(|pin| pin.stats());
                        compact_json(json!({
                            "status": "ready",
                            "reason": Value::Null,
                            "path": path,
                            "core_generation_id": index.generation_id(),
                            "semantic_documents": semantic_documents,
                            "flat_generation": pin.as_ref().map(|pin| pin.generation()),
                            "flat_generation_hash": pin
                                .as_ref()
                                .map(|pin| pin.generation_hash()),
                            "active_events": stats.map(|stats| stats.active_events),
                            "active_chunks": stats.map(|stats| stats.active_chunks),
                            "active_vector_bytes": stats.map(|stats| stats.active_vector_bytes),
                        }))
                    }
                    Err(error) => typed_unavailable_with_error("flat_f32_pin_failed", path, error),
                },
                Ok(false) => compact_json(json!({
                    "status": "pending",
                    "reason": "generation_not_acknowledged",
                    "path": path,
                    "core_generation_id": index.generation_id(),
                    "semantic_documents": semantic_documents,
                })),
                Err(error) => typed_unavailable_with_error("flat_f32_status_failed", path, error),
            },
            Err(error) => typed_unavailable_with_error("semantic_count_failed", path, error.into()),
        },
        Ok(None) => compact_json(json!({
            "status": "pending",
            "reason": "projection_control_missing",
            "path": path,
            "core_generation_id": index.generation_id(),
        })),
        Err(error) => typed_unavailable_with_error("flat_f32_open_failed", path, error),
    };
    let projection_status = flat_f32
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unavailable");
    compact_json(json!({
        "status": if !enabled {
            "disabled"
        } else {
            projection_status
        },
        "reason": if enabled {
            flat_f32.get("reason").cloned().unwrap_or(Value::Null)
        } else {
            json!("semantic_disabled")
        },
        "enabled": enabled,
        "config_source": config.semantic_search_source(),
        "flat_f32": flat_f32,
    }))
}

#[derive(Clone, Copy)]
struct RelationalCounts {
    sessions: u64,
}

fn relational_report(
    data_root: &Path,
    index: Option<&VerifiedIndex>,
    current_policy_hash: &str,
) -> (Value, Option<RelationalCounts>) {
    let path = sql_compatibility_path(data_root);
    if !path.is_file() {
        return (
            compact_json(json!({
                "status": if index.is_some() { "pending" } else { "unavailable" },
                "reason": if index.is_some() {
                    "projection_missing"
                } else {
                    "lexical_generation_unavailable"
                },
                "path": path,
            })),
            None,
        );
    }
    let projection = match SqlCompatibility::open(&path) {
        Ok(projection) => projection,
        Err(error) => {
            return (
                compact_json(json!({
                    "status": "unavailable",
                    "reason": "projection_open_failed",
                    "path": path,
                    "last_error": error.to_string(),
                })),
                None,
            )
        }
    };
    let metadata = match projection.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            return (
                compact_json(json!({
                    "status": "unavailable",
                    "reason": "projection_metadata_failed",
                    "path": path,
                    "last_error": error.to_string(),
                })),
                None,
            )
        }
    };
    let projection_state = match metadata.status {
        RelationalProjectionStatus::Empty => "empty",
        RelationalProjectionStatus::Ready => "ready",
        RelationalProjectionStatus::Behind => "behind",
    };
    let generation_matches = index
        .map(|index| metadata.active_core_generation_id.as_deref() == Some(index.generation_id()));
    let policy_matches =
        Some(metadata.active_policy_schema_hash.as_deref() == Some(current_policy_hash));
    let exact_generation = generation_matches == Some(true) && policy_matches == Some(true);
    let ready = metadata.status == RelationalProjectionStatus::Ready && exact_generation;
    let stale = index.is_some()
        && (metadata.active_core_generation_id.is_some() && generation_matches == Some(false)
            || metadata.active_policy_schema_hash.is_some() && policy_matches == Some(false));
    let value = compact_json(json!({
        "status": if ready {
            "ready"
        } else if index.is_none() {
            "unavailable"
        } else if stale {
            "stale"
        } else {
            "pending"
        },
        "reason": if ready {
            Value::Null
        } else if index.is_none() {
            json!("lexical_generation_unavailable")
        } else if generation_matches == Some(false)
            && metadata.active_core_generation_id.is_some()
        {
            json!("generation_mismatch")
        } else if policy_matches == Some(false)
            && metadata.active_policy_schema_hash.is_some()
        {
            json!("policy_mismatch")
        } else if metadata.status == RelationalProjectionStatus::Behind {
            json!("projection_behind")
        } else {
            json!("projection_empty")
        },
        "path": path,
        "projection_status": projection_state,
        "build_generation": metadata.build_generation,
        "active_core_generation_id": metadata.active_core_generation_id,
        "target_core_generation_id": metadata.target_core_generation_id,
        "generation_matches": generation_matches,
        "active_manifest_version": metadata.active_manifest_version,
        "active_lexical_schema_version": metadata.active_lexical_schema_version,
        "active_policy_schema_hash": metadata.active_policy_schema_hash,
        "policy_matches": policy_matches,
        "source_count": metadata.source_count,
        "session_count": metadata.session_count,
        "event_count": metadata.event_count,
        "file_touch_count": metadata.file_touch_count,
        "last_error": metadata.last_error,
        "read_only": true,
    }));
    let counts = ready.then_some(RelationalCounts {
        sessions: metadata.session_count,
    });
    (value, counts)
}

fn pro_projection_report(data_root: &Path, generation_id: Option<&str>) -> Value {
    let lifecycle = crate::pro::lifecycle_status_json(data_root);
    if lifecycle.get("installed").and_then(Value::as_bool) != Some(true) {
        return compact_json(json!({
            "status": "unavailable",
            "reason": "pro_not_installed",
            "core_generation_id": generation_id,
            "authority": "source_manifest",
            "receipt": {
                "status": "unavailable",
                "reason": "pro_not_installed",
                "core_generation_id": Value::Null,
                "generation_matches": Value::Null,
            },
        }));
    }
    let path = daemon_jobs_path(data_root).join(SOURCE_BACKED_PRO_CATCH_UP_STATUS_FILE);
    let Some(job) = read_daemon_job_status(&path) else {
        return compact_json(json!({
            "status": if generation_id.is_some() { "pending" } else { "unavailable" },
            "reason": if generation_id.is_some() {
                "source_manifest_receipt_not_observed"
            } else {
                "lexical_generation_unavailable"
            },
            "core_generation_id": generation_id,
            "authority": "source_manifest",
            "receipt": {
                "status": if generation_id.is_some() { "pending" } else { "unavailable" },
                "reason": if generation_id.is_some() {
                    "receipt_not_observed"
                } else {
                    "lexical_generation_unavailable"
                },
                "core_generation_id": Value::Null,
                "generation_matches": Value::Null,
            },
            "status_path": path,
        }));
    };
    pro_projection_report_from_job(generation_id, &job, path)
}

fn pro_projection_report_from_job(
    generation_id: Option<&str>,
    job: &Value,
    status_path: impl AsRef<Path>,
) -> Value {
    let job_status = job.get("status").and_then(Value::as_str);
    let job_generation = job.get("core_generation_id").and_then(Value::as_str);
    let receipt_generation = job
        .get("receipt_core_generation_id")
        .and_then(Value::as_str);
    let job_matches = generation_id.is_some() && job_generation == generation_id;
    let receipt_matches = generation_id.is_some() && receipt_generation == generation_id;
    let ready = job_status == Some("completed") && job_matches && receipt_matches;
    let stale = generation_id.is_some()
        && (job_generation.is_some() && !job_matches
            || receipt_generation.is_some() && !receipt_matches);
    let unavailable = generation_id.is_none() || job_status == Some("error");
    let (status, reason) = if ready {
        ("ready", Value::Null)
    } else if stale {
        (
            "stale",
            json!("source_manifest_receipt_generation_mismatch"),
        )
    } else if generation_id.is_none() {
        ("unavailable", json!("lexical_generation_unavailable"))
    } else if unavailable {
        (
            "unavailable",
            job.get("error_code")
                .cloned()
                .unwrap_or_else(|| json!("source_manifest_projection_failed")),
        )
    } else {
        ("pending", json!("source_manifest_receipt_pending"))
    };
    compact_json(json!({
        "status": status,
        "reason": reason,
        "core_generation_id": generation_id,
        "authority": "source_manifest",
        "receipt": {
            "status": status,
            "reason": reason,
            "core_generation_id": receipt_generation,
            "generation_matches": receipt_matches,
        },
        "job_core_generation_id": job_generation,
        "job_generation_matches": job_matches,
        "attempts": job.get("attempts"),
        "retryable": job.get("retryable"),
        "consecutive_failures": job.get("consecutive_failures"),
        "retry_after_ms": job.get("retry_after_ms"),
        "retry_not_before_at_ms": job.get("retry_not_before_at_ms"),
        "last_attempt_at_ms": job.get("last_attempt_at_ms"),
        "last_error": job.get("last_error"),
        "job": job,
        "status_path": status_path.as_ref(),
    }))
}

#[cfg(test)]
#[path = "source_status_tests.rs"]
mod tests;

fn typed_unavailable_with_error(
    reason: &'static str,
    path: impl AsRef<Path>,
    error: anyhow::Error,
) -> Value {
    compact_json(json!({
        "status": "unavailable",
        "reason": reason,
        "path": path.as_ref(),
        "last_error": format!("{error:#}"),
    }))
}
