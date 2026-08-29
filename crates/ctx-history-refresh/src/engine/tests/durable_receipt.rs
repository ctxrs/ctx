//! Durable-receipt coverage owned by the refresh engine.

use super::*;

fn enqueue_synthetic_manual_all_request(
    coordinator: &super::super::CoreRefreshEngine,
    data_root: &Path,
    _revision: u64,
) -> Value {
    coordinator
        .enqueue_manual_all_demand_for_test(data_root, None, Uuid::now_v7().to_string())
        .expect("synthetic manual-all request")
}

fn publication_pin_test_publication(
    generation_id: impl Into<String>,
) -> SourceBackedRefreshPublication {
    let mut publication = test_publication(generation_id);
    publication.current = SourceBackedRefreshCurrent {
        source_count: 1,
        indexed_documents: 1,
        complete_records: 1,
        retained_records: 1,
        certified_source_bytes: 128,
        ..SourceBackedRefreshCurrent::default()
    };
    publication
}

#[test]
fn exact_no_op_restart_migrates_pre_fix_source_count_and_reuses_durable_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let metadata_factories = Arc::new(AtomicUsize::new(0));
    let first_factories = Arc::clone(&metadata_factories);
    let first = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let request_id = execution.request_id.to_owned();
            let operation = execution.operation;
            let scope = execution.admitted_refresh().publication_scope().clone();
            let factories = Arc::clone(&first_factories);
            let source = publication_pin_source();
            let mut writer = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .into_writer()
            .map_err(crate::committed_generation_recovery_error)?;
            writer.begin_source(source.clone())?;
            writer.add_core_record(publication_pin_record(&source))?;
            writer.certify_source(publication_pin_certificate(&source))?;
            let published = writer.commit_with_publication_metadata(
                |_| true,
                move |context| {
                    factories.fetch_add(1, Ordering::SeqCst);
                    let publication =
                        publication_pin_test_publication(context.generation_id().to_owned());
                    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
                        None,
                        context.generation_id().to_owned(),
                        &publication,
                    )
                    .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
                    SourceBackedPublicationMetadata {
                        version: SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
                        request_id: request_id.clone(),
                        operation,
                        refresh_scope: scope.clone(),
                        receipt: receipt.to_json(),
                        route_observations: BTreeMap::new(),
                        route_controls: BTreeMap::new(),
                    }
                    .encode()
                },
            )?;
            Ok(publication_pin_test_publication(
                published.receipt().generation_id.clone(),
            ))
        },
    ));
    let initial_request = first.enqueue_periodic(&data_root).unwrap();
    let initial_request_id = request_id(&initial_request);
    let initial = first.run_next(&data_root).expect("initial publication");
    assert!(!initial.failed, "{:#}", initial.job);
    assert_eq!(metadata_factories.load(Ordering::SeqCst), 1);
    assert!(initial.job.get("request_outcome").is_none());
    let durable_receipt = initial.job["receipt"].clone();
    let generation = initial.job["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();
    drop(first);

    let second_factories = Arc::clone(&metadata_factories);
    let second = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let source = publication_pin_source();
            let mut writer = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .into_writer()
            .map_err(crate::committed_generation_recovery_error)?;
            writer.begin_source(source.clone())?;
            writer.add_core_record(publication_pin_record(&source))?;
            writer.certify_source(publication_pin_certificate(&source))?;
            let published = writer.commit_with_publication_metadata(
                |_| true,
                |_| {
                    second_factories.fetch_add(1, Ordering::SeqCst);
                    Err(IndexError::PublicationMetadata(
                        "no-op replay must not rebuild publication metadata".to_owned(),
                    ))
                },
            )?;
            Ok(publication_pin_test_publication(
                published.receipt().generation_id.clone(),
            ))
        },
    ));
    let no_op_request = second.enqueue_periodic(&data_root).unwrap();
    let no_op_request_id = request_id(&no_op_request);
    assert_ne!(no_op_request_id, initial_request_id);
    let replay = second.run_next(&data_root).expect("exact no-op replay");
    assert!(!replay.failed, "{:#}", replay.job);
    assert!(!replay.did_work);
    assert_eq!(replay.job["published_generation"], generation);
    assert_eq!(replay.job["previous_generation"], generation);
    assert_eq!(replay.job["generation_changed"], false);
    assert_eq!(replay.job["receipt"], durable_receipt);
    assert_eq!(
        replay.job["request_outcome"]["previous_generation"],
        generation
    );
    assert_eq!(replay.job["request_outcome"]["generation_changed"], false);
    assert_eq!(metadata_factories.load(Ordering::SeqCst), 1);
    let pin = second.pinned_core_publication().expect("no-op Core pin");
    assert_eq!(pin.receipt().to_json(), durable_receipt);
    let exact_no_op_response = second.status(&no_op_request_id).unwrap();
    drop(second);

    let status_path = daemon_source_backed_refresh_job_path(&data_root);
    let mut pre_fix_job = read_daemon_job_status(&status_path)
        .expect("exact terminal status before compatibility migration");
    assert_eq!(pre_fix_job["source_count"], 0);
    assert_eq!(pre_fix_job["certified_source_count"], 1);
    // Pre-fix schema v1 projected this global count as `source_count`.
    pre_fix_job["source_count"] = pre_fix_job["certified_source_count"].clone();
    pre_fix_job["progress"]["current_source_progress"] = SourceBackedCurrentSourceProgress {
        stage: SourceBackedCurrentSourceProgressStage::LogicalScan,
        snapshot_pages_completed: None,
        snapshot_pages_total: None,
        snapshot_bytes_completed: None,
        snapshot_bytes_total: None,
        logical_rows_scanned: Some(1),
        logical_certified_bytes: Some(128),
    }
    .to_json();
    write_daemon_job_status(&status_path, &pre_fix_job).unwrap();

    let restarted = CoreRefreshEngine::new();
    assert!(!restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(
        restarted
            .pinned_core_publication()
            .expect("restarted no-op Core pin")
            .receipt()
            .to_json(),
        durable_receipt
    );
    assert_eq!(
        restarted.status(&no_op_request_id).unwrap(),
        exact_no_op_response
    );
    assert!(restarted.status(&initial_request_id).is_none());
    let recovered =
        read_daemon_job_status(&status_path).expect("migrated no-op terminal after restart");
    assert_eq!(recovered["request_id"], no_op_request_id);
    assert_eq!(recovered["source_count"], 0);
    assert_eq!(recovered["certified_source_count"], 1);
    assert!(recovered["progress"]
        .get("current_source_progress")
        .is_none());
}

