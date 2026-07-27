use super::*;

#[test]
fn deadline_partial_batch_keeps_only_fully_embedded_events() {
    let first = Uuid::new_v4();
    let split = Uuid::new_v4();
    let last = Uuid::new_v4();
    let pending = vec![
        test_chunk_at(first, 1, "first", 0, 1),
        test_chunk_at(split, 2, "split", 0, 3),
        test_chunk_at(split, 2, "split", 1, 3),
        test_chunk_at(split, 2, "split", 2, 3),
        test_chunk_at(last, 3, "last", 0, 1),
    ];

    assert_eq!(semantic_complete_embedding_prefix(&pending, 0), 0);
    assert_eq!(semantic_complete_embedding_prefix(&pending, 1), 1);
    assert_eq!(semantic_complete_embedding_prefix(&pending, 2), 1);
    assert_eq!(semantic_complete_embedding_prefix(&pending, 3), 1);
    assert_eq!(semantic_complete_embedding_prefix(&pending, 4), 4);
    assert_eq!(semantic_complete_embedding_prefix(&pending, 5), 5);
    assert_eq!(semantic_complete_embedding_prefix(&pending, 99), 5);

    let considered = vec![first, split, last];
    assert_eq!(
        semantic_contiguous_consumed_event_ids(&considered, &[first, last]),
        vec![first]
    );
    assert_eq!(
        semantic_contiguous_consumed_event_ids(&considered, &[first, split, last]),
        considered
    );

    let cursors = vec![(first, (30, 3)), (split, (10, 2)), (last, (20, 1))];
    assert_eq!(
        semantic_consumed_page_anchor_cursor(&cursors, &[first, last]),
        None,
        "a bounded activity-order prefix cannot advance past any unfinished page member"
    );
    assert_eq!(
        semantic_consumed_page_anchor_cursor(&cursors, &[first, split, last]),
        Some((10, 2)),
        "a fully consumed activity-ordered page advances at its oldest anchor"
    );
}

#[test]
fn semantic_backfill_cursor_waits_for_a_full_reordered_activity_page() -> Result<()> {
    let temp = tempfile::tempdir()?;
    write_late_activity_searchable_store(temp.path(), SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT + 1)?;
    let store = Store::open(database_path(temp.path().to_path_buf()))?;
    let first = store.recent_event_embedding_documents(None, SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT)?;
    assert_eq!(first.len(), SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT);
    let page_cursors = first
        .iter()
        .map(|doc| (doc.event_id, (doc.anchor_occurred_at_ms, doc.seq)))
        .collect::<Vec<_>>();
    let bounded_prefix = first
        .iter()
        .take(SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT / 2)
        .map(|doc| doc.event_id)
        .collect::<Vec<_>>();
    assert_eq!(
        semantic_consumed_page_anchor_cursor(&page_cursors, &bounded_prefix),
        None,
        "a bounded prefix must leave the persisted frontier unchanged"
    );

    let fully_consumed = first.iter().map(|doc| doc.event_id).collect::<Vec<_>>();
    let boundary = semantic_consumed_page_anchor_cursor(&page_cursors, &fully_consumed)
        .expect("a full page has an anchor boundary");
    let second = store
        .recent_event_embedding_documents(Some(boundary), SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT)?;
    assert_eq!(second.len(), 1);
    assert!(!first.iter().any(|doc| doc.event_id == second[0].event_id));
    Ok(())
}

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
fn semantic_incremental_slice_requires_previous_ready_status() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let stats = SemanticSidecarStats {
        embedded_items: 10,
        embedded_chunks: 20,
    };
    assert!(!semantic_worker_status_was_ready_for_stats(
        temp.path(),
        stats
    ));

    write_semantic_worker_status(
        temp.path(),
        &json!({
            "schema_version": 1,
            "status": "completed",
            "model_key": semantic_model_key(),
            "searchable_items": 11,
            "embedded_items": 10,
            "embedded_chunks": 20,
            "dirty_items": 0,
        }),
    )?;
    assert!(!semantic_worker_status_was_ready_for_stats(
        temp.path(),
        stats
    ));

    write_semantic_worker_status(
        temp.path(),
        &json!({
            "schema_version": 1,
            "status": "ready",
            "model_key": semantic_model_key(),
            "searchable_items": 10,
            "embedded_items": 10,
            "embedded_chunks": 20,
            "dirty_items": 0,
        }),
    )?;
    assert!(semantic_worker_status_was_ready_for_stats(
        temp.path(),
        stats
    ));
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
        !semantic_vector_path(temp.path()).exists(),
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
    let retryable = anyhow!("transient sqlite-vec registration failure");
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
            "sqlite-vec temporarily unavailable"
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
fn daemon_semantic_status_ignores_job_from_old_model_key() -> Result<()> {
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

    let daemon = daemon_report(
        temp.path(),
        &semantic_worker_report_best_effort(temp.path()),
    );
    let semantic = &daemon["jobs"]["semantic_index"];
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
fn accelerator_only_signed_cache_admits_index_queue() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let cache = temp.path().join("semantic-model-cache");
    write_test_semantic_cache_variant(
        &cache.join(SEMANTIC_MANAGED_MODEL_CACHE_DIR),
        SemanticOrtModelVariant::AcceleratorO4Fp16,
    )?;
    assert!(semantic_model_cache_snapshot_dir(&cache).is_none());
    assert!(semantic_model_cache_available(&cache));

    write_searchable_store(temp.path(), 1)?;
    let store = Store::open(database_path(temp.path().to_path_buf()))?;
    assert_eq!(
        queue_recent_semantic_work(temp.path(), &store, "accelerator_signed_cache")?,
        1
    );
    assert!(semantic_vector_path(temp.path()).is_file());
    Ok(())
}

