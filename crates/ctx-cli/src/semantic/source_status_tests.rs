use super::*;
use std::{
    cell::{Cell, RefCell},
    fs, io,
    sync::{Arc, Mutex},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, WriterOptions};

use crate::{
    analytics::StatusTelemetry,
    output::JsonOutputFormat,
    ui::{ColorMode, RenderContext, StreamKind, TestContext, Ui},
    StatusArgs,
};

#[derive(Clone, Default)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl SharedWriter {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl io::Write for SharedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn core_publication_fixture() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let route_identity = "ab".repeat(32);
    let publication = ctx_history_index::GenerationWriter::open(
        data_root.join("search/lexical"),
        ctx_history_index::WriterOptions::default(),
    )
    .unwrap()
    .into_writer()
    .unwrap()
    .commit_with_publication_metadata(
        |_| true,
        |context| {
            let generation_id = context.generation_id().to_owned();
            let route = ctx_history_index::SourceRouteIdentity::from_sha256(
                route_identity.clone(),
            )
            .map_err(|error| {
                ctx_history_index::IndexError::PublicationMetadata(error.to_string())
            })?;
            let receipt = ctx_history_refresh::SourceBackedRefreshReceipt {
                previous_generation: None,
                published_generation: generation_id.clone(),
                generation_changed: true,
                published_explicit_source_catalog: None,
                current: ctx_history_refresh::SourceBackedRefreshCurrent::default(),
                route_results: vec![ctx_history_refresh::SourceBackedRefreshRouteResult::succeeded(
                    route_identity.clone(),
                    true,
                )],
                zero_source_authority: vec![
                    ctx_history_refresh::SourceBackedZeroSourceAuthority {
                        generation_id,
                        route_identity: route,
                        kind: ctx_history_refresh::SourceBackedZeroSourceAuthorityKind::CompleteEmptyInventory,
                    },
                ],
                catalog_route_bindings: Vec::new(),
            };
            serde_json::to_vec(&json!({
                "version": 2,
                "request_id": "core-publication",
                "operation": "refresh",
                "refresh_scope": {"kind": "all"},
                "receipt": receipt.to_json(),
                "route_observations": [null],
            }))
            .map_err(|error| ctx_history_index::IndexError::PublicationMetadata(error.to_string()))
        },
    )
    .unwrap();
    let generation_id = publication.receipt().generation_id.clone();
    let catalog = load_explicit_source_catalog_authority(&data_root).unwrap();
    super::super::paths_status::write_daemon_job_status(
        &daemon_core_refresh_job_path(&data_root),
        &json!({
            "mode": "background",
            "owner": "daemon",
            "kind": "core_refresh",
            "status": "completed",
            "request_id": "core-publication",
            "request_state": "published",
            "previous_generation": null,
            "published_generation": generation_id,
            "requested_explicit_source_catalog": catalog.to_json(),
            "published_explicit_source_catalog": catalog.to_json(),
            "generation_changed": true,
            "certified_source_count": 0,
            "certified_source_bytes": 0,
            "receipt": {
                "previous_generation": null,
                "published_generation": generation_id,
                "generation_changed": true,
                "published_explicit_source_catalog": catalog.to_json(),
                "current": {
                    "current_source_count": 0,
                    "current_indexed_documents": 0,
                    "current_complete_records": 0,
                    "current_retained_records": 0,
                    "current_rejected_records": 0,
                    "current_ignored_records": 0,
                    "current_certified_source_bytes": 0,
                    "current_sources_with_rejections": 0,
                    "removed_source_count": 0,
                },
            },
            "progress": {
                "phase": "committed",
                "completed_sources": 0,
                "total_sources": 0,
            },
            "daemon_mode": "full",
            "trigger": "periodic",
            "trigger_provenance": "daemon_scheduler",
        }),
    )
    .unwrap();
    (temp, data_root, generation_id)
}

