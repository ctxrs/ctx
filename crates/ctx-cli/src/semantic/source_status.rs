use std::path::Path;

use anyhow::Result;
use ctx_history_index::{
    current_source_generation_policy, current_source_generation_policy_hash, VerifiedIndex,
};
use serde_json::{json, Value};

use crate::{
    commands::import::{load_explicit_source_catalog_authority, ExplicitSourceCatalogAuthority},
    compact_json,
    config::AppConfig,
};

use super::{
    paths_status::{
        daemon_core_refresh_job_path, daemon_jobs_path, daemon_report_with_disabled_status,
        daemon_semantic_job_path, read_daemon_job_status,
    },
    vector_store::{
        source_backed_semantic_vector_path, SemanticVectorStore, SourceBackedGenerationPin,
    },
};

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";
const PRO_CATCH_UP_STATUS_FILE: &str = "pro-catch-up.json";

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
    let refresh_job = read_daemon_job_status(&daemon_core_refresh_job_path(data_root));
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
    let mut semantic = semantic_report(data_root, config, index.as_ref());
    attach_catch_up_status(
        &mut semantic,
        read_daemon_job_status(&daemon_semantic_job_path(data_root)),
    );
    let pro_projection = pro_projection_report(data_root, generation_id.as_deref());
    let refresh = refresh_report(refresh_job.as_ref(), generation_id.as_deref(), &daemon);

    let indexed_items = index.as_ref().map(VerifiedIndex::document_count);
    let indexed_events = indexed_items;
    let indexed_sources = index
        .as_ref()
        .map(|index| index.manifest().sources.len() as u64);
    let indexed_sessions = index
        .as_ref()
        .map(|index| index.session_count())
        .transpose()?;

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
            "refresh": refresh,
            "semantic": semantic,
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

fn attach_catch_up_status(report: &mut Value, status: Option<Value>) {
    if let Some(status) = status {
        report["catch_up"] = status;
    }
}