#[test]
fn lone_failed_terminal_recovers_exact_status_without_reenqueue() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let first = CoreRefreshEngine::new();
    let request = first.enqueue(None);
    let request_id = request_id(&request);
    let failed = first
        .run_next_with(
            |request_id, engine| {
                let _ = engine.set_progress(
                    request_id,
                    SourceBackedRefreshProgressUpdate {
                        phase: "refreshing".to_owned(),
                        completed_sources: 0,
                        total_sources: 1,
                        total_sources_known: true,
                        current_source: Some("source-a".to_owned()),
                        completed_records: Some(3),
                        completed_bytes: Some(384),
                        current_source_progress: Some(SourceBackedCurrentSourceProgress {
                            stage: SourceBackedCurrentSourceProgressStage::LogicalScan,
                            snapshot_pages_completed: None,
                            snapshot_pages_total: None,
                            snapshot_bytes_completed: None,
                            snapshot_bytes_total: None,
                            logical_rows_scanned: Some(3),
                            logical_certified_bytes: Some(384),
                        }),
                        ..Default::default()
                    },
                );
                Err(anyhow!("exact lone terminal failure"))
            },
            || Ok(None),
            |job| write_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root), job),
            |_| Ok(()),
        )
        .expect("failed terminal");
    assert!(failed.failed);
    assert!(!failed.terminal_persistence_pending);
    let exact_failure = first.status(&request_id).unwrap();
    assert_eq!(exact_failure["request_state"], "failed");
    assert_eq!(exact_failure["logical_phase"], "terminal");
    assert_eq!(exact_failure["physical_attempt_id"], request_id);
    assert_eq!(
        exact_failure["structured_outcome"]["physical_attempt_id"],
        request_id
    );
    assert_eq!(
        exact_failure["structured_outcome"]["code"],
        "source_refresh_failed"
    );
    assert!(exact_failure["progress"].get("current_source").is_none());
    assert!(exact_failure["progress"]
        .get("current_source_progress")
        .is_none());
    assert_eq!(exact_failure["last_error"], "exact lone terminal failure");

    let status_path = daemon_source_backed_refresh_job_path(&data_root);
    let mut pre_fix_failure = read_daemon_job_status(&status_path).unwrap();
    pre_fix_failure["progress"]["current_source_progress"] = SourceBackedCurrentSourceProgress {
        stage: SourceBackedCurrentSourceProgressStage::LogicalScan,
        snapshot_pages_completed: None,
        snapshot_pages_total: None,
        snapshot_bytes_completed: None,
        snapshot_bytes_total: None,
        logical_rows_scanned: Some(3),
        logical_certified_bytes: Some(384),
    }
    .to_json();
    write_daemon_job_status(&status_path, &pre_fix_failure).unwrap();
    drop(first);

    let restarted = CoreRefreshEngine::new();
    assert!(!restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert!(!restarted.has_pending_request());
    assert_eq!(restarted.status(&request_id).unwrap(), exact_failure);
    let migrated_failure = read_daemon_job_status(&status_path).unwrap();
    assert_eq!(migrated_failure["request_id"], request_id);
    assert!(migrated_failure["progress"]
        .get("current_source_progress")
        .is_none());
}

