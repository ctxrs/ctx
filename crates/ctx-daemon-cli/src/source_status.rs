use std::path::Path;

use anyhow::Result;
use ctx_history_index::{
    current_source_generation_policy, current_source_generation_policy_hash, VerifiedIndex,
};
use ctx_history_read_application::{history_health_report, HistoryHealthReport};
use ctx_semantic_index::{
    source_backed_semantic_vector_path, SemanticVectorStore, SourceBackedGenerationPin,
};
use serde_json::{json, Value};

use crate::{compact_json, composition::DaemonRuntimeConfig};

use super::paths_status::{
    daemon_core_refresh_job_path, daemon_report_with_config, daemon_semantic_job_path,
    read_daemon_job_status,
};
use super::source_backed_refresh_coordinator::verified_generation_is_query_ready;

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";
const MAX_SAFE_PUBLIC_JSON_COUNTER: u64 = (1_u64 << 53) - 1;

#[derive(Clone, Copy)]
struct PublicSemanticDocumentCounts {
    semantic_documents: u64,
    projected_documents: u64,
    filtered_documents: u64,
}

impl PublicSemanticDocumentCounts {
    fn new(semantic_documents: u64, projected_documents: u64) -> Result<Self> {
        let semantic_documents =
            validate_public_semantic_counter("semantic_documents", semantic_documents)?;
        let projected_documents =
            validate_public_semantic_counter("projected_documents", projected_documents)?;
        let filtered_documents = semantic_documents
            .checked_sub(projected_documents)
            .ok_or_else(|| anyhow::anyhow!("projected_documents exceeds semantic_documents"))?;
        let filtered_documents =
            validate_public_semantic_counter("filtered_documents", filtered_documents)?;
        Ok(Self {
            semantic_documents,
            projected_documents,
            filtered_documents,
        })
    }
}

fn validate_public_semantic_counter(field: &'static str, value: u64) -> Result<u64> {
    if value > MAX_SAFE_PUBLIC_JSON_COUNTER {
        return Err(anyhow::anyhow!(
            "{field} exceeds maximum {MAX_SAFE_PUBLIC_JSON_COUNTER}"
        ));
    }
    Ok(value)
}

pub struct SourceEpochStatus {
    pub initialized: bool,
    pub indexed_items: Option<u64>,
    pub indexed_sessions: Option<u64>,
    pub indexed_events: Option<u64>,
    pub indexed_sources: Option<u64>,
    pub health: Option<HistoryHealthReport>,
    pub report: Value,
}

