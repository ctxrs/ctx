use super::*;

#[test]
fn e5_embedding_text_uses_query_and_passage_prefixes_once() {
    assert_eq!(
        semantic_e5_query_text_value("find a daemon failure"),
        "query: find a daemon failure"
    );
    assert_eq!(
        semantic_e5_query_text_value("  query: find a daemon failure"),
        "query: find a daemon failure"
    );
    assert_eq!(
        semantic_e5_passage_text("daemon failed to restart"),
        "passage: daemon failed to restart"
    );
    assert_eq!(
        semantic_e5_passage_text("  passage: daemon failed to restart"),
        "passage: daemon failed to restart"
    );
}

#[cfg(ctx_semantic_fastembed)]
#[test]
fn fixed_shape_settings_are_strict() {
    assert_eq!(semantic_fixed_shape_from_values(None, None).unwrap(), None);
    assert_eq!(
        semantic_fixed_shape_from_values(Some("16"), Some("512")).unwrap(),
        Some((16, 512))
    );
    for values in [
        (Some("16"), None),
        (None, Some("512")),
        (Some("0"), Some("512")),
        (Some("wat"), Some("512")),
        (Some("16"), Some("-1")),
    ] {
        assert!(semantic_fixed_shape_from_values(values.0, values.1).is_err());
    }
}

#[cfg(ctx_semantic_fastembed)]
#[test]
fn fixed_batch_padding_preserves_complete_batches() -> Result<()> {
    let make = |count| {
        (0..count)
            .map(|index| format!("passage: {index}"))
            .collect::<Vec<_>>()
    };
    assert!(pad_texts_to_exact_batch(make(0), 4)?.is_empty());
    assert_eq!(pad_texts_to_exact_batch(make(4), 4)?.len(), 4);
    let padded = pad_texts_to_exact_batch(make(5), 4)?;
    assert_eq!(padded.len(), 8);
    assert_eq!(&padded[..5], make(5));
    assert!(padded[5..]
        .iter()
        .all(|text| text == SEMANTIC_PASSAGE_PREFIX));
    assert!(pad_texts_to_exact_batch(make(1), 0).is_err());
    Ok(())
}

#[test]
fn daemon_job_json_keeps_outcomes_without_live_worker_snapshots() {
    let job = daemon_semantic_job_json("budget_exhausted", None, 1234, Some(7), None);

    assert_eq!(job["status"], "budget_exhausted");
    assert_eq!(job["indexed_chunks"], 7);
    for field in [
        "enabled",
        "model_cache_available",
        "model_acquisition",
        "embed_policy",
        "embedding_runtime",
        "worker_status",
        "coverage",
    ] {
        assert!(
            job.get(field).is_none(),
            "unexpected live snapshot: {field}"
        );
    }
}

#[test]
fn daemon_acquisition_failure_is_explicit_retryable_and_fail_closed() -> Result<()> {
    let temp = tempfile::tempdir()?;

    let startup = run_daemon_semantic_model_startup_with(
        1234,
        || Err(anyhow!("signed model input unavailable")),
        |_| -> Result<SemanticDaemonModelAcquisition> {
            unreachable!("failed initial acquisition must not request CPU fallback")
        },
        |_| -> Result<()> { unreachable!("failed acquisition must never initialize the runtime") },
    )?;
    let DaemonSemanticModelStartup::Finished(job) = startup else {
        panic!("failed acquisition must stop daemon model startup");
    };
    assert_eq!(job["status"], "skipped");
    assert_eq!(job["reason"], "model_acquisition_failed");

    let mut backoff = DaemonRetryBackoff::default();
    let job = record_daemon_job_retry(&mut backoff, job);
    assert_eq!(job["failure_class"], "retryable");
    assert_eq!(job["retryable"], true);
    assert!(job["retry_after_ms"]
        .as_u64()
        .is_some_and(|delay| delay > 0));
    assert!(
        !source_backed_semantic_vector_path(temp.path()).exists(),
        "failed model acquisition must not claim a semantic projection"
    );
    Ok(())
}

