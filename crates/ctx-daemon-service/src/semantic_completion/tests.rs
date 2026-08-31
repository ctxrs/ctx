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
            true,
            None,
        ),
    )
}

fn unthrottled_builtin_target() -> DaemonSemanticCompletionTarget {
    DaemonSemanticCompletionTarget::new(
        "generation-1",
        "sha256:model",
        "sha256:source",
        DaemonSemanticConfigBinding::new(
            true,
            "full",
            true,
            "builtin",
            "sha256:model",
            false,
            Some(false),
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
                "semantic_builtin_throttling_configured": true,
                "semantic_builtin_throttling_effective": null,
            },
            "applied": {
                "daemon_enabled": true,
                "daemon_mode": "full",
                "semantic_enabled": true,
                "semantic_executor": applied_executor,
                "semantic_contract_fingerprint": "sha256:model",
                "semantic_builtin_throttling_configured": true,
                "semantic_builtin_throttling_effective": null,
            },
        },
    })
}

fn unthrottled_builtin_status() -> Value {
    json!({
        "status": "running",
        "config_reload": {
            "status": "applied",
            "requested": {
                "daemon_enabled": true,
                "daemon_mode": "full",
                "semantic_enabled": true,
                "semantic_executor": "builtin",
                "semantic_contract_fingerprint": "sha256:model",
                "semantic_builtin_throttling_configured": false,
                "semantic_builtin_throttling_effective": false,
            },
            "applied": {
                "daemon_enabled": true,
                "daemon_mode": "full",
                "semantic_enabled": true,
                "semantic_executor": "builtin",
                "semantic_contract_fingerprint": "sha256:model",
                "semantic_builtin_throttling_configured": false,
                "semantic_builtin_throttling_effective": false,
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
fn ready_requires_exact_unthrottled_builtin_requested_and_applied_identity() {
    let target = unthrottled_builtin_target();
    let exact = unthrottled_builtin_status();
    assert_eq!(
        classify_exact_daemon_semantic_completion(&exact, Some(&job("ready")), &target),
        DaemonSemanticCompletionObservation::Ready
    );

    for binding in ["requested", "applied"] {
        for replacement in [Some(json!(true)), Some(Value::Null), None] {
            let mut stale = exact.clone();
            let config = stale["config_reload"][binding].as_object_mut().unwrap();
            match replacement {
                Some(value) => {
                    config.insert("semantic_builtin_throttling_effective".to_owned(), value);
                }
                None => {
                    config.remove("semantic_builtin_throttling_effective");
                }
            }
            assert!(matches!(
                classify_exact_daemon_semantic_completion(&stale, Some(&job("ready")), &target,),
                DaemonSemanticCompletionObservation::Pending(_)
            ));
        }
    }
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
    for (name, class) in [
        ("retryable", SemanticFailureClass::Retryable),
        ("permanent", SemanticFailureClass::Permanent),
        ("corrupt_sidecar", SemanticFailureClass::CorruptSidecar),
        ("resource_pressure", SemanticFailureClass::ResourcePressure),
    ] {
        let mut failed_job = job("failed");
        failed_job["retryable"] = json!(true);
        failed_job["failure_class"] = json!(name);
        failed_job["last_error"] = json!(format!("{name} failure"));
        assert_eq!(
            classify_exact_daemon_semantic_completion(
                &status("applied", selected, selected),
                Some(&failed_job),
                &target(),
            ),
            DaemonSemanticCompletionObservation::JobFailed {
                detail: format!("{name} failure"),
                retryable: true,
                failure_class: Some(class),
            }
        );
    }
}

#[test]
fn invalid_failure_classes_are_omitted_without_weakening_terminal_failure() {
    let selected = "https://semantic.example.test/";
    for (label, raw_class, retryable) in [
        ("missing", None, false),
        ("unknown", Some(json!("future_failure_class")), true),
        ("non-string", Some(json!(17)), false),
    ] {
        let mut failed_job = job("failed");
        failed_job["retryable"] = json!(retryable);
        failed_job["last_error"] = json!(format!("{label} terminal failure"));
        if let Some(raw_class) = raw_class {
            failed_job["failure_class"] = raw_class;
        }
        assert_eq!(
            classify_exact_daemon_semantic_completion(
                &status("applied", selected, selected),
                Some(&failed_job),
                &target(),
            ),
            DaemonSemanticCompletionObservation::JobFailed {
                detail: format!("{label} terminal failure"),
                retryable,
                failure_class: None,
            }
        );
    }
}

#[test]
fn legacy_skipped_blocking_failures_are_terminal_but_retryable_classes_remain_pending() {
    let selected = "https://semantic.example.test/";
    for (name, class) in [
        ("permanent", SemanticFailureClass::Permanent),
        ("corrupt_sidecar", SemanticFailureClass::CorruptSidecar),
    ] {
        let mut legacy_job = job("skipped");
        legacy_job["failure_class"] = json!(name);
        legacy_job["retryable"] = json!(false);
        legacy_job["last_error"] = json!(format!("legacy {name} failure"));
        assert_eq!(
            classify_exact_daemon_semantic_completion(
                &status("applied", selected, selected),
                Some(&legacy_job),
                &target(),
            ),
            DaemonSemanticCompletionObservation::JobFailed {
                detail: format!("legacy {name} failure"),
                retryable: false,
                failure_class: Some(class),
            },
            "{name}"
        );
    }

    for (job_status, class) in [
        ("skipped", "retryable"),
        ("skipped", "resource_pressure"),
        ("resource_deferred", "resource_pressure"),
    ] {
        let mut retryable_job = job(job_status);
        retryable_job["failure_class"] = json!(class);
        retryable_job["retryable"] = json!(true);
        assert!(matches!(
            classify_exact_daemon_semantic_completion(
                &status("applied", selected, selected),
                Some(&retryable_job),
                &target(),
            ),
            DaemonSemanticCompletionObservation::Pending(_)
        ));
    }
}

#[test]
fn ready_and_exact_target_checks_precede_legacy_failure_classification() {
    let selected = "https://semantic.example.test/";
    let mut ready_job = job("ready");
    ready_job["failure_class"] = json!("permanent");
    ready_job["retryable"] = json!(false);
    ready_job["last_error"] = json!("stale terminal metadata");
    assert_eq!(
        classify_exact_daemon_semantic_completion(
            &status("applied", selected, selected),
            Some(&ready_job),
            &target(),
        ),
        DaemonSemanticCompletionObservation::Ready
    );

    let mut stale_job = job("failed");
    stale_job["core_generation_id"] = json!("generation-old");
    stale_job["failure_class"] = json!("permanent");
    stale_job["retryable"] = json!(false);
    assert!(matches!(
        classify_exact_daemon_semantic_completion(
            &status("applied", selected, selected),
            Some(&stale_job),
            &target(),
        ),
        DaemonSemanticCompletionObservation::Pending(DaemonSemanticProgress {
            job_target_matches: false,
            ..
        })
    ));
}

fn pending(observation: DaemonSemanticCompletionObservation) -> DaemonSemanticProgress {
    match observation {
        DaemonSemanticCompletionObservation::Pending(progress) => progress,
        other => panic!("expected pending observation, got {other:?}"),
    }
}

#[test]
fn substantive_progress_uses_only_the_durable_sequence() {
    let selected = "https://semantic.example.test/";
    let mut first_job = job("budget_exhausted");
    first_job["indexed_chunks"] = json!(8);
    first_job["semantic_progress_sequence"] = json!(8);
    let first = pending(classify_exact_daemon_semantic_completion(
        &status("applied", selected, selected),
        Some(&first_job),
        &target(),
    ));
    let mut advanced_job = job("budget_exhausted");
    advanced_job["last_run_at_ms"] = json!(201);
    advanced_job["indexed_chunks"] = json!(32);
    advanced_job["semantic_progress_sequence"] = json!(16);
    let second = pending(classify_exact_daemon_semantic_completion(
        &status("applied", selected, selected),
        Some(&advanced_job),
        &target(),
    ));
    assert!(second.substantively_advances_from(Some(&first)));

    let mut timestamp_only_job = advanced_job;
    timestamp_only_job["last_run_at_ms"] = json!(999);
    let mut timestamp_only_status = status("applied", selected, selected);
    timestamp_only_status["config_reload"]["last_attempt_at_ms"] = json!(999);
    timestamp_only_status["config_reload"]["last_applied_at_ms"] = json!(999);
    let timestamp_only = pending(classify_exact_daemon_semantic_completion(
        &timestamp_only_status,
        Some(&timestamp_only_job),
        &target(),
    ));
    assert!(!timestamp_only.substantively_advances_from(Some(&second)));
}

#[test]
fn resource_deferred_churn_is_not_substantive_progress() {
    let selected = "https://semantic.example.test/";
    let mut deferred_job = job("resource_deferred");
    deferred_job["reason"] = json!("memory_pressure");
    let first = pending(classify_exact_daemon_semantic_completion(
        &status("applied", selected, selected),
        Some(&deferred_job),
        &target(),
    ));
    assert!(!first.substantively_advances_from(None));

    let mut later_status = status("applied", selected, selected);
    later_status["config_reload"]["last_attempt_at_ms"] = json!(999);
    later_status["config_reload"]["last_applied_at_ms"] = json!(999);
    deferred_job["last_run_at_ms"] = json!(999);
    deferred_job["reason"] = json!("disk_pressure");
    let later = pending(classify_exact_daemon_semantic_completion(
        &later_status,
        Some(&deferred_job),
        &target(),
    ));
    assert!(!later.substantively_advances_from(Some(&first)));
}

#[test]
fn exact_job_binding_and_source_readiness_without_a_sequence_are_not_progress() {
    let selected = "https://semantic.example.test/";
    let config_active = pending(classify_exact_daemon_semantic_completion(
        &status("applied", selected, selected),
        None,
        &target(),
    ));
    let job_active = pending(classify_exact_daemon_semantic_completion(
        &status("applied", selected, selected),
        Some(&job("budget_exhausted")),
        &target(),
    ));
    assert!(!job_active.substantively_advances_from(Some(&config_active)));

    let mut source_ready_job = job("budget_exhausted");
    source_ready_job["source_generation_ready"] = json!(true);
    source_ready_job["source_work_remaining"] = json!(false);
    let source_ready = pending(classify_exact_daemon_semantic_completion(
        &status("applied", selected, selected),
        Some(&source_ready_job),
        &target(),
    ));
    assert!(!source_ready.substantively_advances_from(Some(&job_active)));
}

#[test]
fn sequence_regression_with_8_to_16_to_8_chunk_churn_is_not_progress() {
    let selected = "https://semantic.example.test/";
    let mut first_job = job("budget_exhausted");
    first_job["indexed_chunks"] = json!(8);
    first_job["semantic_progress_sequence"] = json!(8);
    let first = pending(classify_exact_daemon_semantic_completion(
        &status("applied", selected, selected),
        Some(&first_job),
        &target(),
    ));
    let mut advanced_job = job("budget_exhausted");
    advanced_job["indexed_chunks"] = json!(16);
    advanced_job["semantic_progress_sequence"] = json!(16);
    let advanced = pending(classify_exact_daemon_semantic_completion(
        &status("applied", selected, selected),
        Some(&advanced_job),
        &target(),
    ));
    assert!(advanced.substantively_advances_from(Some(&first)));
    let mut regressed_job = job("budget_exhausted");
    regressed_job["indexed_chunks"] = json!(8);
    regressed_job["semantic_progress_sequence"] = json!(8);
    let regressed = pending(classify_exact_daemon_semantic_completion(
        &status("applied", selected, selected),
        Some(&regressed_job),
        &target(),
    ));
    assert!(!regressed.substantively_advances_from(Some(&advanced)));
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
