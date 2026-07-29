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
fn semantic_worker_report_preserves_embed_policy_from_status() -> Result<()> {
    let temp = tempfile::tempdir()?;
    write_semantic_worker_status(
        temp.path(),
        &json!({
            "schema_version": 1,
            "status": "budget_exhausted",
            "model_key": semantic_model_key(),
            "pid": 1234,
            "searchable_items": 10,
            "embedded_items": 2,
            "embedded_chunks": 4,
            "dirty_items": 1,
            "embed_policy": {
                "source": "fixture",
                "threads": 7,
                "batch_size": 96,
                "memory_budget_bytes": 123,
            },
        }),
    )?;

    let report = semantic_worker_report_best_effort(temp.path()).to_json();
    assert_eq!(report["embed_policy"]["source"], "fixture");
    assert_eq!(report["embed_policy"]["threads"], 7);
    assert_eq!(report["coverage"]["embedded_chunks"], 4);
    Ok(())
}

#[test]
fn semantic_worker_report_ignores_status_from_old_model_key() -> Result<()> {
    let temp = tempfile::tempdir()?;
    write_semantic_worker_status(
        temp.path(),
        &json!({
            "schema_version": 1,
            "status": "ready",
            "model_key": "fastembed:old-model-key",
            "pid": 999,
            "last_error": "old failure",
            "searchable_items": 10,
            "embedded_items": 10,
            "embedded_chunks": 20,
            "dirty_items": 0,
            "embed_policy": {
                "source": "old-fixture"
            },
        }),
    )?;

    let report = semantic_worker_report_best_effort(temp.path()).to_json();
    assert_eq!(report["status"], "unknown");
    assert_eq!(report["pid"], Value::Null);
    assert_eq!(report["last_error"], Value::Null);
    assert_ne!(report["embed_policy"]["source"], "old-fixture");
    assert_eq!(report["coverage"]["searchable_items"], 0);
    assert_eq!(report["coverage"]["searchable_items_known"], false);
    assert_eq!(report["coverage"]["embedded_items"], 0);
    Ok(())
}

#[test]
fn ready_index_requests_daemon_model_load_with_or_without_cache() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut report = SemanticWorkerReport::unavailable(temp.path(), "test");
    report.status = "ready".to_owned();
    report.searchable_items = 10;
    report.searchable_items_known = true;
    report.embedded_items = 10;
    report.queued_items_estimate = 0;
    report.model_cache_available = false;
    report.embedding_runtime = Some(json!({
        "backend": "cpu",
        "compute_class": "cpu",
    }));

    assert!(semantic_daemon_model_load_needed(&report, false));
    assert!(!semantic_daemon_model_load_needed(&report, true));
    report.model_cache_available = true;
    assert!(semantic_daemon_model_load_needed(&report, false));
    let status = daemon_semantic_job_report(temp.path(), &report, true);
    assert_eq!(status["embedding_runtime"]["backend"], "cpu");
    assert_eq!(status["embedding_runtime"]["compute_class"], "cpu");
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
fn disabled_semantic_status_is_read_only_and_write_free() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = AppConfig::default();

    let worker = semantic_worker_report_best_effort(temp.path());
    let configured = semantic_worker_report_configured_json(&config, &worker);
    let daemon = daemon_report(temp.path(), &worker);

    assert_eq!(configured["status"], "disabled");
    assert_eq!(configured["reason"], "semantic_disabled");
    assert_eq!(daemon["jobs"]["semantic_index"]["status"], "disabled");
    assert!(fs::read_dir(temp.path())?.next().is_none());
    Ok(())
}