fn publish_changed_core_generation(data_root: &Path) -> String {
    let source = ctx_history_core::SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("status-snapshot-race.jsonl").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let native_session = TypedKey::utf8("status-snapshot-race-session").unwrap();
    let session_key = NativeSessionKey::native_id("session", native_session).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let native_item = NativeItemKey::native_id(
        "message",
        TypedKey::utf8("status-snapshot-race-event").unwrap(),
    )
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        1,
        "message",
        "primary",
        true,
        "status-snapshot-race-v1",
        "new generation published during status assembly",
    )
    .unwrap();
    record.provider_session_id = Some("status-snapshot-race-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(1));
    record.role = Some("assistant".to_owned());
    record.validate_contract().unwrap();

    let mut writer = GenerationWriter::open(
        data_root.join("search/lexical"),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1024 * 1024,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(record).unwrap();
    let observation = SourceObservation::new(source, "status-snapshot-race-v1", vec![2]).unwrap();
    writer
        .certify_source(
            CertifiedSource::certify(
                observation.clone(),
                observation,
                "status-snapshot-race-v1",
                [2; 32],
                ScannedSourceCounts {
                    complete_records: 1,
                    retained_records: 1,
                    indexed_documents: 1,
                    certified_bytes: 128,
                    ..ScannedSourceCounts::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap().generation_id
}

#[test]
fn durable_state_path_is_purpose_based() {
    assert_eq!(
        daemon_jobs_path(Path::new("ctx-data")).join(PRO_CATCH_UP_STATUS_FILE),
        Path::new("ctx-data/daemon/jobs/pro-catch-up.json")
    );
}

#[test]
fn status_contract_has_no_resolver_or_source_manifest_authority() {
    let production = include_str!("source_status.rs");
    assert!(!production.contains("resolver_report"));
    assert!(!production.contains("\"resolver\""));
    assert!(!production.contains("source_manifest"));
}

#[test]
fn pristine_source_status_is_read_only_and_exposes_stable_paths() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("missing");

    let status =
        source_epoch_status_report(&data_root, &AppConfig::default()).expect("source status");

    assert!(!data_root.exists());
    assert_eq!(
        status.report["lexical"]["path"],
        json!(data_root.join("search/lexical"))
    );
    assert_eq!(
        status.report["semantic"]["flat_f32"]["path"],
        json!(data_root.join("search/semantic"))
    );
    assert!(status.report.get("prior_epoch").is_none());
}

#[test]
fn refresh_report_preserves_optional_active_source_record_and_byte_progress() {
    let job = json!({
        "request_state": "running",
        "request_id": "logical-request",
        "logical_request_id": "logical-request",
        "logical_phase": "attached",
        "physical_attempt_id": "physical-attempt",
        "physical_attempt_state": "running",
        "progress_owner_request_id": "progress-owner",
        "progress_owner_attempt_state": "running",
        "structured_outcome": {"code": "exact-engine-value"},
        "progress": {
            "phase": "refreshing",
            "completed_sources": 2,
            "total_sources": 6,
            "current_source": "source.db",
            "completed_records": 1234,
            "completed_bytes": 4 * 1024 * 1024,
        },
    });
    let daemon = json!({"running": true});

    let report = refresh_report(Some(&job), None, &daemon);

    assert_eq!(report["progress"]["current_source"], "source.db");
    assert_eq!(report["progress"]["completed_records"], 1234);
    assert_eq!(report["progress"]["completed_bytes"], 4 * 1024 * 1024);
    for field in [
        "logical_request_id",
        "logical_phase",
        "physical_attempt_id",
        "physical_attempt_state",
        "progress_owner_request_id",
        "progress_owner_attempt_state",
        "structured_outcome",
    ] {
        assert_eq!(report[field], job[field], "field={field}");
    }
}

#[test]
fn source_daemon_report_preserves_semantic_terminal_job_facts() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    fs::create_dir_all(&data_root).unwrap();
    fs::write(
        data_root.join(crate::config::CONFIG_FILE),
        "[daemon]\nenabled = true\n\n[search]\nsemantic = true\n",
    )
    .unwrap();
    super::super::paths_status::write_daemon_job_status(
        &daemon_semantic_job_path(&data_root),
        &json!({
            "status": "skipped",
            "reason": "model_cache_missing",
            "last_run_at_ms": 1,
        }),
    )
    .unwrap();

    let daemon = source_daemon_report(&data_root);
    let jobs = daemon["jobs"].as_object().unwrap();
    assert!(jobs.contains_key("core_refresh"), "{daemon:#}");
    assert!(jobs.contains_key("semantic_index"), "{daemon:#}");
    assert!(!jobs.contains_key("history_refresh"), "{daemon:#}");
    assert_eq!(
        daemon["jobs"]["semantic_index"]["last_run_status"],
        "skipped"
    );
    assert_eq!(
        daemon["jobs"]["semantic_index"]["last_run_reason"],
        "model_cache_missing"
    );
    if super::super::semantic_query_service_supported() {
        assert_eq!(daemon["jobs"]["semantic_index"]["status"], "skipped");
        assert_eq!(
            daemon["jobs"]["semantic_index"]["reason"],
            "model_cache_missing"
        );
    }
}

#[test]
fn lexical_state_depends_only_on_verified_generation_policy_identity() {
    assert_eq!(lexical_state(true), ("ready", None));
    assert_eq!(
        lexical_state(false),
        ("stale", Some("generation_policy_mismatch"))
    );
}

#[test]
fn refresh_report_uses_typed_pending_ready_stale_and_unavailable_states() {
    let daemon = json!({"running": true});
    for request_state in ["admission_pending", "queued", "running"] {
        let pending = refresh_report(
            Some(&json!({"request_state": request_state})),
            Some("generation-1"),
            &daemon,
        );
        assert_eq!(pending["status"], "pending", "{request_state}");
    }
    let ready = refresh_report(
        Some(&json!({
            "request_state": "published",
            "published_generation": "generation-1",
        })),
        Some("generation-1"),
        &daemon,
    );
    let stale = refresh_report(
        Some(&json!({
            "request_state": "published",
            "published_generation": "generation-0",
            "certified_source_count": 2,
            "certified_source_bytes": 4096,
            "timings_us": {"discovery": 11, "scan_stage": 22, "commit": 33},
        })),
        Some("generation-1"),
        &daemon,
    );
    let unavailable = refresh_report(None, None, &json!({"running": false}));

    assert_eq!(ready["status"], "ready");
    assert_eq!(stale["status"], "stale");
    assert_eq!(stale["certified_source_count"], 2);
    assert_eq!(stale["certified_source_bytes"], 4096);
    assert_eq!(stale["timings_us"]["commit"], 33);
    assert_eq!(unavailable["status"], "unavailable");
    assert_eq!(unavailable["reason"], "daemon_unavailable");
}

#[test]
fn refresh_report_keeps_published_sources_distinct_from_route_inventory() {
    let report = refresh_report(
        Some(&json!({
            "request_state": "published",
            "published_generation": "generation-1",
            "source_count": 1,
            "scanned_routes": 38,
            "unsupported_routes": 37,
            "progress": {
                "phase": "published",
                "completed_sources": 38,
                "total_sources": 38,
                "total_sources_known": true,
            },
            "receipt": {
                "outcome": "completed",
                "current": {"current_source_count": 2},
            },
        })),
        Some("generation-1"),
        &json!({"running": true}),
    );

    assert_eq!(report["source_count"], 1);
    assert_eq!(report["current"]["current_source_count"], 2);
    assert_eq!(report["scanned_routes"], 38);
    assert_eq!(report["unsupported_routes"], 37);
    assert_eq!(report["progress"]["total_sources"], 38);
}

#[test]
fn admission_pending_is_active_with_existing_and_empty_generations() {
    let (_temp, data_root, generation_id) = core_publication_fixture();
    super::super::paths_status::write_daemon_job_status(
        &daemon_core_refresh_job_path(&data_root),
        &json!({
            "status": "running",
            "request_id": "admission-existing",
            "request_state": "admission_pending",
            "published_generation": generation_id,
        }),
    )
    .unwrap();

    let existing = source_epoch_status_report(&data_root, &AppConfig::default()).unwrap();
    assert_eq!(existing.report["refresh"]["status"], "pending");
    assert_eq!(existing.report["lexical"]["status"], "ready");
    assert_eq!(
        existing.report["lexical"]["request_state"],
        "admission_pending"
    );

    let empty = tempfile::tempdir().unwrap();
    let empty_root = empty.path().join("data");
    super::super::paths_status::write_daemon_job_status(
        &daemon_core_refresh_job_path(&empty_root),
        &json!({
            "status": "running",
            "request_id": "admission-empty",
            "request_state": "admission_pending",
        }),
    )
    .unwrap();
    let empty = source_epoch_status_report(&empty_root, &AppConfig::default()).unwrap();
    assert_eq!(empty.report["refresh"]["status"], "pending");
    assert_eq!(empty.report["lexical"]["status"], "pending");
    assert_eq!(
        empty.report["lexical"]["reason"],
        "generation_not_published"
    );
}

#[test]
fn authoritative_empty_stays_query_ready_when_the_latest_refresh_failed() {
    let (_temp, data_root, generation_id) = core_publication_fixture();
    super::super::paths_status::write_daemon_job_status(
        &daemon_core_refresh_job_path(&data_root),
        &json!({
            "status": "failed",
            "request_id": "failed-after-authoritative-empty",
            "request_state": "failed",
            "published_generation": generation_id,
            "last_error": "all_provider_terminal_coverage_unavailable",
        }),
    )
    .unwrap();

    let status = source_epoch_status_report(&data_root, &AppConfig::default()).unwrap();
    assert_eq!(status.report["lexical"]["status"], "ready");
    assert_eq!(status.report["history_epoch"]["status"], "ready");
    assert_eq!(status.report["refresh"]["status"], "unavailable");
    assert_eq!(status.report["refresh"]["reason"], "core_refresh_failed");
    assert_eq!(status.indexed_items, Some(0));
}

#[test]
fn legacy_zero_source_publication_is_not_projected_as_ready() {
    let (_temp, data_root, generation_id) = core_publication_fixture();
    let index_root = data_root.join("search/lexical");
    let current = VerifiedIndex::open(&index_root).unwrap();
    let mut metadata: Value =
        serde_json::from_slice(current.publication_metadata().unwrap()).unwrap();
    metadata["version"] = json!(1);
    metadata["receipt"]
        .as_object_mut()
        .unwrap()
        .remove("zero_source_authority");
    drop(current);
    let writer = GenerationWriter::open(&index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer
        .republish_current_publication_metadata(
            &generation_id,
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();

    let status = source_epoch_status_report(&data_root, &AppConfig::default()).unwrap();
    assert_eq!(status.report["lexical"]["status"], "unavailable");
    assert_eq!(
        status.report["lexical"]["reason"],
        "zero_source_publication_uncertified"
    );
    assert_eq!(status.report["history_epoch"]["status"], "unavailable");
    assert_eq!(status.indexed_items, None);
    assert_eq!(status.indexed_sources, None);
}

#[test]
fn refresh_report_is_partial_when_a_transcript_route_failed_or_rejected_records() {
    let daemon = json!({"running": true});
    for outcome in [
        "completed_with_source_failures",
        "completed_with_rejections",
        "completed_with_rejections_and_source_failures",
    ] {
        let report = refresh_report(
            Some(&json!({
                "request_state": "published",
                "published_generation": "generation-1",
                "receipt": {
                    "outcome": outcome,
                    "source_failure_total": usize::from(outcome.contains("source_failures")),
                    "rejected_record_total": u64::from(outcome.contains("rejections")),
                    "current": {
                        "current_rejected_records": u64::from(outcome.contains("rejections")),
                    },
                },
            })),
            Some("generation-1"),
            &daemon,
        );
        assert_eq!(report["status"], "partial", "{outcome}: {report:#}");
        assert_eq!(report["outcome"], outcome);
    }
}

#[test]
fn catalog_status_reports_automatic_roots_and_request_scoped_explicit_overlays() {
    let temp = tempfile::tempdir().unwrap();
    let index_root = temp.path().join("search/lexical");
    let generation_id = ctx_history_index::GenerationWriter::open(
        &index_root,
        ctx_history_index::WriterOptions::default(),
    )
    .unwrap()
    .into_writer()
    .unwrap()
    .commit(|_| true)
    .unwrap()
    .generation_id;
    let index = VerifiedIndex::open(&index_root).unwrap();
    let ready = catalog_report(Some(&generation_id), Some(&index));
    assert_eq!(ready["status"], "ready");
    assert_eq!(ready["authority"], "automatic_provider_registry");
    assert_eq!(ready["explicit_import_authority"], "request_scoped_overlay");
    assert_eq!(ready["persisted_explicit_roots"], false);

    let pending = catalog_report(None, None);
    assert_eq!(pending["status"], "pending");
}

#[test]
fn core_publication_is_ready_in_json_and_human_status() {
    let (_temp, data_root, generation_id) = core_publication_fixture();
    let config = AppConfig::default();
    let json_status = crate::commands::status::status_read_model(&data_root, &config)
        .unwrap()
        .report;

    assert_eq!(json_status["lexical"]["status"], "ready");
    assert_eq!(json_status["lexical"]["generation_id"], generation_id);
    assert!(json_status.get("catalog").is_none());
    assert_eq!(json_status["refresh"]["status"], "ready");
    assert_eq!(
        json_status["refresh"]["published_generation"],
        generation_id
    );
    assert!(json_status.get("relational").is_none());

    let stdout = SharedWriter::default();
    let stdout_copy = stdout.clone();
    let stdout_context =
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Never));
    let stderr_context =
        RenderContext::for_test(TestContext::tty(StreamKind::Stderr, 80).color(ColorMode::Never));
    let mut ui = Ui::with_writers(
        stdout,
        stdout_context,
        SharedWriter::default(),
        stderr_context,
    );
    crate::commands::status::run_status(
        StatusArgs {
            format: JsonOutputFormat::Text,
            usage: None,
        },
        data_root,
        false,
        &mut StatusTelemetry::default(),
        &mut ui,
    )
    .unwrap();
    let rendered = stdout_copy
        .text()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(rendered.contains("Search ready"), "{rendered}");
    assert!(!rendered.contains("Session view"), "{rendered}");
    assert!(!rendered.contains("Catalog pending"), "{rendered}");
    assert!(!rendered.contains("source refresh pending"), "{rendered}");
}

#[test]
fn public_status_model_pins_core_once_and_queries_pro_once() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("missing");
    let (status, counts) = count_public_status_snapshot_reads(|| {
        crate::commands::status::status_read_model(&data_root, &AppConfig::default())
    });

    status.unwrap();
    assert_eq!(
        counts,
        StatusSnapshotReadCounts {
            core_pins: 1,
            pro_queries: 1,
        }
    );
}

#[test]
fn corrupt_core_and_unavailable_pro_remain_typed_in_one_snapshot() {
    let (_temp, data_root, _generation_id) = core_publication_fixture();
    fs::write(
        data_root.join("search/lexical/active-generation.json"),
        b"{corrupt",
    )
    .unwrap();

    let (status, counts) = count_public_status_snapshot_reads(|| {
        crate::commands::status::status_read_model(&data_root, &AppConfig::default())
    });
    let status = status.unwrap().report;

    assert_eq!(counts.core_pins, 1);
    assert_eq!(counts.pro_queries, 1);
    assert_eq!(status["lexical"]["status"], "unavailable");
    assert_eq!(
        status["lexical"]["reason"],
        "generation_verification_failed"
    );
    assert_eq!(status["pro"]["installed"], false);
    assert_eq!(status["pro"]["state"], "not_setup");
    assert_eq!(status["pro_projection"]["status"], "unavailable");
    assert_eq!(status["pro_projection"]["reason"], "pro_not_installed");
}

#[test]
fn generation_publish_during_pro_query_cannot_mix_status_snapshot() {
    let (_temp, data_root, pinned_generation) = core_publication_fixture();
    let pro_queries = Cell::new(0);
    let requested_generation = RefCell::new(None);
    let published_generation = RefCell::new(None);

    let (status, core_pins) =
        super::super::source_backed_refresh_coordinator::count_verified_index_opens(|| {
            source_epoch_status_report_with_pro_query(
                &data_root,
                &AppConfig::default(),
                |_, core| {
                    pro_queries.set(pro_queries.get() + 1);
                    let requested = core.unwrap().generation_id().to_owned();
                    requested_generation.replace(Some(requested));
                    published_generation.replace(Some(publish_changed_core_generation(&data_root)));
                    json!({
                        "installed": true,
                        "ready": true,
                        "materialized": true,
                        "projection_currentness": "current",
                        "materialized_coverage": "complete",
                        "repository_coverage": {},
                        "access_state": "active",
                        "supported_operations": ["file_blame"],
                        "available_operations": ["file_blame"],
                        "error_code": null,
                    })
                },
            )
            .unwrap()
        });

    let published_generation = published_generation.into_inner().unwrap();
    assert_ne!(published_generation, pinned_generation);
    assert_eq!(pro_queries.get(), 1);
    assert_eq!(core_pins, 1);
    assert_eq!(
        requested_generation.into_inner(),
        Some(pinned_generation.clone())
    );
    assert_eq!(status.report["lexical"]["generation_id"], pinned_generation);
    assert_eq!(
        status.report["pro_projection"]["core_generation_id"],
        pinned_generation
    );
    assert_eq!(status.report["pro_projection"]["status"], "ready");
    assert_eq!(status.indexed_items, Some(0));
    let active = VerifiedIndex::open_pinned(data_root.join("search/lexical")).unwrap();
    assert_eq!(active.generation_id(), published_generation);
    assert_eq!(active.document_count(), 1);
}

#[test]
fn pro_helper_status_is_the_only_projection_readiness_authority() {
    let helper_ready = json!({
        "installed": true,
        "ready": false,
        "materialized": true,
        "projection_currentness": "current",
        "materialized_coverage": "empty",
        "repository_coverage": {},
        "access_state": "trial",
        "supported_operations": ["file_blame"],
        "available_operations": [],
        "error_code": null,
    });
    let helper_stale = json!({
        "installed": true,
        "ready": false,
        "materialized": false,
        "projection_currentness": "stale",
        "materialized_coverage": "partial",
        "repository_coverage": {},
        "access_state": "active",
        "supported_operations": ["file_blame"],
        "available_operations": [],
        "error_code": "stale_source",
    });
    let helper_finalizing = json!({
        "installed": true,
        "ready": true,
        "materialized": false,
        "projection_currentness": "finalizing",
        "materialized_coverage": "complete",
        "repository_coverage": {},
        "access_state": "active",
        "supported_operations": ["file_blame"],
        "available_operations": ["file_blame"],
        "error_code": "finalizing",
    });
    let completed_job = json!({
        "status": "completed",
        "pending": false,
        "attempts": 1,
    });
    let retry_job = json!({
        "status": "error",
        "error_code": "helper_crashed",
        "core_generation_id": "generation-0",
        "receipt_core_generation_id": null,
        "attempts": 2,
        "retryable": true,
        "consecutive_failures": 2,
        "retry_after_ms": 250,
        "retry_not_before_at_ms": 1234,
    });

    let ready = pro_projection_report_from_status(
        Some("generation-1"),
        true,
        &helper_ready,
        Some(&retry_job),
        "pro-catch-up.json",
    );
    let stale = pro_projection_report_from_status(
        Some("generation-1"),
        true,
        &helper_stale,
        Some(&completed_job),
        "pro-catch-up.json",
    );
    let finalizing = pro_projection_report_from_status(
        Some("generation-1"),
        false,
        &helper_finalizing,
        Some(&retry_job),
        "pro-catch-up.json",
    );

    assert_eq!(ready["authority"], "pro_helper_status");
    assert_eq!(ready["status"], "ready");
    assert_eq!(ready["receipt"]["status"], "ready");
    assert_eq!(ready["receipt"]["generation_matches"], true);
    assert_eq!(ready["materialized_coverage"], "empty");
    assert_eq!(ready["ready"], false);
    assert_eq!(ready["materialized"], true);
    assert_eq!(ready["available_operations"], json!([]));
    assert_eq!(ready["catch_up"]["status"], "error");
    assert_eq!(ready["catch_up"]["error_code"], "helper_crashed");
    assert_eq!(ready["catch_up"]["core_generation_id"], "generation-0");
    assert_eq!(ready["catch_up"]["consecutive_failures"], 2);
    assert_eq!(ready["catch_up"]["retry_after_ms"], 250);
    assert_eq!(ready["catch_up"]["retry_not_before_at_ms"], 1234);
    assert_eq!(stale["status"], "stale");
    assert_eq!(stale["receipt"]["status"], "stale");
    assert_eq!(stale["receipt"]["generation_matches"], false);
    assert_eq!(stale["catch_up"]["status"], "completed");
    assert_eq!(stale["reason"], "stale_source");
    assert_eq!(finalizing["status"], "pending");
    assert_eq!(finalizing["reason"], "finalizing");
    assert_eq!(finalizing["available_operations"], json!(["file_blame"]));

    let raced = pro_projection_report_from_status(
        Some("generation-1"),
        false,
        &helper_ready,
        Some(&completed_job),
        "pro-catch-up.json",
    );
    assert_eq!(raced["status"], "stale");
    assert_eq!(raced["reason"], "stale_source");
    assert_eq!(raced["receipt"]["generation_matches"], false);
}
