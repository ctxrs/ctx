use serde_json::json;

use super::*;

fn target() -> DaemonSemanticCompletionTarget {
    DaemonSemanticCompletionTarget::new(
        "generation-1",
        "sha256:model",
        "sha256:source",
        DaemonSemanticConfigBinding::new(
            true,
            "full",
            true,
            "https://semantic.example.test/",
            "sha256:model",
        ),
    )
}

fn status(reload_status: &str, requested_executor: &str, applied_executor: &str) -> Value {
    json!({
        "status": "running",
        "config_reload": {
            "status": reload_status,
            "last_attempt_at_ms": 100,
            "last_applied_at_ms": 90,
            "requested": {
                "daemon_enabled": true,
                "daemon_mode": "full",
                "semantic_enabled": true,
                "semantic_executor": requested_executor,
                "semantic_contract_fingerprint": "sha256:model",
            },
            "applied": {
                "daemon_enabled": true,
                "daemon_mode": "full",
                "semantic_enabled": true,
                "semantic_executor": applied_executor,
                "semantic_contract_fingerprint": "sha256:model",
            },
        },
    })
}

fn job(status: &str) -> Value {
    json!({
        "status": status,
        "last_run_at_ms": 200,
        "core_generation_id": "generation-1",
        "model_contract_fingerprint": "sha256:model",
        "source_contract_fingerprint": "sha256:source",
        "source_generation_ready": status == "ready",
        "source_work_remaining": status != "ready",
    })
}

#[test]
fn ready_requires_exact_requested_applied_and_job_bindings() {
    let selected = "https://semantic.example.test/";
    assert_eq!(
        classify_exact_daemon_semantic_completion(
            &status("applied", selected, selected),
            Some(&job("ready")),
            &target(),
        ),
        DaemonSemanticCompletionObservation::Ready
    );

    let stale_config = classify_exact_daemon_semantic_completion(
        &status("applied", selected, "https://old.example.test/"),
        Some(&job("ready")),
        &target(),
    );
    assert!(matches!(
        stale_config,
        DaemonSemanticCompletionObservation::Pending(DaemonSemanticProgress {
            requested_config_matches: true,
            applied_config_matches: false,
            ..
        })
    ));

    let mut stale_job = job("ready");
    stale_job["source_contract_fingerprint"] = json!("sha256:old-source");
    assert!(matches!(
        classify_exact_daemon_semantic_completion(
            &status("applied", selected, selected),
            Some(&stale_job),
            &target(),
        ),
        DaemonSemanticCompletionObservation::Pending(_)
    ));
}

#[test]
fn matching_activation_failure_is_terminal_but_stale_failure_is_ignored() {
    let selected = "https://semantic.example.test/";
    let mut failed = status("activation_failed", selected, "");
    failed["config_reload"]["last_error"] = json!("contract endpoint unavailable");
    assert_eq!(
        classify_exact_daemon_semantic_completion(&failed, None, &target()),
        DaemonSemanticCompletionObservation::ActivationFailed {
            detail: "contract endpoint unavailable".to_owned(),
            retryable: true,
        }
    );

    let stale = status("activation_failed", "https://old.example.test/", "");
    assert!(matches!(
        classify_exact_daemon_semantic_completion(&stale, None, &target()),
        DaemonSemanticCompletionObservation::Pending(_)
    ));
}

#[test]
fn matching_job_failure_preserves_retryability_and_taxonomy() {
    let selected = "https://semantic.example.test/";
    let mut failed_job = job("failed");
    failed_job["retryable"] = json!(true);
    failed_job["failure_class"] = json!("retryable");
    failed_job["last_error"] = json!("backend unavailable");
    assert_eq!(
        classify_exact_daemon_semantic_completion(
            &status("applied", selected, selected),
            Some(&failed_job),
            &target(),
        ),
        DaemonSemanticCompletionObservation::JobFailed {
            detail: "backend unavailable".to_owned(),
            retryable: true,
            failure_class: Some("retryable".to_owned()),
        }
    );
}

#[test]
fn progress_marker_changes_only_with_observable_activation_or_job_progress() {
    let selected = "https://semantic.example.test/";
    let first = classify_exact_daemon_semantic_completion(
        &status("pending", selected, "https://old.example.test/"),
        Some(&job("budget_exhausted")),
        &target(),
    );
    let mut advanced_job = job("budget_exhausted");
    advanced_job["last_run_at_ms"] = json!(201);
    advanced_job["indexed_chunks"] = json!(32);
    let second = classify_exact_daemon_semantic_completion(
        &status("pending", selected, "https://old.example.test/"),
        Some(&advanced_job),
        &target(),
    );
    assert_ne!(first, second);
}

#[test]
fn unrelated_reload_and_job_receipts_do_not_count_as_exact_progress() {
    let selected = "https://semantic.example.test/";
    let mut stale_job = job("budget_exhausted");
    stale_job["core_generation_id"] = json!("generation-old");
    let first = classify_exact_daemon_semantic_completion(
        &status("pending", "https://old.example.test/", selected),
        Some(&stale_job),
        &target(),
    );
    let mut later_status = status("pending", "https://old.example.test/", selected);
    later_status["config_reload"]["last_attempt_at_ms"] = json!(999);
    stale_job["last_run_at_ms"] = json!(999);
    stale_job["indexed_chunks"] = json!(99);
    let second =
        classify_exact_daemon_semantic_completion(&later_status, Some(&stale_job), &target());

    assert_eq!(first, second);
    assert!(matches!(
        first,
        DaemonSemanticCompletionObservation::Pending(DaemonSemanticProgress {
            requested_config_matches: false,
            job_target_matches: false,
            reload_status: None,
            job_status: None,
            ..
        })
    ));
}