#[test]
fn daemon_recent_queue_marks_user_anchor_dirty_when_assistant_changes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path();
    let store = Store::open(database_path(data_root.to_path_buf()))?;
    let session_id = Uuid::new_v4();
    insert_test_session(&store, session_id)?;
    let user = test_session_message(1, session_id, EventRole::User, "semantic anchor prompt");
    let assistant = test_session_message(
        2,
        session_id,
        EventRole::Assistant,
        "original assistant answer",
    );
    store.upsert_event(&user)?;
    store.upsert_event(&assistant)?;
    store.refresh_event_embedding_document_count_cache()?;
    let docs = store.event_embedding_documents_by_ids(&[user.id])?;
    let doc = docs.first().expect("user lite-turn document");
    let source_text = semantic_source_text(&doc.text);
    let source_hash = semantic_document_hash(doc, &source_text);

    let vector_path = semantic_vector_path(data_root);
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    vector_store.upsert_chunk_embeddings(&[(
        test_chunk(user.id, user.seq, &source_hash),
        test_embedding(1.0, 0.0),
    )])?;
    assert_eq!(vector_store.dirty_event_count()?, 0);
    drop(vector_store);

    let mut updated_assistant = assistant.clone();
    updated_assistant.payload = json!({ "text": "updated assistant answer" });
    updated_assistant.occurred_at = utc_now();
    store.upsert_event(&updated_assistant)?;

    assert_eq!(
        queue_recent_semantic_work(data_root, &store, "test_recent")?,
        1
    );
    let vector_store = SemanticVectorStore::open(&vector_path)?;
    assert_eq!(vector_store.queued_dirty_event_ids(10)?, vec![user.id]);
    Ok(())
}

#[test]
fn daemon_restart_reconciles_commit_that_missed_semantic_handoff() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path();
    let store = Store::open(database_path(data_root.to_path_buf()))?;
    let session_id = Uuid::new_v4();
    insert_test_session(&store, session_id)?;
    let user = test_session_message(1, session_id, EventRole::User, "restart anchor prompt");
    let assistant = test_session_message(
        2,
        session_id,
        EventRole::Assistant,
        "answer committed before crash",
    );
    store.upsert_event(&user)?;
    store.upsert_event(&assistant)?;
    let docs = store.event_embedding_documents_by_ids(&[user.id])?;
    let doc = docs.first().expect("user lite-turn document");
    let source_text = semantic_source_text(&doc.text);
    let source_hash = semantic_document_hash(doc, &source_text);
    let vector_path = semantic_vector_path(data_root);
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    vector_store.upsert_chunk_embeddings(&[(
        test_chunk(user.id, user.seq, &source_hash),
        test_embedding(1.0, 0.0),
    )])?;
    drop(vector_store);

    let mut updated_assistant = assistant;
    updated_assistant.payload = json!({ "text": "committed update with no dirty handoff" });
    store.upsert_event(&updated_assistant)?;
    drop(store);

    let restarted_store = Store::open(database_path(data_root.to_path_buf()))?;
    reconcile_committed_semantic_work(data_root, &restarted_store)?;
    let vector_store = SemanticVectorStore::open(&vector_path)?;
    assert_eq!(vector_store.queued_dirty_event_ids(10)?, vec![user.id]);
    Ok(())
}

