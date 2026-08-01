use super::*;
use std::{
    fs, io,
    sync::{Arc, Mutex},
};

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
    assert!(jobs.contains_key("source_backed_refresh"), "{daemon:#}");
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
    assert_eq!(json_status["catalog"]["status"], "ready");
    assert_eq!(json_status["refresh"]["status"], "ready");
    assert_eq!(
        json_status["refresh"]["published_generation"],
        generation_id
    );
    assert_eq!(json_status["relational"]["status"], "pending");

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
    assert!(rendered.contains("Session view pending"), "{rendered}");
    assert!(!rendered.contains("Catalog pending"), "{rendered}");
    assert!(!rendered.contains("source refresh pending"), "{rendered}");
}

#[test]
fn pro_core_receipt_is_generation_bound() {
    let ready_job = json!({
        "status": "completed",
        "core_generation_id": "generation-1",
        "receipt_core_generation_id": "generation-1",
        "attempts": 1,
    });
    let stale_job = json!({
        "status": "completed",
        "core_generation_id": "generation-0",
        "receipt_core_generation_id": "generation-0",
        "attempts": 1,
    });
    let retry_job = json!({
        "status": "error",
        "error_code": "helper_crashed",
        "core_generation_id": "generation-1",
        "receipt_core_generation_id": null,
        "attempts": 2,
        "retryable": true,
        "consecutive_failures": 2,
        "retry_after_ms": 250,
        "retry_not_before_at_ms": 1234,
    });

    let ready =
        pro_projection_report_from_job(Some("generation-1"), &ready_job, "pro-catch-up.json");
    let stale =
        pro_projection_report_from_job(Some("generation-1"), &stale_job, "pro-catch-up.json");
    let retry =
        pro_projection_report_from_job(Some("generation-1"), &retry_job, "pro-catch-up.json");

    assert_eq!(ready["authority"], "core_generation");
    assert_eq!(ready["receipt"]["status"], "ready");
    assert_eq!(ready["receipt"]["generation_matches"], true);
    assert_eq!(stale["status"], "stale");
    assert_eq!(stale["receipt"]["status"], "stale");
    assert_eq!(stale["receipt"]["generation_matches"], false);
    assert_eq!(retry["status"], "unavailable");
    assert_eq!(retry["reason"], "helper_crashed");
    assert_eq!(retry["consecutive_failures"], 2);
    assert_eq!(retry["retry_after_ms"], 250);
    assert_eq!(retry["retry_not_before_at_ms"], 1234);
}