#[test]
fn pointer_crash_recovers_matching_running_publication_terminal_and_preserves_successor() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let first = Arc::new(CoreRefreshEngine::new());
    let active_request_id = "019fcaaa-0000-7000-8000-000000000693".to_owned();
    let active = first
        .handle_ipc_request(
            &data_root,
            &json!({
                "schema_version": 1,
                "op": SOURCE_REFRESH_REQUEST_OP,
                "request_id": active_request_id,
                "mode": "wait",
                "operation": "refresh",
                "fresh_after_admitted_snapshot": true,
            }),
        )
        .unwrap()
        .expect("active wait admission");
    assert_eq!(active["request_state"], "admission_pending");
    let active_scope = refresh_scope_from_json(active.get("refresh_scope")).unwrap();
    let active_operation = SourceBackedRefreshOperation::from_request_json(&active).unwrap();
    assert!(first.prepare_next_pending_admission(&data_root).unwrap());

    let successor_request_id = Arc::new(Mutex::new(None::<String>));
    let committed = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let runner = Arc::clone(&first);
    let execution_root = data_root.clone();
    let recorded_successor = Arc::clone(&successor_request_id);
    let execution_committed = Arc::clone(&committed);
    let execution_release = Arc::clone(&release);
    let metadata_scope = active_scope.clone();
    let metadata_operation = active_operation;
    let crash = std::thread::spawn(move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = runner.run_next_with(
                |active_id, coordinator| {
                    let successor =
                        enqueue_synthetic_manual_all_request(coordinator, &execution_root, 1);
                    *recorded_successor.lock().unwrap() = Some(request_id(&successor));

                    let metadata_request_id = active_id.to_owned();
                    let published = ctx_history_index::GenerationWriter::open(
                        source_backed_index_root(&execution_root),
                        WriterOptions::default(),
                    )?
                    .into_writer()
                    .map_err(crate::committed_generation_recovery_error)?
                    .commit_with_publication_metadata(
                        |_| true,
                        |context| {
                            let mut publication = empty_test_publication(context.generation_id());
                            add_complete_empty_authority(&mut publication, route_identity(0x97));
                            let receipt = SourceBackedRefreshReceipt::from_verified_publication(
                                None,
                                context.generation_id().to_owned(),
                                &publication,
                            )
                            .map_err(|error| {
                                IndexError::PublicationMetadata(format!("{error:#}"))
                            })?;
                            SourceBackedPublicationMetadata {
                                version: SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
                                request_id: metadata_request_id.clone(),
                                operation: metadata_operation,
                                refresh_scope: metadata_scope.clone(),
                                receipt: receipt.to_json(),
                                route_observations: BTreeMap::new(),
                                route_controls: BTreeMap::new(),
                            }
                            .encode()
                        },
                    )?;
                    execution_committed.wait();
                    execution_release.wait();
                    panic!(
                        "injected crash after pointer publication {}",
                        published.receipt().generation_id
                    );
                },
                || panic!("crash must precede publication probe"),
                |_| panic!("crash must precede terminal persistence"),
                |_| panic!("crash must precede failure persistence"),
            );
        }))
    });

    committed.wait();
    first.fence_watch_uncertainty(EventWatermark::new(29, 1));
    release.wait();
    assert!(crash.join().unwrap().is_err());
    let successor_request_id = successor_request_id
        .lock()
        .unwrap()
        .clone()
        .expect("recorded successor request");
    let interrupted = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("interrupted durable queue");
    assert_eq!(interrupted["request_id"], active_request_id);
    assert_eq!(interrupted["request_state"], "running");
    assert_ne!(interrupted["progress"]["phase"], "watch_recovery");
    assert_eq!(
        interrupted["queued_successors"][0]["request_id"],
        successor_request_id
    );
    drop(first);

    let executions = Arc::new(AtomicUsize::new(0));
    let observed_executions = Arc::clone(&executions);
    let restarted = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            observed_executions.fetch_add(1, Ordering::SeqCst);
            super::publication_lifecycle_tests::publish_empty_generation_with_request_metadata(
                &execution, 0x9a,
            )
        },
    ));
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    let recovered = restarted.status(&active_request_id).unwrap();
    assert_eq!(recovered["request_state"], "published");
    assert_eq!(
        restarted.status(&successor_request_id).unwrap()["request_state"],
        "admission_pending"
    );

    let reconnect = restarted
        .handle_ipc_request(
            &data_root,
            &json!({
                "schema_version": 1,
                "op": SOURCE_REFRESH_REQUEST_OP,
                "request_id": active_request_id,
                "mode": "wait",
                "operation": "refresh",
                "fresh_after_admitted_snapshot": true,
            }),
        )
        .unwrap()
        .expect("reconnected active wait");
    assert_eq!(reconnect["request_id"], active_request_id);
    assert_eq!(reconnect["request_state"], "published");

    assert!(restarted
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    let successor = restarted.run_next(&data_root).expect("queued successor");
    assert_eq!(successor.job["request_id"], successor_request_id);
    assert_eq!(successor.job["request_state"], "published");
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_terminal_retry_journals_successor_before_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let first = CoreRefreshEngine::new();
    let active = first.enqueue(None);
    let active_id = request_id(&active);
    let failed = first
        .run_next_with(
            |_, _| Err(anyhow!("injected provider failure")),
            || Ok(None),
            |_| Err(anyhow!("injected terminal status write failure")),
            |_| Ok(()),
        )
        .unwrap();
    assert!(failed.failed);
    assert!(failed.terminal_persistence_pending);

    let successor = enqueue_synthetic_manual_all_request(&first, &data_root, 4);
    let successor_id = request_id(&successor);
    let pending = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("failed root with durable successor");
    assert_eq!(pending["request_id"], active_id);
    assert_eq!(pending["queued_successors"][0]["request_id"], successor_id);

    let retried = first
        .run_next_with(
            |_, _| panic!("terminal retry must not recapture"),
            || panic!("terminal retry must not reopen Core"),
            |job| write_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root), job),
            |_| panic!("terminal retry must not rerun failure handling"),
        )
        .unwrap();
    assert!(retried.failed);
    assert!(!retried.terminal_persistence_pending);
    drop(first);

    let restarted = CoreRefreshEngine::new();
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(
        restarted.status(&successor_id).unwrap()["request_state"],
        "admission_pending"
    );
    assert!(restarted.has_pending_request());
}
