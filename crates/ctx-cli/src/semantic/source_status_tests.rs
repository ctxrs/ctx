use super::*;

#[test]
fn durable_state_path_is_purpose_based() {
    assert_eq!(
        daemon_jobs_path(Path::new("ctx-data")).join(SOURCE_BACKED_PRO_CATCH_UP_STATUS_FILE),
        Path::new("ctx-data/daemon/jobs/pro-catch-up.json")
    );
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
fn pro_source_manifest_receipt_is_generation_bound() {
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

    assert_eq!(ready["authority"], "source_manifest");
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