fn source_daemon_report(data_root: &Path) -> Value {
    let mut daemon = daemon_report_with_disabled_status(data_root, true);
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
    let (status, reason) = match request_state {
        Some("published") if generation_matches => ("ready", None),
        Some("queued" | "running") => ("pending", Some("core_refresh_pending")),
        Some("failed") => ("unavailable", Some("core_refresh_failed")),
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
    match VerifiedIndex::open_pinned(&path) {
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
        Err(ctx_history_index::IndexError::MissingActiveGenerationPointer) => {
            let (status, reason) = match request_state {
                Some("queued" | "running") => ("pending", "generation_not_published"),
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
    let job_published_authority = refresh_job
        .and_then(|job| job.get("published_explicit_source_catalog"))
        .cloned();
    let receipt_published_authority = refresh_job
        .and_then(|job| job.get("receipt"))
        .and_then(|receipt| receipt.get("published_explicit_source_catalog"))
        .cloned();
    let publication_verified = job_published_authority
        .as_ref()
        .zip(receipt_published_authority.as_ref())
        .and_then(|(job, receipt)| {
            ExplicitSourceCatalogAuthority::from_json(job)
                .ok()
                .zip(ExplicitSourceCatalogAuthority::from_json(receipt).ok())
        })
        .is_some_and(|(job, receipt)| job == receipt);
    let published_authority = publication_verified
        .then_some(job_published_authority)
        .flatten();
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
    } else if !publication_verified {
        ("unavailable", Some("catalog_publication_unverified"))
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
        "published_authority_present": publication_verified,
        "published_generation": published_generation,
        "generation_id": generation_id,
        "generation_matches": ready,
        "certified_sources": index.map(|index| index.manifest().sources.len()),
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
                .source_backed_generation_pin_exact(index.generation_id(), semantic_documents)
            {
                Ok(SourceBackedGenerationPin::Ready(pin)) => {
                    let stats = pin.stats();
                    compact_json(json!({
                        "status": "ready",
                        "reason": Value::Null,
                        "path": path,
                        "core_generation_id": index.generation_id(),
                        "semantic_documents": semantic_documents,
                        "flat_generation": pin.generation(),
                        "flat_generation_hash": pin.generation_hash(),
                        "active_events": stats.active_events,
                        "active_chunks": stats.active_chunks,
                        "active_vector_bytes": stats.active_vector_bytes,
                    }))
                }
                Ok(SourceBackedGenerationPin::ReadyEmpty) => compact_json(json!({
                    "status": "ready",
                    "reason": Value::Null,
                    "path": path,
                    "core_generation_id": index.generation_id(),
                    "semantic_documents": semantic_documents,
                    "flat_generation": Value::Null,
                    "flat_generation_hash": Value::Null,
                    "active_events": 0,
                    "active_chunks": 0,
                    "active_vector_bytes": 0,
                })),
                Ok(SourceBackedGenerationPin::NotReady) => compact_json(json!({
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

fn pro_projection_report(data_root: &Path, generation_id: Option<&str>) -> Value {
    let lifecycle = crate::pro::lifecycle_status_json(data_root);
    let helper_generation_matches = lifecycle
        .get("projection_currentness")
        .and_then(Value::as_str)
        == Some("current")
        && lifecycle.get("materialized").and_then(Value::as_bool) == Some(true)
        && generation_id.is_some_and(|expected| {
            super::pin_active_verified_generation(data_root)
                .is_ok_and(|active| active.generation_id() == expected)
        });
    let path = daemon_jobs_path(data_root).join(PRO_CATCH_UP_STATUS_FILE);
    let catch_up = read_daemon_job_status(&path);
    pro_projection_report_from_status(
        generation_id,
        helper_generation_matches,
        &lifecycle,
        catch_up.as_ref(),
        path,
    )
}

fn pro_projection_report_from_status(
    generation_id: Option<&str>,
    helper_generation_matches: bool,
    lifecycle: &Value,
    catch_up: Option<&Value>,
    status_path: impl AsRef<Path>,
) -> Value {
    let installed = lifecycle.get("installed").and_then(Value::as_bool) == Some(true);
    let currentness = lifecycle
        .get("projection_currentness")
        .and_then(Value::as_str);
    let materialized = lifecycle.get("materialized").and_then(Value::as_bool) == Some(true);
    let (status, reason) = if !installed {
        ("unavailable", json!("pro_not_installed"))
    } else if generation_id.is_none() {
        ("unavailable", json!("lexical_generation_unavailable"))
    } else if currentness == Some("current") && materialized {
        if helper_generation_matches {
            ("ready", Value::Null)
        } else {
            ("stale", json!("stale_source"))
        }
    } else {
        match currentness {
            Some("stale") => ("stale", json!("stale_source")),
            Some("not_materialized" | "partial") => (
                "pending",
                lifecycle
                    .get("error_code")
                    .cloned()
                    .unwrap_or_else(|| json!("core_receipt_pending")),
            ),
            Some("needs_rebuild") => ("unavailable", json!("needs_rebuild")),
            Some("current") => ("unavailable", json!("invalid_response")),
            Some(_) | None => (
                "unavailable",
                lifecycle
                    .get("error_code")
                    .cloned()
                    .unwrap_or_else(|| json!("pro_status_unavailable")),
            ),
        }
    };
    let receipt_matches = status == "ready";
    let catch_up = catch_up.map(|job| {
        compact_json(json!({
            "status": job.get("status"),
            "pending": job.get("pending"),
            "reason": job.get("reason"),
            "error_code": job.get("error_code"),
            "core_generation_id": job.get("core_generation_id"),
            "receipt_core_generation_id": job.get("receipt_core_generation_id"),
            "attempts": job.get("attempts"),
            "retryable": job.get("retryable"),
            "consecutive_failures": job.get("consecutive_failures"),
            "retry_after_ms": job.get("retry_after_ms"),
            "retry_not_before_at_ms": job.get("retry_not_before_at_ms"),
            "last_attempt_at_ms": job.get("last_attempt_at_ms"),
            "last_attempt_duration_us": job.get("last_attempt_duration_us"),
            "last_error": job.get("last_error"),
        }))
    });
    compact_json(json!({
        "status": status,
        "reason": reason,
        "core_generation_id": generation_id,
        "authority": "pro_helper_status",
        "projection_currentness": lifecycle.get("projection_currentness"),
        "materialized_coverage": lifecycle.get("materialized_coverage"),
        "repository_coverage": lifecycle.get("repository_coverage"),
        "ready": lifecycle.get("ready"),
        "materialized": lifecycle.get("materialized"),
        "access_state": lifecycle.get("access_state"),
        "supported_operations": lifecycle.get("supported_operations"),
        "available_operations": lifecycle.get("available_operations"),
        "receipt": {
            "status": status,
            "reason": reason,
            "core_generation_id": if receipt_matches { generation_id } else { None },
            "generation_matches": receipt_matches,
        },
        "catch_up": catch_up,
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
