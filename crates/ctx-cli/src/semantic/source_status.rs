use std::{fs, path::Path};

use anyhow::Result;
use ctx_history_core::database_path;
use ctx_history_index::{
    current_source_generation_policy, current_source_generation_policy_hash, VerifiedIndex,
};
use ctx_history_search::{sql_compatibility_path, SqlCompatibility};
use ctx_history_store::RelationalProjectionStatus;
use serde_json::{json, Value};

use crate::{
    commands::import::load_explicit_source_catalog_authority,
    compact_json,
    config::AppConfig,
    upgrade::data_migration::{self, MigrationOrigin, MigrationPhase},
};

use super::{
    paths_status::{
        daemon_jobs_path, daemon_report_with_disabled_status,
        daemon_source_backed_refresh_job_path, read_daemon_job_status,
    },
    reports::SemanticWorkerReport,
    vector_store::{source_backed_semantic_vector_path, SemanticVectorStore},
};

const EPOCH_ACTIVATION_JOURNAL: &str = "activation.jsonl";
const SOURCE_BACKED_PRO_CATCH_UP_STATUS_FILE: &str = "source-backed-pro-catch-up.json";

pub(crate) struct SourceEpochStatus {
    pub(crate) initialized: bool,
    pub(crate) indexed_items: Option<u64>,
    pub(crate) indexed_sessions: Option<u64>,
    pub(crate) indexed_events: Option<u64>,
    pub(crate) indexed_sources: Option<u64>,
    pub(crate) report: Value,
}

struct EpochObservation {
    initialized: bool,
    valid: bool,
    phase: Option<MigrationPhase>,
    report: Value,
}

