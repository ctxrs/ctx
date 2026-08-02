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
    let generation_id = ctx_history_index::GenerationWriter::open(
        data_root.join("search/lexical"),
        ctx_history_index::WriterOptions::default(),
    )
    .unwrap()
    .commit(|_| true)
    .unwrap()
    .generation_id;
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
fn refresh_report_preserves_optional_active_source_record_progress() {
    let job = json!({
        "request_state": "running",
        "progress": {
            "phase": "refreshing",
            "completed_sources": 2,
            "total_sources": 6,
            "current_source": "source.db",
            "completed_records": 1234,
        },
    });
    let daemon = json!({"running": true});

    let report = refresh_report(Some(&job), None, &daemon);

    assert_eq!(report["progress"]["current_source"], "source.db");
    assert_eq!(report["progress"]["completed_records"], 1234);
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

    assert_eq!(pending["status"], "pending");
    assert_eq!(ready["status"], "ready");
    assert_eq!(stale["status"], "stale");
    assert_eq!(stale["certified_source_count"], 2);
    assert_eq!(stale["certified_source_bytes"], 4096);
    assert_eq!(stale["timings_us"]["commit"], 33);
    assert_eq!(unavailable["status"], "unavailable");
    assert_eq!(unavailable["reason"], "daemon_unavailable");
}

#[test]
fn catalog_status_requires_matching_job_and_terminal_receipt_authority() {
    let temp = tempfile::tempdir().unwrap();
    let authority = load_explicit_source_catalog_authority(temp.path())
        .unwrap()
        .to_json();
    let ready_job = json!({
        "request_state": "published",
        "published_generation": "generation-1",
        "published_explicit_source_catalog": authority,
        "receipt": {
            "published_explicit_source_catalog": authority,
        },
    });
    let ready = catalog_report(temp.path(), Some("generation-1"), Some(&ready_job), None);

    assert_eq!(ready["status"], "ready");
    assert_eq!(ready["published_authority_present"], true);

    for unverified in [
        json!({
            "request_state": "published",
            "published_generation": "generation-1",
        }),
        json!({
            "request_state": "published",
            "published_generation": "generation-1",
            "published_explicit_source_catalog": authority,
            "receipt": {},
        }),
        json!({
            "request_state": "published",
            "published_generation": "generation-1",
            "published_explicit_source_catalog": authority,
            "receipt": {
                "published_explicit_source_catalog": {
                    "schema_version": 1,
                    "revision": 7,
                    "integrity": {
                        "algorithm": "sha256",
                        "digest": "77".repeat(32),
                    },
                },
            },
        }),
        json!({
            "request_state": "published",
            "published_generation": "generation-1",
            "published_explicit_source_catalog": {
                "schema_version": 99,
                "revision": 0,
                "integrity": {
                    "algorithm": "sha256",
                    "digest": "00".repeat(32),
                },
            },
            "receipt": {
                "published_explicit_source_catalog": {
                    "schema_version": 99,
                    "revision": 0,
                    "integrity": {
                        "algorithm": "sha256",
                        "digest": "00".repeat(32),
                    },
                },
            },
        }),
    ] {
        let report = catalog_report(temp.path(), Some("generation-1"), Some(&unverified), None);
        assert_eq!(report["status"], "unavailable");
        assert_eq!(report["reason"], "catalog_publication_unverified");
        assert_eq!(report["published_authority_present"], false);
    }
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