#[test]
fn daemon_status_reports_retryable_memory_deferral() -> Result<()> {
    let temp = tempfile::tempdir()?;
    write_semantic_enabled_config(temp.path())?;
    let mut report = SemanticWorkerReport::unavailable(temp.path(), "test");
    report.status = "model_load_deferred".to_owned();
    report.searchable_items = 10;
    report.searchable_items_known = true;
    report.queued_items_estimate = 10;
    write_daemon_job_status(
        &daemon_semantic_job_path(temp.path()),
        &compact_json(json!({
            "schema_version": 1,
            "model_key": semantic_model_key(),
            "status": "skipped",
            "reason": "memory_pressure",
            "retryable": true,
            "available_memory_bytes": 1_610_612_736_u64,
            "required_available_memory_bytes": 2_147_483_648_u64,
        })),
    )?;

    let value = daemon_semantic_job_report(temp.path(), &report, true);
    assert_eq!(value["status"], "skipped");
    assert_eq!(value["reason"], "memory_pressure");
    assert_eq!(value["worker_status"], "model_load_deferred");
    assert_eq!(value["retryable"], true);
    assert_eq!(value["available_memory_bytes"], 1_610_612_736_u64);
    assert_eq!(value["required_available_memory_bytes"], 2_147_483_648_u64);
    Ok(())
}

#[test]
fn daemon_acquisition_failure_is_explicit_retryable_and_fail_closed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    write_semantic_enabled_config(temp.path())?;
    write_searchable_store(temp.path(), 2)?;

    let startup = run_daemon_semantic_model_startup_with(
        temp.path(),
        1234,
        || Err(anyhow!("signed model input unavailable")),
        |_| -> Result<SemanticDaemonModelAcquisition> {
            unreachable!("failed initial acquisition must not request CPU fallback")
        },
        |_| -> Result<(Option<Value>, Value)> {
            unreachable!("failed acquisition must never initialize the runtime")
        },
    )?;
    let DaemonSemanticModelStartup::Finished(job) = startup else {
        panic!("failed acquisition must stop daemon model startup");
    };
    assert_eq!(job["status"], "skipped");
    assert_eq!(job["reason"], "model_acquisition_failed");

    let mut backoff = DaemonRetryBackoff::default();
    let job = record_daemon_job_retry(&mut backoff, job);
    write_daemon_job_status(&daemon_semantic_job_path(temp.path()), &job)?;

    let store = Store::open(database_path(temp.path().to_path_buf()))?;
    let report = semantic_worker_report(temp.path(), Some(&store))?;
    let value = daemon_semantic_job_report(temp.path(), &report, true);
    assert_eq!(value["status"], "skipped");
    assert_eq!(value["reason"], "model_acquisition_failed");
    assert_eq!(value["last_run_status"], "skipped");
    assert_eq!(value["last_run_reason"], "model_acquisition_failed");
    assert_eq!(value["failure_class"], "retryable");
    assert_eq!(value["retryable"], true);
    assert_eq!(value["model_cache_available"], false);
    assert!(value["retry_after_ms"].is_null());
    assert!(
        !source_backed_semantic_vector_path(temp.path()).exists(),
        "failed model acquisition must not claim a semantic projection"
    );
    Ok(())
}