pub(crate) fn source_epoch_status_report(
    data_root: &Path,
    config: &AppConfig,
) -> Result<SourceEpochStatus> {
    let current_policy = current_source_generation_policy();
    let current_policy_hash = current_source_generation_policy_hash()?;
    let legacy_history = legacy_history_report(data_root);
    let epoch = epoch_report(data_root);
    let (lexical, index) = lexical_report(
        data_root,
        &epoch,
        &current_policy_hash,
        serde_json::to_value(&current_policy)?,
    );
    let generation_id = index.as_ref().map(|index| index.generation_id().to_owned());
    let refresh_job = read_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root));
    let placeholder =
        SemanticWorkerReport::unavailable(data_root, "legacy semantic status is inactive");
    let daemon = source_daemon_report(data_root, &placeholder);
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
        initialized: epoch.initialized,
        indexed_items,
        indexed_sessions,
        indexed_events,
        indexed_sources,
        report: compact_json(json!({
            "schema_version": 2,
            "initialized": epoch.initialized,
            "data_root": data_root,
            "config_path": data_root.join(crate::config::CONFIG_FILE),
            "history_epoch": epoch.report,
            "lexical": lexical,
            "catalog": catalog,
            "resolver": resolver,
            "refresh": refresh,
            "semantic": semantic,
            "relational": relational,
            "pro_projection": pro_projection,
            "legacy_history": legacy_history,
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

fn source_daemon_report(data_root: &Path, placeholder: &SemanticWorkerReport) -> Value {
    let mut daemon = daemon_report_with_disabled_status(data_root, placeholder, true);
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
        Some("published") => ("unavailable", Some("published_generation_mismatch")),
        Some(_) => ("unavailable", Some("refresh_state_unrecognized")),
        None => ("unavailable", Some("refresh_state_missing")),
    };
    compact_json(json!({
        "status": status,
        "reason": reason,
        "request_state": request_state,
        "request_id": job.get("request_id"),
        "published_generation": published_generation,
        "generation_id": generation_id,
        "generation_matches": generation_matches,
        "source_count": job.get("source_count"),
        "progress": job.get("progress"),
        "last_error": job.get("last_error"),
    }))
}

fn epoch_report(data_root: &Path) -> EpochObservation {
    let journal_path =
        data_migration::migration_directory(data_root).join(EPOCH_ACTIVATION_JOURNAL);
    if !journal_path.is_file() {
        return EpochObservation {
            initialized: false,
            valid: true,
            phase: None,
            report: compact_json(json!({
                "name": "v0.26_source_backed",
                "status": "unavailable",
                "reason": "epoch_not_initialized",
                "activation_path": journal_path,
            })),
        };
    }

    match data_migration::inspect(data_root) {
        Ok(Some(marker)) => {
            let status = match marker.phase {
                MigrationPhase::Ready => "ready",
                MigrationPhase::Detected | MigrationPhase::RebuildPending => "pending",
                MigrationPhase::SourceRebuildFailed => "unavailable",
            };
            let reason = match marker.phase {
                MigrationPhase::Ready => None,
                MigrationPhase::Detected => Some("source_rebuild_not_requested"),
                MigrationPhase::RebuildPending => Some("source_rebuild_pending"),
                MigrationPhase::SourceRebuildFailed => Some("source_rebuild_failed"),
            };
            EpochObservation {
                initialized: true,
                valid: true,
                phase: Some(marker.phase),
                report: compact_json(json!({
                    "name": "v0.26_source_backed",
                    "status": status,
                    "reason": reason,
                    "origin": match marker.origin {
                        MigrationOrigin::Fresh => "fresh",
                        MigrationOrigin::PreviousHistoryStore => "previous_history_store",
                    },
                    "phase": migration_phase_name(marker.phase),
                    "migration_id": marker.migration_id,
                    "source_rebuild_required": marker.source_rebuild_required,
                    "lexical_generation_id": marker.lexical_generation_id,
                    "activation_path": journal_path,
                    "last_error": marker.error,
                })),
            }
        }
        Ok(None) => EpochObservation {
            initialized: true,
            valid: false,
            phase: None,
            report: compact_json(json!({
                "name": "v0.26_source_backed",
                "status": "unavailable",
                "reason": "epoch_marker_missing",
                "activation_path": journal_path,
            })),
        },
        Err(error) => EpochObservation {
            initialized: true,
            valid: false,
            phase: None,
            report: compact_json(json!({
                "name": "v0.26_source_backed",
                "status": "unavailable",
                "reason": "epoch_marker_invalid",
                "activation_path": journal_path,
                "last_error": format!("{error:#}"),
            })),
        },
    }
}

fn lexical_report(
    data_root: &Path,
    epoch: &EpochObservation,
    current_policy_hash: &str,
    current_policy: Value,
) -> (Value, Option<VerifiedIndex>) {
    let path = data_migration::lexical_projection_path(data_root);
    if !path.join("meta.json").is_file() {
        let (status, reason) = match epoch.phase {
            Some(MigrationPhase::SourceRebuildFailed) => ("unavailable", "source_rebuild_failed"),
            Some(MigrationPhase::Detected | MigrationPhase::RebuildPending) => {
                ("pending", "generation_not_published")
            }
            Some(MigrationPhase::Ready) => ("unavailable", "ready_generation_missing"),
            None if epoch.initialized => ("unavailable", "epoch_marker_invalid"),
            None => ("unavailable", "epoch_not_initialized"),
        };
        return (
            compact_json(json!({
                "status": status,
                "reason": reason,
                "path": path,
                "generation_id": Value::Null,
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
            let (status, reason) = if !epoch.valid {
                ("unavailable", Some("epoch_marker_invalid"))
            } else if !policy_matches {
                ("unavailable", Some("generation_policy_mismatch"))
            } else {
                match epoch.phase {
                    Some(MigrationPhase::Ready) => ("ready", None),
                    Some(MigrationPhase::Detected | MigrationPhase::RebuildPending) => {
                        ("pending", Some("epoch_activation_pending"))
                    }
                    Some(MigrationPhase::SourceRebuildFailed) => {
                        ("unavailable", Some("source_rebuild_failed"))
                    }
                    None => ("unavailable", Some("epoch_not_initialized")),
                }
            };
            let value = compact_json(json!({
                "status": status,
                "reason": reason,
                "path": path,
                "generation_id": index.generation_id(),
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
        Err(error) => (
            compact_json(json!({
                "status": "unavailable",
                "reason": "generation_verification_failed",
                "path": path,
                "generation_id": Value::Null,
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
    let (status, reason) = if ready {
        ("ready", None)
    } else if active_request || generation_id.is_none() {
        ("pending", Some("catalog_publication_pending"))
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
    if matches!(request_state, Some("queued" | "running")) {
        return compact_json(json!({
            "status": "pending",
            "reason": "source_refresh_pending",
            "generation_id": generation_id,
            "daemon_running": daemon_running,
            "endpoint_available": endpoint_available,
        }));
    }
    let reason = if !daemon_running || !endpoint_available {
        "daemon_unavailable"
    } else {
        "runtime_resolver_status_not_exposed"
    };
    compact_json(json!({
        "status": "unavailable",
        "reason": reason,
        "generation_id": generation_id,
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
    let exact_generation = index.is_some_and(|index| {
        metadata.active_core_generation_id.as_deref() == Some(index.generation_id())
            && metadata.active_policy_schema_hash.as_deref() == Some(current_policy_hash)
    });
    let ready = metadata.status == RelationalProjectionStatus::Ready && exact_generation;
    let value = compact_json(json!({
        "status": if ready { "ready" } else { "pending" },
        "reason": if ready {
            Value::Null
        } else if metadata.status == RelationalProjectionStatus::Behind {
            json!("projection_behind")
        } else if !exact_generation {
            json!("generation_mismatch")
        } else {
            json!("projection_empty")
        },
        "path": path,
        "projection_status": projection_state,
        "build_generation": metadata.build_generation,
        "active_core_generation_id": metadata.active_core_generation_id,
        "target_core_generation_id": metadata.target_core_generation_id,
        "active_manifest_version": metadata.active_manifest_version,
        "active_lexical_schema_version": metadata.active_lexical_schema_version,
        "active_policy_schema_hash": metadata.active_policy_schema_hash,
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
        }));
    }
    let path = daemon_jobs_path(data_root).join(SOURCE_BACKED_PRO_CATCH_UP_STATUS_FILE);
    let Some(job) = read_daemon_job_status(&path) else {
        return compact_json(json!({
            "status": "pending",
            "reason": "projection_not_observed",
            "core_generation_id": generation_id,
            "status_path": path,
        }));
    };
    let job_status = job.get("status").and_then(Value::as_str);
    let job_generation = job.get("core_generation_id").and_then(Value::as_str);
    let receipt_generation = job
        .get("receipt_core_generation_id")
        .and_then(Value::as_str);
    let ready = generation_id.is_some()
        && job_status == Some("completed")
        && job_generation == generation_id
        && receipt_generation == generation_id;
    let unavailable = job_status == Some("error");
    compact_json(json!({
        "status": if ready {
            "ready"
        } else if unavailable {
            "unavailable"
        } else {
            "pending"
        },
        "reason": if ready {
            Value::Null
        } else if unavailable {
            job.get("error_code").cloned().unwrap_or_else(|| json!("projection_failed"))
        } else {
            json!("projection_pending")
        },
        "core_generation_id": generation_id,
        "job": job,
        "status_path": path,
    }))
}

fn legacy_history_report(data_root: &Path) -> Value {
    let path = database_path(data_root.to_path_buf());
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            compact_json(json!({
                "status": "unavailable",
                "reason": "legacy_path_not_regular_file",
                "present": true,
                "active": false,
                "opened": false,
                "purpose": "rollback_or_manual_recovery_only",
                "path": path,
            }))
        }
        Ok(metadata) => compact_json(json!({
            "status": "inactive",
            "reason": "previous_history_epoch",
            "present": true,
            "active": false,
            "opened": false,
            "purpose": "rollback_or_manual_recovery_only",
            "path": path,
            "bytes": metadata.len(),
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => compact_json(json!({
            "status": "absent",
            "reason": Value::Null,
            "present": false,
            "active": false,
            "opened": false,
            "purpose": "rollback_or_manual_recovery_only",
            "path": path,
        })),
        Err(error) => compact_json(json!({
            "status": "unavailable",
            "reason": "legacy_path_inspection_failed",
            "present": Value::Null,
            "active": false,
            "opened": false,
            "purpose": "rollback_or_manual_recovery_only",
            "path": path,
            "last_error": error.to_string(),
        })),
    }
}

fn migration_phase_name(phase: MigrationPhase) -> &'static str {
    match phase {
        MigrationPhase::Detected => "detected",
        MigrationPhase::RebuildPending => "rebuild_pending",
        MigrationPhase::SourceRebuildFailed => "source_rebuild_failed",
        MigrationPhase::Ready => "ready",
    }
}

fn typed_state(status: &'static str, reason: &'static str) -> Value {
    json!({
        "status": status,
        "reason": reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previous_store_is_metadata_only_and_remains_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let path = database_path(temp.path().to_path_buf());
        let sentinel = b"not a sqlite database";
        fs::write(&path, sentinel).unwrap();

        let report = legacy_history_report(temp.path());

        assert_eq!(report["status"], "inactive");
        assert_eq!(report["active"], false);
        assert_eq!(report["opened"], false);
        assert_eq!(fs::read(path).unwrap(), sentinel);
    }

    #[test]
    fn refresh_report_uses_typed_pending_ready_and_unavailable_states() {
        let daemon = json!({"running": true});
        let pending = refresh_report(
            Some(&json!({"request_state": "running"})),
            Some("generation-1"),
            &daemon,
        );
        let ready = refresh_report(
            Some(&json!({
                "request_state": "published",
                "published_generation": "generation-1",
            })),
            Some("generation-1"),
            &daemon,
        );
        let unavailable = refresh_report(None, None, &json!({"running": false}));

        assert_eq!(pending["status"], "pending");
        assert_eq!(ready["status"], "ready");
        assert_eq!(unavailable["status"], "unavailable");
        assert_eq!(unavailable["reason"], "daemon_unavailable");
    }
}

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