#[test]
fn completed_reconciliation_rearms_for_second_store_assistant_append() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = database_path(temp.path().to_path_buf());
    let store = Store::open(&store_path)?;
    let session_id = Uuid::new_v4();
    insert_test_session(&store, session_id)?;
    let user = test_session_message(1, session_id, EventRole::User, "paired user A");
    store.upsert_event(&user)?;
    let document = store
        .event_embedding_documents_by_ids(&[user.id])?
        .pop()
        .expect("searchable user anchor");
    let source = semantic_source_text(&document.text);
    let source_hash = semantic_document_hash(&document, &source);
    let vector_path = semantic_vector_path(temp.path());
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    vector_store.upsert_chunk_embeddings(&[(
        test_chunk(user.id, user.seq, &source_hash),
        test_embedding(1.0, 0.0),
    )])?;
    drop(vector_store);

    let mut sweep = SemanticReconciliationSweepState::default();
    let initial = reconcile_committed_semantic_work_with_state(temp.path(), &store, &mut sweep)?;
    assert!(!initial.work_remaining);
    let completed_version = store.canonical_semantic_projection_version()?;
    assert_eq!(
        SemanticVectorStore::open(&vector_path)?.reconciled_store_version()?,
        Some(completed_version)
    );

    let second_writer = Store::open(&store_path)?;
    second_writer.upsert_event(&test_session_message(
        2,
        session_id,
        EventRole::Assistant,
        "late paired assistant B",
    ))?;
    let appended_version = store.canonical_semantic_projection_version()?;
    assert!(appended_version.mutation_epoch > completed_version.mutation_epoch);

    let rearmed = reconcile_committed_semantic_work_with_state(temp.path(), &store, &mut sweep)?;
    assert!(!rearmed.work_remaining);
    assert!(rearmed.deleted_chunks > 0);
    let vector_store = SemanticVectorStore::open(&vector_path)?;
    assert_eq!(
        vector_store.reconciled_store_version()?,
        Some(appended_version)
    );
    assert_eq!(vector_store.queued_dirty_event_ids(10)?, vec![user.id]);
    assert!(vector_store
        .existing_hashes_for_event_ids(&[user.id])?
        .is_empty());
    Ok(())
}

#[test]
fn redaction_and_deletion_rearm_and_prune_stale_vectors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = database_path(temp.path().to_path_buf());
    let store = Store::open(&store_path)?;
    let redacted_event = test_searchable_event(1);
    let deleted_event = test_searchable_event(2);
    store.upsert_event(&redacted_event)?;
    store.upsert_event(&deleted_event)?;
    let documents =
        store.event_embedding_documents_by_ids(&[redacted_event.id, deleted_event.id])?;
    let chunks = documents
        .iter()
        .map(|document| {
            let source = semantic_source_text(&document.text);
            let source_hash = semantic_document_hash(document, &source);
            (
                test_chunk(document.event_id, document.seq, &source_hash),
                test_embedding(1.0, 0.0),
            )
        })
        .collect::<Vec<_>>();
    let vector_path = semantic_vector_path(temp.path());
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    vector_store.upsert_chunk_embeddings(&chunks)?;
    drop(vector_store);

    let mut sweep = SemanticReconciliationSweepState::default();
    reconcile_committed_semantic_work_with_state(temp.path(), &store, &mut sweep)?;
    let before = store.canonical_semantic_projection_version()?;

    let second_writer = Store::open(&store_path)?;
    let mut redacted = second_writer.get_event(redacted_event.id)?;
    redacted.payload = json!({ "text": "[redacted]" });
    second_writer.upsert_event(&redacted)?;
    let mut deleted = second_writer.get_event(deleted_event.id)?;
    deleted.sync.deleted_at = Some(utc_now());
    second_writer.upsert_event(&deleted)?;
    let after = store.canonical_semantic_projection_version()?;
    assert!(after.mutation_epoch > before.mutation_epoch);

    let outcome = reconcile_committed_semantic_work_with_state(temp.path(), &store, &mut sweep)?;
    assert!(!outcome.work_remaining);
    assert_eq!(outcome.deleted_chunks, 2);
    let vector_store = SemanticVectorStore::open(&vector_path)?;
    assert_eq!(vector_store.reconciled_store_version()?, Some(after));
    assert_eq!(vector_store.cached_or_exact_stats()?.embedded_items, 0);
    assert_eq!(
        vector_store.queued_dirty_event_ids(10)?,
        vec![redacted_event.id]
    );
    Ok(())
}

