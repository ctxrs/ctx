use super::*;

#[test]
fn production_jsonl_scheduler_projects_multiple_sources_concurrently() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    for index in 0..8 {
        fs::write(
            root.join(format!("{index}.jsonl")),
            b"{\"message\":\"parallel\"}\n",
        )
        .unwrap();
    }
    let adapter = ParallelTestAdapter;
    let resident = Mutex::new(FamilyResident::default());
    let mut writer = match IndexCaptureLifecycle::open(&temp.path().join("index"), ()).unwrap() {
        CaptureLifecycleOpenOutcome::Ready(lifecycle) => lifecycle,
        CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => {
            panic!("test lifecycle unexpectedly requires recovery")
        }
    };
    let mut owners = HashMap::new();
    let mut complete_inventories = Vec::new();
    let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
    let mut record_rejections = SourceBackedRecordRejections::default();
    let mut applied_removals = Vec::new();
    let mut sink = SourceBackedGenerationSink::new(
        &mut writer,
        &mut owners,
        &mut complete_inventories,
        &mut applied_removals,
        0,
        test_route_identity(),
        None,
        SourceBackedRouteResources::production(4),
        &mut logical_source_failures,
        &mut record_rejections,
        None,
        None,
        None,
    );

    with_family_scanner_workers(4, || {
        capture(&adapter, &root, &resident, &mut sink).unwrap();
    });

    assert_eq!(
        jsonl_family_scanner_activity(),
        JsonlFamilyScannerActivity {
            worker_count: 4,
            sources_started: 8,
            sources_completed: 8,
            peak_active_scanners: 4,
        },
        "the production JSONL route must keep all four selected scanners active"
    );
    assert_eq!(resident.lock().unwrap().terminal_sources.len(), 8);
}

#[test]
fn parallel_all_rejected_candidates_preserve_cold_and_warm_source_semantics() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    for index in 0..2 {
        fs::write(
            root.join(format!("{index}.jsonl")),
            b"{\"message\":\"valid\"}\n",
        )
        .unwrap();
    }
    let reject = Arc::new(AtomicBool::new(false));
    let adapter = AllRejectedParallelTestAdapter {
        reject: Arc::clone(&reject),
    };
    let index = temp.path().join("index");

    let cold = capture_parallel_test_generation(&adapter, &root, &index, 2).0;
    assert_eq!(cold.manifest.sources.len(), 2);
    assert_eq!(cold.manifest.records.len(), 2);

    reject.store(true, Ordering::SeqCst);
    for leaf in 0..2 {
        fs::write(
            root.join(format!("{leaf}.jsonl")),
            format!("{{\"message\":\"rejected-{leaf}\"}}\n"),
        )
        .unwrap();
    }
    let carried = capture_parallel_test_generation(&adapter, &root, &index, 2).0;
    assert_eq!(carried.manifest, cold.manifest);
    assert_eq!(carried.generation_id, cold.generation_id);

    let cold_rejected_index = temp.path().join("cold-rejected-index");
    let cold_rejected =
        capture_parallel_test_generation(&adapter, &root, &cold_rejected_index, 2).0;
    assert!(cold_rejected.manifest.records.is_empty());
    assert_eq!(cold_rejected.manifest.sources.len(), 2);
    assert!(cold_rejected.manifest.sources.iter().all(|source| {
        let counts = source.counts();
        counts.complete_records == 1 && counts.retained_records == 0 && counts.rejected_records == 1
    }));
}

#[test]
fn parallel_all_ignored_pages_publish_bytes_before_terminal_reconciliation() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let payloads = [
        b"{\"message\":\"ignored-a\"}\n".as_slice(),
        b"{\"message\":\"ignored-b\"}\n".as_slice(),
    ];
    for (index, payload) in payloads.iter().enumerate() {
        fs::write(root.join(format!("{index}.jsonl")), payload).unwrap();
    }
    let expected_bytes = payloads
        .iter()
        .map(|payload| u64::try_from(payload.len()).unwrap())
        .sum::<u64>();
    let adapter = DirectAppendTestAdapter::default();
    let resident = Mutex::new(FamilyResident::default());
    let mut writer = match IndexCaptureLifecycle::open(&temp.path().join("index"), ()).unwrap() {
        CaptureLifecycleOpenOutcome::Ready(lifecycle) => lifecycle,
        CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => {
            panic!("test lifecycle unexpectedly requires recovery")
        }
    };
    let shared_history = ctx_history_capture_model::SharedAttemptHistoryProgress::default();
    let callback_history = shared_history.clone();
    let mut coordinator_bytes = 0_u64;
    let mut accepted_records = 0_u64;
    let mut observed_ignored_progress = false;
    let mut owners = HashMap::new();
    let mut complete_inventories = Vec::new();
    let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
    let mut record_rejections = SourceBackedRecordRejections::default();
    let mut applied_removals = Vec::new();
    let resources = SourceBackedRouteResources::production(2)
        .with_attempt_history_progress(shared_history.clone());
    {
        let mut report_record_progress =
            |delta: ctx_history_capture_model::SourceBackedRecordProgressDelta| -> std::result::Result<
                (),
                ctx_history_capture_runtime::SourceBackedCoordinatorError<CaptureError>,
            > {
                if delta.completed_bytes != 0 {
                    let before = callback_history.snapshot();
                    assert!(
                        before.processed_bytes
                            >= coordinator_bytes.saturating_add(delta.completed_bytes),
                        "ignored JSONL bytes must be producer-published before terminal reconciliation: before={before:?}, coordinator_bytes={coordinator_bytes}, delta={delta:?}"
                    );
                    assert_eq!(before.processed_sessions, 0);
                    assert_eq!(before.processed_messages, 0);
                    assert_eq!(before.processed_tool_calls, 0);
                    observed_ignored_progress = true;
                }
                callback_history.advance_coordinator(&delta);
                coordinator_bytes = coordinator_bytes.saturating_add(delta.completed_bytes);
                accepted_records = accepted_records.saturating_add(delta.accepted_records);
                Ok(())
            };
        let mut sink = SourceBackedGenerationSink::new(
            &mut writer,
            &mut owners,
            &mut complete_inventories,
            &mut applied_removals,
            0,
            test_route_identity(),
            None,
            resources.clone(),
            &mut logical_source_failures,
            &mut record_rejections,
            Some(&mut report_record_progress),
            None,
            None,
        );

        with_family_scanner_workers(2, || {
            capture(&adapter, &root, &resident, &mut sink).unwrap();
        });
    }

    assert!(observed_ignored_progress);
    assert_eq!(accepted_records, 0);
    assert_eq!(coordinator_bytes, expected_bytes);
    assert!(writer.records.is_empty());
    assert_eq!(writer.certified_sources.len(), 2);
    assert!(writer.certified_sources.iter().all(|source| {
        let counts = source.counts();
        counts.complete_records == 1
            && counts.ignored_records == 1
            && counts.retained_records == 0
            && counts.indexed_documents == 0
    }));
    assert_eq!(
        shared_history.snapshot(),
        ctx_history_capture_model::AttemptHistoryProgressSnapshot {
            processed_sessions: 0,
            processed_messages: 0,
            processed_tool_calls: 0,
            processed_bytes: expected_bytes,
        }
    );
    assert_eq!(shared_history.parallel_byte_debt(), 0);
    assert_eq!(
        resources
            .live_bytes(ctx_history_capture_runtime::SourceBackedRouteResourceKind::CoreOutput),
        0
    );
}