#[cfg(ctx_semantic_fastembed)]
#[test]
fn verified_cache_missing_runtime_reports_model_load_failed_compatibly() -> Result<()> {
    let temp = tempfile::tempdir()?;
    write_semantic_enabled_config(temp.path())?;
    write_searchable_store(temp.path(), 1)?;
    let cache_dir = temp.path().join("semantic-model-cache");
    write_test_semantic_cache(&cache_dir)?;
    let missing_runtime = temp.path().join("missing-libonnxruntime.so");

    let startup = run_daemon_semantic_model_startup_with(
        temp.path(),
        1234,
        || {
            let status =
                read_semantic_worker_status(temp.path()).expect("acquisition phase status");
            assert_eq!(status["status"], "acquiring_model");
            Ok(SemanticDaemonModelAcquisition::verified_cpu_cache_for_test())
        },
        |_| -> Result<SemanticDaemonModelAcquisition> {
            unreachable!("CPU runtime load failure must not request Core ML fallback")
        },
        |_| -> Result<(Option<Value>, Value)> {
            let status = read_semantic_worker_status(temp.path()).expect("loading phase status");
            assert_eq!(status["status"], "loading_model");
            assert_eq!(
                status["model_acquisition"]["cpu"]["cache_status"],
                "present"
            );
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

    let mut backoff = DaemonRetryBackoff::default();
    let job = record_daemon_job_retry(&mut backoff, job);
    write_daemon_job_status(&daemon_semantic_job_path(temp.path()), &job)?;

    let worker_status =
        read_semantic_worker_status(temp.path()).expect("model load failure status");
    assert_eq!(worker_status["status"], "model_load_failed");
    assert_eq!(
        worker_status["model_acquisition"]["cpu"]["cache_status"], "present",
        "runtime initialization failure must preserve verified-cache state"
    );
    assert!(worker_status["last_error"]
        .as_str()
        .is_some_and(|message| message.contains("failed to load ONNX Runtime")));

    let store = Store::open(database_path(temp.path().to_path_buf()))?;
    let report = semantic_worker_report(temp.path(), Some(&store))?;
    let value = daemon_semantic_job_report(temp.path(), &report, true);
    assert_eq!(value["status"], "skipped");
    assert_eq!(value["reason"], "model_load_failed");
    assert_eq!(value["last_run_status"], "skipped");
    assert_eq!(value["last_run_reason"], "model_load_failed");
    assert_eq!(value["model_cache_available"], true);
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
        temp.path(),
        1234,
        || {
            let status = read_semantic_worker_status(temp.path()).expect("Core ML acquire status");
            assert_eq!(status["status"], "acquiring_model");
            Ok(SemanticDaemonModelAcquisition::verified_coreml_cache_for_test())
        },
        |fallback| {
            assert_eq!(fallback, "coreml_load_error");
            let status = read_semantic_worker_status(temp.path()).expect("CPU acquire status");
            assert_eq!(status["status"], "acquiring_model");
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
            let status = read_semantic_worker_status(temp.path()).expect("model loading status");
            assert_eq!(status["status"], "loading_model");
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
            Ok((
                Some(json!({
                    "backend": "cpu",
                    "acquisition_source": acquisition.source(),
                    "acquisition_fallback": acquisition.fallback(),
                })),
                json!({"compute_class": "cpu"}),
            ))
        },
    )?;

    assert!(matches!(startup, DaemonSemanticModelStartup::Loaded));
    assert!(cpu_acquired.get());
    assert_eq!(load_attempts.get(), 2);
    let status = read_semantic_worker_status(temp.path()).expect("model loaded status");
    assert_eq!(status["status"], "model_loaded");
    assert_eq!(status["embedding_runtime"]["backend"], "cpu");
    assert_eq!(
        status["embedding_runtime"]["acquisition_source"],
        "download"
    );
    assert_eq!(
        status["embedding_runtime"]["acquisition_fallback"],
        "coreml_load_error"
    );
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

#[test]
fn legacy_semantic_job_report_ignores_job_from_old_model_key() -> Result<()> {
    let temp = tempfile::tempdir()?;
    write_semantic_enabled_config(temp.path())?;
    write_daemon_job_status(
        &daemon_semantic_job_path(temp.path()),
        &json!({
            "schema_version": 1,
            "status": "ready",
            "model_key": "fastembed:old-model-key",
            "last_run_at_ms": 1234,
            "indexed_chunks": 99,
        }),
    )?;

    let semantic = daemon_semantic_job_report(
        temp.path(),
        &semantic_worker_report_best_effort(temp.path()),
        true,
    );
    assert_eq!(semantic["status"], "unknown");
    assert_eq!(semantic["reason"], "searchable_items_unknown");
    assert_eq!(semantic["last_run_status"], Value::Null);
    assert_eq!(semantic["indexed_chunks"], Value::Null);
    Ok(())
}

#[test]
fn hybrid_semantic_readiness_requires_complete_coverage() {
    assert!(semantic_hybrid_coverage_ready(0, 0, 0));
    assert!(semantic_hybrid_coverage_ready(10, 10, 0));
    assert!(semantic_hybrid_coverage_ready(11, 10, 0));

    assert!(!semantic_hybrid_coverage_ready(0, 10, 0));
    assert!(!semantic_hybrid_coverage_ready(1_000, 100_000, 0));
    assert!(!semantic_hybrid_coverage_ready(99_999, 100_000, 0));
    assert!(!semantic_hybrid_coverage_ready(10, 10, 1));
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

    let iteration = run_daemon_once_with_activity(
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