#[test]
fn mutation_during_multipage_sweep_finishes_then_runs_successor() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let document_count = SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT + 1;
    let documents = write_late_activity_searchable_store(temp.path(), document_count)?;
    let vector_path = semantic_vector_path(temp.path());
    drop(SemanticVectorStore::open(&vector_path)?);
    let store_path = database_path(temp.path().to_path_buf());
    let store = Store::open(&store_path)?;
    let mut sweep = SemanticReconciliationSweepState::default();

    let first = reconcile_committed_semantic_work_with_state(temp.path(), &store, &mut sweep)?;
    assert_eq!(
        first.committed_documents_scanned,
        SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT
    );
    assert!(first.work_remaining);
    let first_target = sweep.target_version.expect("first sweep target");

    let second_writer = Store::open(&store_path)?;
    let mut changed = second_writer.get_event(documents[0].event_id)?;
    changed.payload = json!({ "text": "mutated after the first reconciliation page" });
    second_writer.upsert_event(&changed)?;
    let changed_version = store.canonical_semantic_projection_version()?;
    assert!(changed_version.mutation_epoch > first_target.mutation_epoch);

    let finished_original =
        reconcile_committed_semantic_work_with_state(temp.path(), &store, &mut sweep)?;
    assert_eq!(finished_original.committed_documents_scanned, 1);
    assert!(finished_original.work_remaining);
    assert_eq!(sweep.target_version, Some(changed_version));
    assert_ne!(
        SemanticVectorStore::open(&vector_path)?.reconciled_store_version()?,
        Some(changed_version)
    );

    let successor_first =
        reconcile_committed_semantic_work_with_state(temp.path(), &store, &mut sweep)?;
    assert_eq!(
        successor_first.committed_documents_scanned,
        SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT
    );
    assert!(successor_first.work_remaining);

    let completed = reconcile_committed_semantic_work_with_state(temp.path(), &store, &mut sweep)?;
    assert_eq!(completed.committed_documents_scanned, 1);
    assert!(!completed.work_remaining);
    assert_eq!(
        SemanticVectorStore::open(&vector_path)?.reconciled_store_version()?,
        Some(changed_version)
    );
    Ok(())
}

#[test]
fn equal_epoch_store_replacement_rearms_reconciliation() -> Result<()> {
    let original_root = tempfile::tempdir()?;
    let replacement_root = tempfile::tempdir()?;
    let original_store = Store::open(database_path(original_root.path().to_path_buf()))?;
    let original_event = test_searchable_event(1);
    original_store.upsert_event(&original_event)?;
    let original_document = original_store
        .event_embedding_documents_by_ids(&[original_event.id])?
        .pop()
        .expect("original searchable event");
    let original_source = semantic_source_text(&original_document.text);
    let original_hash = semantic_document_hash(&original_document, &original_source);
    let vector_path = semantic_vector_path(original_root.path());
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    vector_store.upsert_chunk_embeddings(&[(
        test_chunk(original_event.id, original_event.seq, &original_hash),
        test_embedding(1.0, 0.0),
    )])?;
    drop(vector_store);

    let mut original_sweep = SemanticReconciliationSweepState::default();
    reconcile_committed_semantic_work_with_state(
        original_root.path(),
        &original_store,
        &mut original_sweep,
    )?;
    let original_version = original_store.canonical_semantic_projection_version()?;

    let replacement_store = Store::open(database_path(replacement_root.path().to_path_buf()))?;
    let mut replacement_event = test_searchable_event(1);
    replacement_event.payload = json!({ "text": "independent equal-epoch replacement" });
    replacement_store.upsert_event(&replacement_event)?;
    let replacement_version = replacement_store.canonical_semantic_projection_version()?;
    assert_eq!(
        replacement_version.mutation_epoch,
        original_version.mutation_epoch
    );
    assert_ne!(
        replacement_version.store_identity,
        original_version.store_identity
    );

    let mut restarted_sweep = SemanticReconciliationSweepState::default();
    let outcome = reconcile_committed_semantic_work_with_state(
        original_root.path(),
        &replacement_store,
        &mut restarted_sweep,
    )?;
    assert!(!outcome.work_remaining);
    assert_eq!(outcome.deleted_chunks, 1);
    let vector_store = SemanticVectorStore::open(&vector_path)?;
    assert_eq!(
        vector_store.reconciled_store_version()?,
        Some(replacement_version)
    );
    assert_eq!(
        vector_store.queued_dirty_event_ids(10)?,
        vec![replacement_event.id]
    );
    assert!(vector_store
        .existing_hashes_for_event_ids(&[original_event.id])?
        .is_empty());
    Ok(())
}