#[test]
fn restored_daemon_retry_deadline_is_clamped_to_runtime_maximum() {
    let now_ms = utc_now().timestamp_millis();
    let persisted = json!({
        "consecutive_failures": 99,
        "retry_not_before_at_ms": now_ms + 24 * 60 * 60 * 1_000,
    });
    let mut backoff = DaemonRetryBackoff::default();
    backoff.restore(Some(&persisted));

    let maximum_ms = DaemonRetryBackoff::MAX_DELAY.as_millis() as u64;
    assert!(
        backoff
            .retry_after_ms()
            .is_some_and(|remaining| remaining <= maximum_ms),
        "{backoff:#?}"
    );
    assert!(
        backoff.retry_not_before_at_ms.is_some_and(|deadline| {
            deadline > now_ms && deadline <= now_ms + maximum_ms as i64
        }),
        "{backoff:#?}"
    );
    assert_eq!(backoff.consecutive_failures, 99);
}

#[cfg(ctx_semantic_fastembed)]
#[test]
fn verified_cache_missing_runtime_reports_model_load_failed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let cache_dir = temp.path().join("semantic-model-cache");
    write_test_semantic_cache(&cache_dir)?;
    let missing_runtime = temp.path().join("missing-libonnxruntime.so");

    let startup = run_daemon_semantic_model_startup_with(
        1234,
        || Ok(SemanticDaemonModelAcquisition::verified_cpu_cache_for_test()),
        |_| -> Result<SemanticDaemonModelAcquisition> {
            unreachable!("CPU runtime load failure must not request Core ML fallback")
        },
        |_| -> Result<()> {
            load_missing_semantic_onnxruntime_for_test(&cache_dir, &missing_runtime)?;
            unreachable!("missing explicit runtime must fail deterministically")
        },
    )?;
    let DaemonSemanticModelStartup::Finished(job) = startup else {
        panic!("missing ONNX Runtime must stop daemon model startup");
    };
    assert_eq!(job["status"], "skipped");
    assert_eq!(job["reason"], "model_load_failed");
    assert_eq!(job["failure_class"], "retryable");
    assert!(job["last_error"]
        .as_str()
        .is_some_and(|message| message.contains("failed to load ONNX Runtime")));
    assert!(semantic_model_cache_available(&cache_dir));
    Ok(())
}

#[cfg(ctx_semantic_fastembed)]
#[test]
fn auto_coreml_load_failure_acquires_cpu_and_preserves_fallback_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    write_semantic_enabled_config(temp.path())?;
    let cpu_cache = temp.path().join("semantic-model-cache");
    assert!(!cpu_cache.exists(), "CPU fallback cache must start empty");
    let cpu_acquired = std::cell::Cell::new(false);
    let load_attempts = std::cell::Cell::new(0_u8);

    let startup = run_daemon_semantic_model_startup_with(
        1234,
        || Ok(SemanticDaemonModelAcquisition::verified_coreml_cache_for_test()),
        |fallback| {
            assert_eq!(fallback, "coreml_load_error");
            assert!(
                !cpu_cache.exists(),
                "forced Core ML load failure must precede CPU acquisition"
            );
            fs::create_dir_all(cpu_cache.join("daemon-authorized-cpu-acquisition"))?;
            cpu_acquired.set(true);
            Ok(SemanticDaemonModelAcquisition::downloaded_cpu_fallback_for_test(fallback))
        },
        |acquisition| {
            load_attempts.set(load_attempts.get() + 1);
            if acquisition.fallback().is_none() {
                return Err(map_daemon_coreml_load_error(
                    acquisition,
                    anyhow!("forced Core ML runtime load failure"),
                ));
            }
            assert!(
                cpu_acquired.get(),
                "cache-only CPU load must follow daemon-authorized acquisition"
            );
            assert_eq!(acquisition.source(), "download");
            assert_eq!(acquisition.fallback(), Some("coreml_load_error"));
            Ok(())
        },
    )?;

    assert!(matches!(startup, DaemonSemanticModelStartup::Loaded));
    assert!(cpu_acquired.get());
    assert_eq!(load_attempts.get(), 2);
    Ok(())
}