pub fn source_epoch_status_report(
    data_root: &Path,
    config: &DaemonRuntimeConfig,
) -> Result<SourceEpochStatus> {
    let current_policy = current_source_generation_policy();
    let current_policy_hash = current_source_generation_policy_hash()?;
    let refresh_job = read_daemon_job_status(&daemon_core_refresh_job_path(data_root));
    let (lexical, index) = lexical_report(
        data_root,
        refresh_job.as_ref(),
        &current_policy_hash,
        serde_json::to_value(&current_policy)?,
    );
    let admitted_index = matches!(
        lexical.get("status").and_then(Value::as_str),
        Some("ready" | "stale")
    )
    .then_some(index.as_ref())
    .flatten();
    let history_epoch = history_epoch_report(&lexical, admitted_index);
    let initialized = admitted_index.is_some();
    let generation_id = index.as_ref().map(|index| index.generation_id().to_owned());
    let admitted_generation_id = admitted_index.map(|index| index.generation_id().to_owned());
    let daemon = source_daemon_report(data_root, config);
    let catalog = catalog_report(admitted_generation_id.as_deref(), admitted_index);
    let mut semantic = semantic_report(data_root, config, admitted_index);
    attach_catch_up_status(
        &mut semantic,
        read_daemon_job_status(&daemon_semantic_job_path(data_root)),
    );
    let refresh = refresh_report(refresh_job.as_ref(), generation_id.as_deref(), &daemon);
    let mut health = admitted_index.map(history_health_report).transpose()?;
    if let Some(health) = health.as_mut() {
        let (source_failures, rejected_records) = refresh_diagnostic_totals(&refresh);
        health.record_refresh_diagnostics(source_failures, rejected_records);
    }

    let indexed_items = admitted_index.map(VerifiedIndex::document_count);
    let indexed_events = indexed_items;
    let indexed_sources = admitted_index.map(|index| index.manifest().sources.len() as u64);
    let indexed_sessions = admitted_index
        .map(|index| index.session_count())
        .transpose()?;

    Ok(SourceEpochStatus {
        initialized,
        indexed_items,
        indexed_sessions,
        indexed_events,
        indexed_sources,
        health,
        report: compact_json(json!({
            "schema_version": 2,
            "initialized": initialized,
            "data_root": data_root,
            "config_path": data_root.join(ctx_app_config::CONFIG_FILE),
            "history_epoch": history_epoch,
            "lexical": lexical,
            "catalog": catalog,
            "refresh": refresh,
            "semantic": semantic,
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

fn refresh_diagnostic_totals(refresh: &Value) -> (u64, u64) {
    let diagnostics = refresh.get("diagnostics");
    (
        diagnostics
            .and_then(|value| value.get("source_failure_total"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        diagnostics
            .and_then(|value| value.get("rejected_record_total"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    )
}

fn attach_catch_up_status(report: &mut Value, status: Option<Value>) {
    if let Some(status) = status {
        report["catch_up"] = status;
    }
}

fn source_daemon_report(data_root: &Path, config: &DaemonRuntimeConfig) -> Value {
    let mut daemon = daemon_report_with_config(data_root, true, config);
    if let Some(jobs) = daemon.get_mut("jobs").and_then(Value::as_object_mut) {
        jobs.retain(|name, _| matches!(name.as_str(), "core_refresh" | "semantic_index"));
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
    let request_outcome = job.get("request_outcome").or_else(|| job.get("receipt"));
    let outcome = request_outcome
        .and_then(|receipt| receipt.get("outcome"))
        .or_else(|| job.get("outcome"))
        .and_then(Value::as_str);
    let source_failures = request_outcome
        .and_then(|receipt| receipt.get("source_failure_total"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let has_source_failures = source_failures > 0
        || matches!(
            outcome,
            Some(
                "completed_with_source_failures" | "completed_with_rejections_and_source_failures"
            )
        );
    let retryable = job
        .get("structured_outcome")
        .and_then(|outcome| outcome.get("retryable"))
        .and_then(Value::as_bool)
        .or_else(|| job.get("retryable").and_then(Value::as_bool))
        .unwrap_or(false);
    let automatic_retry_state = job
        .get("automatic_retry")
        .and_then(|automatic_retry| automatic_retry.get("state"))
        .and_then(Value::as_str);
    let (status, reason) = match automatic_retry_state {
        Some("confirming") => ("pending", Some("automatic_retry_confirming")),
        Some("paused") if current_internal_failure_is_fully_paused(job) => {
            ("paused", Some("automatic_retry_paused"))
        }
        Some("paused") => ("partial", Some("automatic_retry_partially_paused")),
        Some("mixed") => ("partial", Some("automatic_retry_partially_paused")),
        _ => match request_state {
            Some("published") if generation_matches && (has_source_failures || retryable) => (
                "partial",
                Some(outcome.unwrap_or("refresh_completed_partially")),
            ),
            Some("published") if generation_matches => ("ready", None),
            Some("admission_pending" | "queued" | "running") => {
                ("pending", Some("core_refresh_pending"))
            }
            Some("failed") => ("unavailable", Some("core_refresh_failed")),
            Some("published") => ("stale", Some("published_generation_mismatch")),
            Some(_) => ("unavailable", Some("refresh_state_unrecognized")),
            None => ("unavailable", Some("refresh_state_missing")),
        },
    };
    compact_json(json!({
        "status": status,
        "reason": reason.clone(),
        "request_state": request_state,
        "request_id": job.get("request_id"),
        "logical_request_id": job.get("logical_request_id"),
        "logical_phase": job.get("logical_phase"),
        "physical_attempt_id": job.get("physical_attempt_id"),
        "physical_attempt_state": job.get("physical_attempt_state"),
        "progress_owner_request_id": job.get("progress_owner_request_id"),
        "progress_owner_attempt_state": job.get("progress_owner_attempt_state"),
        "structured_outcome": job.get("structured_outcome"),
        "automatic_retry": job.get("automatic_retry"),
        "published_generation": published_generation,
        "generation_id": generation_id,
        "generation_matches": generation_matches,
        "outcome": outcome,
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
        "current": request_outcome.and_then(|receipt| receipt.get("current")),
        "source_failure_total": request_outcome
            .and_then(|receipt| receipt.get("source_failure_total")),
        "rejected_record_total": request_outcome
            .and_then(|receipt| receipt.get("rejected_record_total")),
        "diagnostics": refresh_diagnostics_report(request_outcome),
    }))
}

fn current_internal_failure_is_fully_paused(job: &Value) -> bool {
    job.get("request_state").and_then(Value::as_str) == Some("failed")
        && job
            .pointer("/structured_outcome/code")
            .and_then(Value::as_str)
            == Some("source_refresh_failed")
        && job
            .pointer("/structured_outcome/class")
            .and_then(Value::as_str)
            == Some("internal")
        && job
            .pointer("/structured_outcome/retryable")
            .and_then(Value::as_bool)
            == Some(false)
}

fn refresh_diagnostics_report(receipt: Option<&Value>) -> Option<Value> {
    let receipt = receipt?;
    let mut source_failures = Vec::new();
    let mut record_rejections = Vec::new();
    for (route_identity, result) in receipt
        .get("route_results")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let Some(fields) = result.as_array() else {
            continue;
        };
        let outcome = fields.first().and_then(Value::as_str);
        if matches!(outcome, Some("s" | "f")) {
            if let Some(failures) = fields.get(4).and_then(Value::as_array) {
                source_failures.extend(
                    failures.iter().filter_map(|failure| {
                        compact_source_failure_report(route_identity, failure)
                    }),
                );
            }
        }
        if outcome == Some("s") {
            if let Some(rejections) = fields.get(6).and_then(Value::as_array) {
                record_rejections.extend(rejections.iter().filter_map(|rejection| {
                    compact_record_rejection_report(route_identity, rejection)
                }));
            }
        }
    }
    Some(compact_json(json!({
        "source_failure_total": receipt.get("source_failure_total"),
        "source_failures_shown": source_failures.len(),
        "source_failures_omitted": receipt.get("source_failures_omitted"),
        "source_failures": source_failures,
        "rejected_record_total": receipt.get("rejected_record_total"),
        "rejection_diagnostics_shown": record_rejections.len(),
        "rejection_diagnostics_omitted": receipt.get("rejection_diagnostics_omitted"),
        "record_rejections": record_rejections,
    })))
}

fn compact_source_failure_report(route_identity: &str, value: &Value) -> Option<Value> {
    let fields = value.as_array().filter(|fields| fields.len() == 6)?;
    Some(json!({
        "route_identity": route_identity,
        "source_identity": fields[0].as_str()?,
        "provider": fields[1].as_str()?,
        "class": match fields[2].as_str()? {
            "u" => "unavailable",
            "c" => "source_changed",
            "r" => "unreadable",
            "i" => "incompatible",
            _ => return None,
        },
        "carried_forward": fields[3].as_bool()?,
        "source_selector": fields[4].as_str()?,
        "detail": fields[5].as_str()?,
    }))
}

fn compact_record_rejection_report(route_identity: &str, value: &Value) -> Option<Value> {
    let fields = value.as_array().filter(|fields| fields.len() == 7)?;
    Some(json!({
        "route_identity": route_identity,
        "source_identity": fields[0].as_str()?,
        "provider": fields[1].as_str()?,
        "source_selector": fields[2].as_str()?,
        "line": fields[3].as_u64()?,
        "payload_type": fields[4].as_str()?,
        "class": match fields[5].as_str()? {
            "m" => "malformed_record",
            "u" => "unsupported_record",
            _ => return None,
        },
        "detail": fields[6].as_str()?,
    }))
}

pub fn current_rejected_record_count(report: &Value) -> u64 {
    report
        .get("refresh")
        .and_then(|refresh| refresh.get("current"))
        .and_then(|current| current.get("current_rejected_records"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn history_epoch_report(lexical: &Value, index: Option<&VerifiedIndex>) -> Value {
    compact_json(json!({
        "name": "self_contained_core",
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
    match super::source_backed_refresh_coordinator::open_verified_index(&path) {
        Ok(index) => {
            let manifest = index.manifest();
            let policy_matches = manifest.policy_schema_hash == current_policy_hash;
            let generation_matches =
                published_generation.map(|generation| generation == index.generation_id());
            let readiness = verified_generation_is_query_ready(&index);
            let (status, reason, authority_error) = match readiness {
                Ok(true) => {
                    let (status, reason) = lexical_state(policy_matches);
                    (status, reason, None)
                }
                Ok(false) => (
                    "unavailable",
                    Some("zero_source_publication_uncertified"),
                    None,
                ),
                Err(error) => (
                    "unavailable",
                    Some("publication_authority_invalid"),
                    Some(format!("{error:#}")),
                ),
            };
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
                "publication_authority_error": authority_error,
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
        Err(ctx_history_index::IndexError::MissingActiveGenerationPointer) => {
            let (status, reason) = match request_state {
                Some("admission_pending" | "queued" | "running") => {
                    ("pending", "generation_not_published")
                }
                Some("failed") => ("unavailable", "core_refresh_failed"),
                Some("published") => ("unavailable", "published_generation_missing"),
                _ => ("unavailable", "generation_not_published"),
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
                })),
                None,
            )
        }
        Err(error) => (
            compact_json(json!({
                "status": "unavailable",
                "reason": "generation_verification_failed",
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
        ),
    }
}

fn lexical_state(policy_matches: bool) -> (&'static str, Option<&'static str>) {
    if !policy_matches {
        return ("stale", Some("generation_policy_mismatch"));
    }
    ("ready", None)
}

fn catalog_report(generation_id: Option<&str>, index: Option<&VerifiedIndex>) -> Value {
    compact_json(json!({
        "status": if generation_id.is_some() { "ready" } else { "pending" },
        "reason": if generation_id.is_some() { None } else { Some("core_generation_pending") },
        "authority": "automatic_provider_registry",
        "explicit_import_authority": "request_scoped_overlay",
        "persisted_explicit_roots": false,
        "generation_id": generation_id,
        "certified_sources": index.map(|index| index.manifest().sources.len()),
    }))
}

fn semantic_report(
    data_root: &Path,
    config: &DaemonRuntimeConfig,
    index: Option<&VerifiedIndex>,
) -> Value {
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

    let contract = match crate::query_adapter::semantic_index_contract_for_selected(
        config.semantic_model_contract(),
    ) {
        Ok(contract) => contract,
        Err(error) => {
            return compact_json(json!({
                "status": "unavailable",
                "reason": "semantic_contract_invalid",
                "enabled": enabled,
                "config_source": config.semantic_search_source(),
                "flat_f32": typed_unavailable_with_error(
                    "semantic_contract_invalid",
                    path,
                    error,
                ),
            }));
        }
    };
    let flat_f32 = match SemanticVectorStore::open_read_only(&path, &contract) {
        Ok(Some(store)) => match index.semantic_eligible_event_count() {
            Ok(semantic_documents) => {
                match validate_public_semantic_counter("semantic_documents", semantic_documents) {
                    Ok(semantic_documents) => match store.source_backed_generation_pin_exact(
                        index.generation_id(),
                        semantic_documents,
                    ) {
                        Ok(SourceBackedGenerationPin::Ready(pin)) => {
                            let counts = u64::try_from(pin.active_event_count())
                                .map_err(anyhow::Error::from)
                                .and_then(|projected_documents| {
                                    PublicSemanticDocumentCounts::new(
                                        semantic_documents,
                                        projected_documents,
                                    )
                                });
                            match counts {
                                Ok(counts) => compact_json(json!({
                                    "status": "ready",
                                    "reason": Value::Null,
                                    "path": path,
                                    "core_generation_id": index.generation_id(),
                                    "semantic_documents": counts.semantic_documents,
                                    "projected_documents": counts.projected_documents,
                                    "filtered_documents": counts.filtered_documents,
                                    "flat_generation": pin.generation(),
                                    "flat_generation_hash": pin.generation_hash(),
                                    "active_events": counts.projected_documents,
                                    "active_chunks": pin.active_chunk_count(),
                                    "active_vector_bytes": pin.active_vector_bytes(),
                                })),
                                Err(error) => typed_unavailable_with_error(
                                    "semantic_count_out_of_range",
                                    path,
                                    error,
                                ),
                            }
                        }
                        Ok(SourceBackedGenerationPin::ReadyEmpty) => {
                            match PublicSemanticDocumentCounts::new(semantic_documents, 0) {
                                Ok(counts) => compact_json(json!({
                                    "status": "ready",
                                    "reason": Value::Null,
                                    "path": path,
                                    "core_generation_id": index.generation_id(),
                                    "semantic_documents": counts.semantic_documents,
                                    "projected_documents": counts.projected_documents,
                                    "filtered_documents": counts.filtered_documents,
                                    "flat_generation": Value::Null,
                                    "flat_generation_hash": Value::Null,
                                    "active_events": counts.projected_documents,
                                    "active_chunks": 0,
                                    "active_vector_bytes": 0,
                                })),
                                Err(error) => typed_unavailable_with_error(
                                    "semantic_count_out_of_range",
                                    path,
                                    error,
                                ),
                            }
                        }
                        Ok(SourceBackedGenerationPin::NotReady) => compact_json(json!({
                            "status": "pending",
                            "reason": "generation_not_acknowledged",
                            "path": path,
                            "core_generation_id": index.generation_id(),
                            "semantic_documents": semantic_documents,
                        })),
                        Err(error) => {
                            typed_unavailable_with_error("flat_f32_status_failed", path, error)
                        }
                    },
                    Err(error) => {
                        typed_unavailable_with_error("semantic_count_out_of_range", path, error)
                    }
                }
            }
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