#[test]
fn daemon_restart_finds_old_store_event_beyond_reordered_activity_page() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let docs =
        write_late_activity_searchable_store(temp.path(), SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT + 1)?;
    let missing = docs
        .last()
        .expect("fixture must contain an event beyond the recent reconciliation page")
        .event_id;
    let mut embedded = Vec::with_capacity(docs.len().saturating_sub(1));
    for doc in docs.iter().filter(|doc| doc.event_id != missing) {
        let source_text = semantic_source_text(&doc.text);
        let source_hash = semantic_document_hash(doc, &source_text);
        embedded.push((
            test_chunk(doc.event_id, doc.seq, &source_hash),
            test_embedding(1.0, 0.0),
        ));
    }
    let vector_path = semantic_vector_path(temp.path());
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    vector_store.upsert_chunk_embeddings(&embedded)?;
    drop(vector_store);

    let store = Store::open(database_path(temp.path().to_path_buf()))?;
    reconcile_committed_semantic_work(temp.path(), &store)?;
    let vector_store = SemanticVectorStore::open(&vector_path)?;
    assert!(vector_store.queued_dirty_event_ids(10)?.is_empty());
    drop(vector_store);
    drop(store);

    let restarted_store = Store::open(database_path(temp.path().to_path_buf()))?;
    reconcile_committed_semantic_work(temp.path(), &restarted_store)?;
    let vector_store = SemanticVectorStore::open(&vector_path)?;
    assert_eq!(vector_store.queued_dirty_event_ids(10)?, vec![missing]);
    Ok(())
}

#[test]
fn daemon_scheduler_round_robins_sources_and_backoff_is_capped() {
    let mut cursor = 0;
    assert_eq!(daemon_take_next_source_index(&mut cursor, 3), Some(0));
    assert_eq!(daemon_take_next_source_index(&mut cursor, 3), Some(1));
    assert_eq!(daemon_take_next_source_index(&mut cursor, 3), Some(2));
    assert_eq!(daemon_take_next_source_index(&mut cursor, 3), Some(0));
    assert_eq!(daemon_take_next_source_index(&mut cursor, 0), None);

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
    )?;

    assert!(!iteration.did_work);
    assert!(!iteration.failed);
    assert!(calls.borrow().is_empty());
    let daemon = daemon_report(temp.path(), &semantic_worker_report_for_daemon(temp.path()));
    assert_eq!(
        daemon["jobs"]["history_refresh"]["reason"],
        "foreground_query"
    );
    assert_eq!(
        daemon["jobs"]["semantic_index"]["last_run_reason"],
        "foreground_query"
    );
    Ok(())
}

#[test]
fn semantic_only_search_does_not_reject_a_running_worker() -> Result<()> {
    let temp = tempfile::tempdir()?;
    write_test_semantic_cache(&temp.path().join("semantic-model-cache"))?;
    let docs = write_searchable_store(temp.path(), 1)?;
    let doc = docs.first().expect("searchable fixture doc");
    let source_text = semantic_source_text(&doc.text);
    let source_hash = semantic_document_hash(doc, &source_text);
    let mut vector_store = SemanticVectorStore::open(&semantic_vector_path(temp.path()))?;
    vector_store.upsert_chunk_embeddings(&[(
        test_chunk(doc.event_id, doc.seq, &source_hash),
        test_embedding(1.0, 0.0),
    )])?;
    drop(vector_store);

    let _lock = SemanticWorkerLock::acquire(temp.path())?
        .expect("test should acquire semantic worker lock");
    let store = Store::open(database_path(temp.path().to_path_buf()))?;
    let err = search_packet_with_backend(
        &store,
        temp.path(),
        "semantic daemon scheduling fixture",
        &[],
        &ctx_history_search::PacketOptions::default(),
        SearchBackendArg::Semantic,
        true,
        1.0,
        RefreshArg::Off,
        false,
    )
    .expect_err("fixture has no daemon query service");
    let message = format!("{err:#}");
    assert!(message.contains(&DaemonQueryServiceUnavailable.to_string()));
    assert!(!message.contains("semantic worker is currently indexing"));
    Ok(())
}