#[test]
fn semantic_failure_classes_control_retry_backoff() {
    let retryable = anyhow!("transient flat segment publication failure");
    assert_eq!(
        classify_semantic_failure(&retryable),
        SemanticFailureClass::Retryable
    );
    let permanent = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied").into();
    assert_eq!(
        classify_semantic_failure(&permanent),
        SemanticFailureClass::Permanent
    );
    let corrupt: anyhow::Error = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        None,
    )
    .into();
    assert_eq!(
        classify_semantic_failure(&corrupt),
        SemanticFailureClass::CorruptSidecar
    );
    for typed in [
        SemanticVectorStoreError::storage_conflict("sidecar identity changed"),
        SemanticVectorStoreError::newer_schema(SEMANTIC_VECTOR_SCHEMA_VERSION + 1),
    ] {
        assert_eq!(
            classify_semantic_failure(&anyhow::Error::new(typed)),
            SemanticFailureClass::Permanent
        );
    }
    assert_eq!(
        classify_semantic_failure(&anyhow::Error::new(
            SemanticVectorStoreError::reset_required("sidecar reset required")
        )),
        SemanticFailureClass::CorruptSidecar
    );
    assert_eq!(
        classify_semantic_failure(&anyhow::Error::new(SemanticVectorStoreError::unavailable(
            "flat segment store temporarily unavailable"
        ))),
        SemanticFailureClass::Retryable
    );
    let pressure = SemanticModelLoadDeferred {
        available_memory_bytes: 1,
        required_available_memory_bytes: 2,
    };
    assert_eq!(
        classify_semantic_failure(&anyhow::Error::new(pressure)),
        SemanticFailureClass::ResourcePressure
    );

    for (class, should_backoff) in [
        (SemanticFailureClass::Retryable, true),
        (SemanticFailureClass::Permanent, false),
        (SemanticFailureClass::CorruptSidecar, false),
        (SemanticFailureClass::ResourcePressure, false),
    ] {
        let job = annotate_semantic_failure(
            daemon_semantic_job_json("failed", None, 1234, None, Some("failure".to_owned())),
            class,
        );
        assert_eq!(daemon_job_should_backoff(&job), should_backoff);
    }
}

#[cfg(ctx_semantic_fastembed)]
#[test]
fn daemon_retry_backoff_is_capped() {
    let mut backoff = DaemonRetryBackoff::default();
    let mut last = StdDuration::ZERO;
    for _ in 0..40 {
        let delay = backoff.record_failure();
        assert!(delay >= last);
        assert!(delay <= DaemonRetryBackoff::MAX_DELAY);
        last = delay;
    }
    assert_eq!(last, DaemonRetryBackoff::MAX_DELAY);
    assert!(!backoff.ready());
    assert!(backoff.retry_after_ms().is_some_and(|delay| delay > 0));
    let persisted = json!({
        "consecutive_failures": backoff.consecutive_failures,
        "retry_not_before_at_ms": backoff.retry_not_before_at_ms,
    });
    let mut restarted = DaemonRetryBackoff::default();
    restarted.restore(Some(&persisted));
    assert!(!restarted.ready(), "restart must preserve watcher backoff");
    backoff.reset();
    assert!(backoff.ready());
}

#[test]
fn foreground_query_preempts_daemon_background_jobs() -> Result<()> {
    let temp = tempfile::tempdir()?;
    write_semantic_enabled_config(temp.path())?;
    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let _hooks = install_test_daemon_jobs(
        calls.clone(),
        Some(daemon_history_completed_test_job()),
        Some(daemon_semantic_indexed_test_job(temp.path())),
    );
    let activity = Arc::new(DaemonQueryActivity::new());
    let _request = activity
        .begin_request()
        .expect("test foreground query should be accepted");
    let mut runtime = DaemonRuntime::default();

    let iteration = run_daemon_scheduler_cycle_with_activity(
        &test_daemon_run_args(),
        temp.path(),
        &mut runtime,
        None,
        true,
        Some(activity.as_ref()),
        None,
    )?;

    assert!(!iteration.did_work);
    assert!(!iteration.failed);
    assert!(calls.borrow().is_empty());
    assert!(!daemon_source_backed_refresh_job_path(temp.path()).exists());
    Ok(())
}
