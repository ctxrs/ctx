use super::*;
use std::cell::RefCell;

#[test]
fn ready_driven_mixed_outcomes_finalize_diagnostics_in_canonical_job_order() {
    let serial = run_failure_ordering_fixture(1, false);
    let parallel = run_failure_ordering_fixture(16, true);

    assert_eq!(parallel, serial);
    assert_eq!(
        parallel.failed_outcomes,
        (0_u8..70).map(|id| id % 2 == 1).collect::<Vec<_>>()
    );
    assert_eq!(
        parallel.diagnostic_sources,
        (0_u8..70)
            .filter(|id| id % 2 == 1)
            .map(test_source)
            .collect::<Vec<_>>()
    );
    assert_eq!(parallel.rejection_lines, (0_u64..64).collect::<Vec<_>>());
    assert_eq!(parallel.omitted_failures, 0);
    assert_eq!(parallel.omitted_rejections, 6);
}

fn run_failure_ordering_fixture(
    worker_count: usize,
    force_out_of_order: bool,
) -> FailureOrderingSummary {
    let temp = tempdir();
    let mut harness = SinkHarness::open(&temp.path().join("index"));
    let jobs = (0_u8..70)
        .map(|id| ParallelLeafScanJob::new(test_source(id), id))
        .collect();
    let (accepted_sender, accepted_receiver) = mpsc::channel();
    let accepted_receiver = Mutex::new(accepted_receiver);
    let mut outcomes = harness
        .run_with_source_outcomes(jobs, worker_count, move |job, emitter| {
            let id = *job.leaf();
            if force_out_of_order && id == 0 {
                let receiver = accepted_receiver.lock().unwrap();
                for _ in 0..65 {
                    receiver.recv_timeout(Duration::from_secs(5)).map_err(|_| {
                        ParallelLeafScanWorkerError::provider(TestWorkerFailure::Injected)
                    })?;
                }
            }
            let mut rejections = SourceBackedRecordRejectionDrafts::default();
            rejections.record(SourceBackedRecordRejectionDraft {
                source: job.source().clone(),
                provider: CaptureProvider::Codex,
                source_selector: format!("source-{id}"),
                line_number: u64::from(id),
                payload_type: Some("fixture".to_owned()),
                class: SourceBackedRecordRejectionClass::MalformedRecord,
                detail: format!("rejection-{id}"),
            });
            if id % 2 == 0 {
                emitter.complete(ParallelLeafScanComplete::Skipped {
                    result: (id, rejections),
                })?;
            } else {
                emitter.complete(ParallelLeafScanComplete::source_failure_with_rejections(
                    job.source().clone(),
                    None,
                    SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::InvalidSource,
                        format!("failure-{id}"),
                    ),
                    rejections,
                ))?;
            }
            if force_out_of_order && id % 16 != 0 {
                accepted_sender.send(id).unwrap();
            }
            Ok(())
        })
        .unwrap();

    let mut failed_outcomes = Vec::with_capacity(outcomes.len());
    let mut canonical_rejections = SourceBackedRecordRejectionDrafts::default();
    for (id, outcome) in outcomes.iter_mut().enumerate() {
        match outcome {
            SourceBackedSourceOutcome::Success((result_id, rejections)) => {
                assert_eq!(usize::from(*result_id), id);
                failed_outcomes.push(false);
                canonical_rejections.merge(std::mem::take(rejections));
            }
            SourceBackedSourceOutcome::Failed(failure) => {
                assert_eq!(failure.source, test_source(u8::try_from(id).unwrap()));
                failed_outcomes.push(true);
                canonical_rejections.merge(std::mem::take(&mut failure.record_rejections));
            }
        }
    }
    harness.record_rejections(canonical_rejections);

    FailureOrderingSummary {
        failed_outcomes,
        diagnostic_sources: harness
            .logical_source_failures
            .failures()
            .iter()
            .map(|failure| failure.source.clone())
            .collect(),
        rejection_lines: harness
            .record_rejections
            .rejections()
            .iter()
            .map(|rejection| rejection.line_number)
            .collect(),
        omitted_failures: harness.logical_source_failures.omitted(),
        omitted_rejections: harness.record_rejections.omitted(),
    }
}

#[test]
fn source_worker_names_and_spawn_count_are_deterministically_bounded() {
    let names = (0..MAX_PARALLEL_LEAF_WORKERS)
        .map(source_worker_thread_name)
        .collect::<HashSet<_>>();

    assert_eq!(names.len(), MAX_PARALLEL_LEAF_WORKERS);
    assert!(names.iter().all(|name| name.len() <= 15));
    assert!(names.contains("ctx-src-scan00"));
    assert!(names.contains("ctx-src-scan15"));
    assert_eq!(
        bounded_leaf_worker_count(usize::MAX, usize::MAX),
        MAX_PARALLEL_LEAF_WORKERS
    );
    assert_eq!(bounded_leaf_worker_count(3, usize::MAX), 3);
}

#[test]
fn sink_budget_caps_requested_workers_and_writer_consumes_jobs_in_input_order() {
    let temp = tempdir();
    let mut harness = SinkHarness::open(&temp.path().join("index"));
    harness.leaf_worker_budget = 2;
    let later_job_reached_emission = Arc::new(AtomicBool::new(false));
    let later_ready = Arc::clone(&later_job_reached_emission);
    let observed_workers = Arc::new(Mutex::new(HashSet::new()));
    let workers = Arc::clone(&observed_workers);
    let jobs = (0_u8..4)
        .map(|id| ParallelLeafScanJob::new(test_source(id.saturating_add(20)), id))
        .collect();

    let results = harness
        .run(jobs, usize::MAX, move |job, emitter| {
            workers
                .lock()
                .unwrap()
                .insert(std::thread::current().name().unwrap_or_default().to_owned());
            if *job.leaf() == 1 {
                later_ready.store(true, Ordering::Release);
            } else if *job.leaf() == 0 {
                while !later_ready.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
            }
            emitter.complete(ParallelLeafScanComplete::Skipped {
                result: *job.leaf(),
            })?;
            Ok(())
        })
        .unwrap();

    assert_eq!(results, [0, 1, 2, 3]);
    assert_eq!(observed_workers.lock().unwrap().len(), 2);
    assert_eq!(
        *observed_workers.lock().unwrap(),
        HashSet::from([source_worker_thread_name(0), source_worker_thread_name(1),])
    );
}

#[test]
fn append_and_skipped_jobs_use_typed_lifecycles_and_ordered_results() {
    let temp = tempdir();
    let index_root = temp.path().join("index");
    let append_source = test_source(1);
    let skipped_source = test_source(2);
    let base = publish_append_base(&index_root, &append_source, 11);
    let current = test_certificate(&append_source, 12, 2, true);
    let append = CertifiedSourceAppend::certify(
        &base,
        current,
        base.counts().certified_bytes,
        *base.content_digest(),
    )
    .unwrap();
    let jobs = vec![
        ParallelLeafScanJob::new(append_source.clone(), true),
        ParallelLeafScanJob::new(skipped_source, false),
    ];
    let mut harness = SinkHarness::with_base(base.clone());

    let results = harness
        .run(jobs, 2, |job, emitter| {
            if *job.leaf() {
                emitter.begin(ParallelLeafScanBegin::Append {
                    source: job.source().clone(),
                    base: Box::new(base.clone()),
                })?;
                emitter.emit_core_record(test_core_record(job.source(), 2, 12))?;
                emitter.complete(ParallelLeafScanComplete::append(append.clone(), "append"))?;
            } else {
                emitter.complete(ParallelLeafScanComplete::Skipped { result: "skip" })?;
            }
            Ok(())
        })
        .unwrap();
    let commit = harness.commit();

    assert_eq!(results, ["append", "skip"]);
    assert_eq!(commit.certified_sources, 1);
    assert_eq!(commit.indexed_documents, 2);
}

#[test]
fn protocol_rejects_wrong_exact_source() {
    let expected = test_source(1);
    let observed = test_source(2);
    let error = run_single(expected, move |_job, emitter| {
        emitter.begin(ParallelLeafScanBegin::Replace {
            source: observed.clone(),
        })?;
        Ok(())
    })
    .unwrap_err();

    assert!(matches!(
        error,
        ParallelLeafScanError::Protocol(ParallelLeafScanProtocolError::SourceMismatch {
            job_index: 0,
            message: ParallelLeafScanMessageKind::BeginReplace,
            ..
        })
    ));
}

#[test]
fn protocol_rejects_wrong_append_base() {
    let temp = tempdir();
    let index_root = temp.path().join("index");
    let source = test_source(3);
    let base = publish_append_base(&index_root, &source, 31);
    let wrong_base = test_certificate(&source, 32, 1, true);
    let mut harness = SinkHarness::with_base(base);
    let jobs = vec![ParallelLeafScanJob::new(source, ())];

    let error = harness
        .run::<_, (), _>(jobs, 1, move |job, emitter| {
            emitter.begin(ParallelLeafScanBegin::Append {
                source: job.source().clone(),
                base: Box::new(wrong_base.clone()),
            })?;
            Ok(())
        })
        .unwrap_err();

    assert!(matches!(
        error,
        ParallelLeafScanError::Protocol(ParallelLeafScanProtocolError::AppendBaseMismatch {
            job_index: 0
        })
    ));
}

#[test]
fn begin_rendezvous_blocks_worker_until_coordinator_acknowledgement() {
    let temp = tempdir();
    let harness = SinkHarness::open(&temp.path().join("index"));
    let source = test_source(36);
    let cancellation = AtomicBool::new(false);
    let resources = SourceBackedRouteResources::production(1);
    let preparer = harness.writer.core_preparation();
    let (sender, receiver) = mpsc::sync_channel(0);
    let (returned_sender, returned_receiver) = mpsc::sync_channel(0);

    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut emitter = ParallelLeafScanEmitter::<(), TestWorkerFailure> {
                worker_index: 0,
                job_index: 0,
                sender: &sender,
                cancellation: &cancellation,
                resources,
                core_record_preparer: preparer,
                successful_resource_acquisitions: None,
            };
            emitter
                .begin(ParallelLeafScanBegin::replace(source.clone()))
                .unwrap();
            returned_sender.send(()).unwrap();
        });

        let event = receiver.recv().unwrap();
        assert!(matches!(
            returned_receiver.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        let ParallelLeafWorkerEvent::Protocol { message, .. } = event else {
            panic!("worker must emit a Begin protocol message");
        };
        let ParallelLeafProtocolMessage::Begin {
            acknowledgement, ..
        } = *message
        else {
            panic!("worker must emit Begin before returning");
        };
        acknowledgement.acknowledge(0).unwrap();
        returned_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
    });
}

#[test]
fn protocol_rejects_duplicate_begin() {
    let error = run_single(test_source(4), |job, emitter| {
        for _ in 0..2 {
            emitter.begin(ParallelLeafScanBegin::Replace {
                source: job.source().clone(),
            })?;
        }
        Ok(())
    })
    .unwrap_err();

    assert!(matches!(
        error,
        ParallelLeafScanError::Protocol(ParallelLeafScanProtocolError::DuplicateBegin {
            job_index: 0
        })
    ));
}

#[test]
fn protocol_rejects_missing_and_duplicate_completion() {
    let missing = run_single(test_source(5), |_job, _emitter| Ok(())).unwrap_err();
    assert!(matches!(
        missing,
        ParallelLeafScanError::Protocol(ParallelLeafScanProtocolError::MissingCompletion {
            job_index: 0
        })
    ));

    let duplicate = run_single(test_source(6), |_job, emitter| {
        emitter.complete(ParallelLeafScanComplete::Skipped { result: () })?;
        emitter.complete(ParallelLeafScanComplete::Skipped { result: () })?;
        Ok(())
    })
    .unwrap_err();
    assert!(matches!(
        duplicate,
        ParallelLeafScanError::Protocol(ParallelLeafScanProtocolError::DuplicateCompletion {
            job_index: 0
        })
    ));
}

#[test]
fn protocol_rejects_core_record_before_begin_and_skip_after_begin() {
    let source = test_source(7);
    let record_source = source.clone();
    let record = run_single(source, move |_job, emitter| {
        emitter.emit_core_record(test_core_record(&record_source, 1, 71))?;
        Ok(())
    })
    .unwrap_err();
    assert!(matches!(
        record,
        ParallelLeafScanError::Protocol(ParallelLeafScanProtocolError::CoreRecordBeforeBegin {
            job_index: 0
        })
    ));

    let skipped = run_single(test_source(8), |job, emitter| {
        emitter.begin(ParallelLeafScanBegin::Replace {
            source: job.source().clone(),
        })?;
        emitter.complete(ParallelLeafScanComplete::Skipped { result: () })?;
        Ok(())
    })
    .unwrap_err();
    assert!(matches!(
        skipped,
        ParallelLeafScanError::Protocol(ParallelLeafScanProtocolError::SkippedAfterBegin {
            job_index: 0
        })
    ));
}

#[test]
fn worker_error_cancels_its_peer_and_all_workers_join() {
    let temp = tempdir();
    let mut harness = SinkHarness::open(&temp.path().join("index"));
    let jobs = vec![
        ParallelLeafScanJob::new(test_source(9), 0_u8),
        ParallelLeafScanJob::new(test_source(10), 1_u8),
    ];
    let barrier = Arc::new(Barrier::new(2));
    let scan_barrier = Arc::clone(&barrier);
    let peer_cancelled = Arc::new(AtomicBool::new(false));
    let observed_cancel = Arc::clone(&peer_cancelled);

    let error = harness
        .run::<_, (), _>(jobs, 2, move |job, emitter| {
            scan_barrier.wait();
            if *job.leaf() == 0 {
                return Err(ParallelLeafScanWorkerError::provider(
                    TestWorkerFailure::Injected,
                ));
            }
            while !emitter.is_cancelled() {
                std::thread::yield_now();
            }
            observed_cancel.store(true, Ordering::Release);
            Err(ParallelLeafScanCancelled.into())
        })
        .unwrap_err();

    assert!(matches!(
        error,
        ParallelLeafScanError::Worker {
            worker_index: 0,
            job_index: 0,
            ..
        }
    ));
    assert!(peer_cancelled.load(Ordering::Acquire));
}

#[test]
fn worker_panic_and_unprompted_cancel_are_typed() {
    let panic_error = run_single(test_source(11), |_job, _emitter| {
        panic!("injected guarded panic");
    })
    .unwrap_err();
    assert!(matches!(
        panic_error,
        ParallelLeafScanError::WorkerPanicked {
            worker_index: 0,
            job_index: 0
        }
    ));

    let cancel_error = run_single(test_source(12), |_job, _emitter| {
        Err(ParallelLeafScanCancelled.into())
    })
    .unwrap_err();
    assert!(matches!(
        cancel_error,
        ParallelLeafScanError::WorkerCancelled {
            worker_index: 0,
            job_index: 0
        }
    ));
}

struct SpawnedWorkerDropProbe(Arc<AtomicBool>);

impl Drop for SpawnedWorkerDropProbe {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[test]
fn worker_spawn_failure_is_typed_and_joins_already_started_workers() {
    let temp = tempdir();
    let index_root = temp.path().join("index");
    let retained_source = test_source(29);
    let retained = publish_append_base(&index_root, &retained_source, 29);
    let mut harness = SinkHarness::with_base(retained.clone());
    let first_worker_dropped_job = Arc::new(AtomicBool::new(false));
    let jobs = vec![
        ParallelLeafScanJob::new(
            test_source(30),
            SpawnedWorkerDropProbe(Arc::clone(&first_worker_dropped_job)),
        ),
        ParallelLeafScanJob::new(
            test_source(31),
            SpawnedWorkerDropProbe(Arc::new(AtomicBool::new(false))),
        ),
    ];
    let previous = INJECT_WORKER_SPAWN_FAILURE_AT.with(|injected| injected.replace(Some(1)));
    let error = harness
        .run::<_, (), _>(jobs, 2, |_job, emitter| {
            emitter.complete(ParallelLeafScanComplete::Skipped { result: () })?;
            Ok(())
        })
        .unwrap_err();
    INJECT_WORKER_SPAWN_FAILURE_AT.with(|injected| injected.set(previous));

    assert!(matches!(
        error,
        ParallelLeafScanError::WorkerSpawn {
            worker_index: 1,
            ..
        }
    ));
    assert!(first_worker_dropped_job.load(Ordering::Acquire));
    assert_eq!(harness.writer.base_sources, [retained]);
}

struct PanicOnDrop;

impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        panic!("injected unguarded worker drop panic");
    }
}

#[test]
fn unguarded_worker_panic_is_reported_from_mandatory_join() {
    let temp = tempdir();
    let mut harness = SinkHarness::open(&temp.path().join("index"));
    let jobs = vec![ParallelLeafScanJob::new(test_source(13), PanicOnDrop)];

    let error = harness
        .run(jobs, 1, |_job, emitter| {
            emitter.complete(ParallelLeafScanComplete::Skipped { result: () })?;
            Ok(())
        })
        .unwrap_err();

    assert!(matches!(
        error,
        ParallelLeafScanError::WorkerJoinPanicked { worker_index: 0 }
    ));
}

#[test]
fn worker_budget_coordinates_indexers_runtime_and_scanners() {
    assert_eq!(leaf_worker_budget_for_parallelism(8, 32), 16);
    assert_eq!(leaf_worker_budget_for_parallelism(usize::MAX, 32), 16);
    assert_eq!(leaf_worker_budget_for_parallelism(4, 10), 5);
    assert_eq!(leaf_worker_budget_for_parallelism(8, 4), 1);

    let allocations = [1_usize, 2, 4, 8, 16, 32].map(|parallelism| {
        let indexers = source_backed_refresh_indexer_threads_for_parallelism(parallelism);
        let scanners = leaf_worker_budget_for_parallelism(indexers, parallelism);
        (parallelism, indexers, scanners)
    });
    assert_eq!(
        allocations,
        [
            (1, 1, 1),
            (2, 2, 1),
            (4, 1, 2),
            (8, 3, 4),
            (16, 7, 8),
            (32, 8, 16),
        ]
    );

    let temp = tempdir();
    let mut harness = SinkHarness::open(&temp.path().join("index"));
    harness.leaf_worker_budget = 6;
    let sink = SourceBackedGenerationSink {
        core_record_preparer: harness.writer.core_preparation(),
        lifecycle: &mut harness.writer,
        owners: &mut harness.owners,
        complete_inventories: &mut harness.complete_inventories,
        route_index: 0,
        route_identity: test_route_identity(),
        base_route_aliases: BTreeSet::new(),
        base_route_control: None,
        resources: SourceBackedRouteResources::production(harness.leaf_worker_budget),
        logical_source_failures: &mut harness.logical_source_failures,
        record_rejections: &mut harness.record_rejections,
        applied_removals: &mut Vec::new(),
        record_progress: None,
        current_source_progress: None,
        intermediate_progress_last_emitted_at: None,
        intermediate_progress_pending_stage: None,
        last_progress_session_id: None,
        exact_scan_total_bytes: None,
        exact_scan_accounting_enabled: false,
    };
    assert_eq!(sink.recommended_leaf_workers(0), 0);
    assert_eq!(sink.recommended_leaf_workers(2), 2);
    assert_eq!(sink.recommended_leaf_workers(20), 6);
}

#[test]
fn single_core_record_transport_uses_one_bounded_zero_capacity_batch_rendezvous() {
    let temp = tempdir();
    let harness = SinkHarness::open(&temp.path().join("index"));
    let core_record_preparer = harness.writer.core_preparation();
    let source = test_source(14);
    let record = test_core_record(&source, 1, 141);
    let (sender, receiver) =
        mpsc::sync_channel::<ParallelLeafWorkerEvent<(), TestWorkerFailure, FakePreparation>>(0);
    let cancellation = AtomicBool::new(false);
    let barrier = Barrier::new(2);
    let (finished_sender, finished_receiver) = mpsc::channel();

    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut emitter = ParallelLeafScanEmitter {
                worker_index: 0,
                job_index: 0,
                sender: &sender,
                cancellation: &cancellation,
                resources: SourceBackedRouteResources::production(1),
                core_record_preparer,
                successful_resource_acquisitions: None,
            };
            barrier.wait();
            emitter.emit_core_record(record).unwrap();
            finished_sender.send(()).unwrap();
        });

        barrier.wait();
        assert!(matches!(
            finished_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        let event = receiver.recv().unwrap();
        let ParallelLeafWorkerEvent::Protocol {
            worker_index,
            job_index,
            message,
        } = event
        else {
            panic!("expected a protocol event");
        };
        assert_eq!(worker_index, 0);
        assert_eq!(job_index, 0);
        let ParallelLeafProtocolMessage::CoreRecordBatch { batch, .. } = *message else {
            panic!("expected one Core-record batch");
        };
        let batch = batch.unwrap();
        assert_eq!(batch.len(), 1);
        finished_receiver.recv().unwrap();
    });
}

#[test]
fn core_record_batch_transport_is_one_bounded_zero_capacity_rendezvous() {
    let temp = tempdir();
    let harness = SinkHarness::open(&temp.path().join("index"));
    let core_record_preparer = harness.writer.core_preparation();
    let source = test_source(32);
    let records = (1..=3)
        .map(|sequence| test_core_record(&source, sequence, 32))
        .collect::<Vec<_>>();
    let resources = SourceBackedRouteResources::production(1);
    let successful_resource_acquisitions = AtomicUsize::new(0);
    let (sender, receiver) =
        mpsc::sync_channel::<ParallelLeafWorkerEvent<(), TestWorkerFailure, FakePreparation>>(0);
    let cancellation = AtomicBool::new(false);
    let barrier = Barrier::new(2);
    let (finished_sender, finished_receiver) = mpsc::channel();

    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut emitter = ParallelLeafScanEmitter {
                worker_index: 0,
                job_index: 0,
                sender: &sender,
                cancellation: &cancellation,
                resources: resources.clone(),
                core_record_preparer: core_record_preparer.clone(),
                successful_resource_acquisitions: Some(&successful_resource_acquisitions),
            };
            barrier.wait();
            emitter.emit_core_records(records).unwrap();
            finished_sender.send(()).unwrap();
        });

        barrier.wait();
        assert!(matches!(
            finished_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        let event = receiver.recv().unwrap();
        let ParallelLeafWorkerEvent::Protocol { message, .. } = event else {
            panic!("expected a protocol event");
        };
        let ParallelLeafProtocolMessage::CoreRecordBatch { batch, .. } = *message else {
            panic!("expected one Core-record batch");
        };
        let batch = batch.unwrap();
        assert_eq!(batch.len(), 3);
        assert_eq!(
            successful_resource_acquisitions.load(Ordering::Relaxed),
            1,
            "one transported batch must perform exactly one successful resource acquisition"
        );
        finished_receiver.recv().unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    });
    assert_eq!(
        resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        0
    );

    cancellation.store(true, Ordering::Release);
    let mut invalid_record = test_core_record(&source, 4, 32);
    invalid_record.source = test_source(132);
    let mut emitter = ParallelLeafScanEmitter {
        worker_index: 0,
        job_index: 0,
        sender: &sender,
        cancellation: &cancellation,
        resources: resources.clone(),
        core_record_preparer,
        successful_resource_acquisitions: None,
    };
    let error = emitter.emit_core_records(vec![invalid_record]).unwrap_err();
    assert!(matches!(error, ParallelLeafScanEmitError::Cancelled(_)));
    assert_eq!(
        resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        0
    );
}

#[test]
fn batch_emitter_rejects_a_zero_output_budget_without_transporting_a_record() {
    let temp = tempdir();
    let harness = SinkHarness::open(&temp.path().join("index"));
    let source = test_source(39);
    let resources = SourceBackedRouteResources::for_test(1, 0, u64::MAX);
    let (sender, receiver) =
        mpsc::sync_channel::<ParallelLeafWorkerEvent<(), TestWorkerFailure, FakePreparation>>(0);
    let cancellation = AtomicBool::new(false);
    let successful_resource_acquisitions = AtomicUsize::new(0);
    let mut emitter = ParallelLeafScanEmitter {
        worker_index: 0,
        job_index: 0,
        sender: &sender,
        cancellation: &cancellation,
        resources: resources.clone(),
        core_record_preparer: harness.writer.core_preparation(),
        successful_resource_acquisitions: Some(&successful_resource_acquisitions),
    };

    let error = emitter
        .emit_core_records(vec![test_core_record(&source, 1, 39)])
        .unwrap_err();
    assert!(matches!(
        error,
        ParallelLeafScanEmitError::Route(SourceBackedRouteError {
            kind: SourceBackedRouteErrorKind::ResourceUnavailable,
            ..
        })
    ));
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert_eq!(
        successful_resource_acquisitions.load(Ordering::Relaxed),
        0,
        "a zero-capacity rendezvous must perform zero successful resource acquisitions"
    );
    assert_eq!(
        resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        0
    );
}

#[test]
fn batch_emitter_streams_records_that_fit_individually_but_not_together() {
    let temp = tempdir();
    let harness = SinkHarness::open(&temp.path().join("index"));
    let core_record_preparer = harness.writer.core_preparation();
    let source = test_source(35);
    let records = (1..=2)
        .map(|sequence| test_core_record(&source, sequence, 35))
        .collect::<Vec<_>>();
    let maximum_record_bytes = records
        .iter()
        .cloned()
        .map(|record| {
            u64::try_from(core_record_preparer.prepare(record).unwrap().encoded_bytes).unwrap()
        })
        .max()
        .unwrap();
    let resources = SourceBackedRouteResources::for_test(1, maximum_record_bytes, u64::MAX);
    let (sender, receiver) =
        mpsc::sync_channel::<ParallelLeafWorkerEvent<(), TestWorkerFailure, FakePreparation>>(0);
    let (finished_sender, finished_receiver) = mpsc::channel();
    let cancellation = AtomicBool::new(false);

    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut emitter = ParallelLeafScanEmitter {
                worker_index: 0,
                job_index: 0,
                sender: &sender,
                cancellation: &cancellation,
                resources: resources.clone(),
                core_record_preparer,
                successful_resource_acquisitions: None,
            };
            finished_sender
                .send(emitter.emit_core_records(records))
                .unwrap();
        });

        for _ in 0..2 {
            let ParallelLeafWorkerEvent::Protocol { message, .. } = receiver.recv().unwrap() else {
                panic!("expected a protocol event");
            };
            let ParallelLeafProtocolMessage::CoreRecordBatch { batch, .. } = *message else {
                panic!("expected a Core-record batch");
            };
            let batch = batch.unwrap();
            assert_eq!(batch.len(), 1);
        }
        finished_receiver.recv().unwrap().unwrap();
    });
    assert_eq!(
        resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        0
    );
}

#[test]
fn batch_emitter_backpressures_multiple_workers_at_the_shared_byte_limit() {
    let temp = tempdir();
    let harness = SinkHarness::open(&temp.path().join("index"));
    let core_record_preparer = harness.writer.core_preparation();
    let sources = [test_source(36), test_source(37)];
    let records = sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            test_core_record(source, u64::try_from(index).unwrap().saturating_add(1), 36)
        })
        .collect::<Vec<_>>();
    let maximum_record_bytes = records
        .iter()
        .cloned()
        .map(|record| {
            u64::try_from(core_record_preparer.prepare(record).unwrap().encoded_bytes).unwrap()
        })
        .max()
        .unwrap();
    let resources = SourceBackedRouteResources::for_test(2, maximum_record_bytes, u64::MAX);
    let (sender, receiver) =
        mpsc::sync_channel::<ParallelLeafWorkerEvent<(), TestWorkerFailure, FakePreparation>>(0);
    let cancellation = AtomicBool::new(false);
    let barrier = Barrier::new(3);

    std::thread::scope(|scope| {
        for (worker_index, record) in records.into_iter().enumerate() {
            let sender = &sender;
            let cancellation = &cancellation;
            let barrier = &barrier;
            let resources = resources.clone();
            let core_record_preparer = core_record_preparer.clone();
            scope.spawn(move || {
                let mut emitter = ParallelLeafScanEmitter {
                    worker_index,
                    job_index: worker_index,
                    sender,
                    cancellation,
                    resources,
                    core_record_preparer,
                    successful_resource_acquisitions: None,
                };
                barrier.wait();
                emitter.emit_core_records(vec![record]).unwrap();
            });
        }

        barrier.wait();
        for _ in 0..2 {
            let ParallelLeafWorkerEvent::Protocol { message, .. } = receiver.recv().unwrap() else {
                panic!("expected a protocol event");
            };
            assert!(matches!(
                *message,
                ParallelLeafProtocolMessage::CoreRecordBatch { .. }
            ));
        }
    });
    assert_eq!(
        resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        0
    );
}

#[test]
fn batch_emitter_chunks_projector_fanout_at_the_protocol_bound() {
    let temp = tempdir();
    let harness = SinkHarness::open(&temp.path().join("index"));
    let core_record_preparer = harness.writer.core_preparation();
    let source = test_source(33);
    let records = (1..=u64::try_from(SOURCE_BACKED_CORE_RECORD_BATCH_MAX_RECORDS + 1).unwrap())
        .map(|sequence| test_core_record(&source, sequence, 33))
        .collect::<Vec<_>>();
    let (sender, receiver) =
        mpsc::sync_channel::<ParallelLeafWorkerEvent<(), TestWorkerFailure, FakePreparation>>(0);
    let cancellation = AtomicBool::new(false);

    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut emitter = ParallelLeafScanEmitter {
                worker_index: 0,
                job_index: 0,
                sender: &sender,
                cancellation: &cancellation,
                resources: SourceBackedRouteResources::production(1),
                core_record_preparer,
                successful_resource_acquisitions: None,
            };
            emitter.emit_core_records(records).unwrap();
        });

        let mut batch_lengths = Vec::new();
        for _ in 0..2 {
            let ParallelLeafWorkerEvent::Protocol { message, .. } = receiver.recv().unwrap() else {
                panic!("expected a protocol event");
            };
            let ParallelLeafProtocolMessage::CoreRecordBatch { batch, .. } = *message else {
                panic!("expected a Core-record batch");
            };
            let batch = batch.unwrap();
            batch_lengths.push(batch.len());
        }
        assert_eq!(
            batch_lengths,
            [SOURCE_BACKED_CORE_RECORD_BATCH_MAX_RECORDS, 1]
        );
    });
}

#[test]
fn scanner_and_worker_history_precede_durable_batch_acceptance() {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Observation {
        Activity(SourceBackedCurrentSourceProgressStage),
        Accepted(u64),
    }

    let source = test_source(38);
    let record = test_core_record(&source, 1, 38);
    let shared_history = ctx_history_capture_model::SharedAttemptHistoryProgress::default();
    let resources = SourceBackedRouteResources::production(1)
        .with_attempt_history_progress(shared_history.clone());
    let scanner_resources = resources.clone();
    let observed_history = shared_history.clone();
    let observations = RefCell::new(Vec::new());
    let mut report_record_progress = |delta: SourceBackedRecordProgressDelta| {
        observations
            .borrow_mut()
            .push(Observation::Accepted(delta.accepted_records));
        Ok(())
    };
    let mut report_current_source_progress = |progress: SourceBackedCurrentSourceProgress| {
        if progress.stage == SourceBackedCurrentSourceProgressStage::IndexWriting {
            assert_eq!(
                observed_history.snapshot(),
                ctx_history_capture_model::AttemptHistoryProgressSnapshot {
                    processed_sessions: 1,
                    processed_messages: 1,
                    processed_tool_calls: 0,
                    processed_bytes: 0,
                },
                "worker-owned history must be visible before writer admission"
            );
        }
        observations
            .borrow_mut()
            .push(Observation::Activity(progress.stage));
        Ok(())
    };
    let mut harness = SinkHarness::with_lifecycle(FakeLifecycle::with_add_prepared_delay(
        Duration::from_millis(150),
    ));

    harness
        .run_with_resources_and_progress::<_, (), _>(
            vec![ParallelLeafScanJob::new(source, ())],
            1,
            resources,
            &mut report_record_progress,
            &mut report_current_source_progress,
            move |job, emitter| {
                emitter.begin(ParallelLeafScanBegin::replace(job.source().clone()))?;
                scanner_resources
                    .record_intermediate_activity(SourceBackedCurrentSourceProgressStage::Parsing);
                std::thread::sleep(Duration::from_millis(150));
                emitter.emit_core_record(record.clone())?;
                emitter.complete(ParallelLeafScanComplete::replace(
                    test_certificate(job.source(), 38, 1, false),
                    (),
                ))?;
                Ok(())
            },
        )
        .unwrap();

    assert_eq!(
        observations.into_inner(),
        [
            Observation::Activity(SourceBackedCurrentSourceProgressStage::Parsing),
            Observation::Activity(SourceBackedCurrentSourceProgressStage::IndexWriting),
            Observation::Accepted(1),
        ],
        "scanner activity and worker history must precede durable Core acceptance"
    );
    assert_eq!(harness.writer.records.len(), 1);
}

#[test]
fn alternating_intermediate_stages_share_one_ten_hertz_cadence() {
    let source = test_source(39);
    let resources = SourceBackedRouteResources::production(1);
    let scanner_resources = resources.clone();
    let observations = RefCell::new(Vec::new());
    let mut report_record_progress = |_delta: SourceBackedRecordProgressDelta| Ok(());
    let mut report_current_source_progress = |progress: SourceBackedCurrentSourceProgress| {
        observations
            .borrow_mut()
            .push((Instant::now(), progress.stage));
        Ok(())
    };
    let mut harness = SinkHarness::with_lifecycle(FakeLifecycle::default());

    harness
        .run_with_resources_and_progress::<_, (), _>(
            vec![ParallelLeafScanJob::new(source, ())],
            1,
            resources,
            &mut report_record_progress,
            &mut report_current_source_progress,
            move |job, emitter| {
                emitter.begin(ParallelLeafScanBegin::replace(job.source().clone()))?;
                for index in 0..32 {
                    let stage = if index % 2 == 0 {
                        SourceBackedCurrentSourceProgressStage::Parsing
                    } else {
                        SourceBackedCurrentSourceProgressStage::IndexWriting
                    };
                    scanner_resources.record_intermediate_activity(stage);
                    std::thread::sleep(Duration::from_millis(15));
                }
                emitter.complete(ParallelLeafScanComplete::replace(
                    test_certificate(job.source(), 39, 0, false),
                    (),
                ))?;
                Ok(())
            },
        )
        .unwrap();

    let observations = observations.into_inner();
    assert!(
        observations.len() >= 3,
        "expected repeated liveness callbacks"
    );
    assert!(
        observations.windows(2).all(|window| {
            window[1].0.saturating_duration_since(window[0].0) >= Duration::from_millis(100)
        }),
        "all stages must share the same route-wide 100 ms gate: {observations:?}"
    );
}

#[test]
fn intermediate_callback_failure_cancels_and_joins_worker_promptly() {
    let source = test_source(40);
    let resources = SourceBackedRouteResources::production(1);
    let scanner_resources = resources.clone();
    let worker_observed_cancellation = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&worker_observed_cancellation);
    let callback_count = AtomicUsize::new(0);
    let mut report_record_progress = |_delta: SourceBackedRecordProgressDelta| Ok(());
    let mut report_current_source_progress = |_progress: SourceBackedCurrentSourceProgress| {
        callback_count.fetch_add(1, Ordering::SeqCst);
        Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Internal,
            "injected intermediate progress failure",
        ))
    };
    let mut harness = SinkHarness::with_lifecycle(FakeLifecycle::default());
    let started = Instant::now();

    let error = harness
        .run_with_resources_and_progress::<_, (), _>(
            vec![ParallelLeafScanJob::new(source, ())],
            1,
            resources,
            &mut report_record_progress,
            &mut report_current_source_progress,
            move |job, emitter| {
                emitter.begin(ParallelLeafScanBegin::replace(job.source().clone()))?;
                scanner_resources
                    .record_intermediate_activity(SourceBackedCurrentSourceProgressStage::Parsing);
                while !emitter.is_cancelled() {
                    std::thread::yield_now();
                }
                worker_cancelled.store(true, Ordering::Release);
                Err(ParallelLeafScanCancelled.into())
            },
        )
        .unwrap_err();

    assert!(matches!(error, ParallelLeafScanError::Activity { .. }));
    assert_eq!(callback_count.load(Ordering::SeqCst), 1);
    assert!(worker_observed_cancellation.load(Ordering::Acquire));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "callback failure must cancel and join without an unbounded wait"
    );
}

#[test]
fn batch_validates_every_source_before_writing_and_propagates_progress_errors() {
    let temp = tempdir();
    let index_root = temp.path().join("index");
    let expected = test_source(35);
    let observed = test_source(36);
    let resources = SourceBackedRouteResources::production(1);
    let mut progress = Vec::new();
    let mut report_progress = |delta| {
        progress.push(delta);
        Ok(())
    };
    let records = vec![
        test_core_record(&expected, 1, 35),
        test_core_record(&observed, 2, 35),
        test_core_record(&expected, 3, 35),
    ];
    let mut harness = SinkHarness::open(&index_root);
    let error = harness
        .run_with_resources_and_record_progress::<_, (), _>(
            vec![ParallelLeafScanJob::new(expected.clone(), ())],
            1,
            resources.clone(),
            &mut report_progress,
            move |job, emitter| {
                emitter.begin(ParallelLeafScanBegin::replace(job.source().clone()))?;
                emitter.emit_core_records(records.clone())?;
                Ok(())
            },
        )
        .unwrap_err();
    assert!(matches!(
        error,
        ParallelLeafScanError::Protocol(ParallelLeafScanProtocolError::SourceMismatch {
            job_index: 0,
            message: ParallelLeafScanMessageKind::CoreRecordBatch,
            ..
        })
    ));
    assert!(
        progress.is_empty(),
        "the whole batch must validate before writes"
    );
    assert_eq!(
        resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        0
    );

    let source = test_source(37);
    let records = (1..=3)
        .map(|sequence| test_core_record(&source, sequence, 37))
        .collect::<Vec<_>>();
    let resources = SourceBackedRouteResources::production(1);
    let mut accepted = 0_u64;
    let mut fail_progress = |delta: SourceBackedRecordProgressDelta| {
        accepted = accepted.saturating_add(delta.accepted_records);
        if accepted == 3 {
            return Err(SourceBackedCoordinatorError::Progress(
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "injected batch progress failure",
                ),
            ));
        }
        Ok(())
    };
    let mut harness = SinkHarness::open(&temp.path().join("progress-index"));
    let error = harness
        .run_with_resources_and_record_progress::<_, (), _>(
            vec![ParallelLeafScanJob::new(source, ())],
            1,
            resources.clone(),
            &mut fail_progress,
            move |job, emitter| {
                emitter.begin(ParallelLeafScanBegin::replace(job.source().clone()))?;
                emitter.emit_core_records(records.clone())?;
                Ok(())
            },
        )
        .unwrap_err();
    let ParallelLeafScanError::Sink {
        operation: ParallelLeafSinkOperation::AddCoreRecordBatch,
        source,
        ..
    } = error
    else {
        panic!("expected add-core-record-batch sink failure");
    };
    assert!(matches!(
        *source,
        SourceBackedCoordinatorError::Progress(SourceBackedRouteError {
            ref detail,
            ..
        }) if detail == "injected batch progress failure"
    ));
    assert_eq!(accepted, 3);
    assert_eq!(
        resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        0,
        "a coordinator error must release the accepted and unconsumed batch reservations"
    );
}

#[test]
fn prepared_record_bytes_are_reserved_before_lifecycle_acceptance() {
    let source = test_source(19);
    let record = test_core_record(&source, 1, 191);
    let preparation = FakePreparation;
    let prepared_bytes = preparation.prepare(record.clone()).unwrap().encoded_bytes;

    let one_under = SourceBackedRouteResources::for_test(
        4,
        u64::try_from(prepared_bytes - 1).unwrap(),
        u64::MAX,
    );
    let error = CoreRecordEmission::new(record.clone(), &one_under, &preparation)
        .map_err(SourceBackedRouteError::from)
        .unwrap_err();
    assert_eq!(error.kind, SourceBackedRouteErrorKind::ResourceUnavailable);
    assert_eq!(
        one_under.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        0
    );

    let exact =
        SourceBackedRouteResources::for_test(1, u64::try_from(prepared_bytes).unwrap(), u64::MAX);
    let emission = CoreRecordEmission::new(record, &exact, &preparation).unwrap();
    assert_eq!(
        exact.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        u64::try_from(prepared_bytes).unwrap()
    );
    let (_prepared, reservation) = emission.into_prepared();
    assert_eq!(
        exact.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        u64::try_from(prepared_bytes).unwrap(),
        "the reservation must outlive lifecycle acceptance"
    );
    drop(reservation);
    assert_eq!(
        exact.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        0
    );
}
#[test]
fn oversized_valid_core_record_is_rejected_by_the_emission_envelope() {
    let source = test_source(15);
    let mut record = test_core_record(&source, 1, 151);
    record.content.normalized_body = Some("\0".repeat(MAX_CORE_CONTENT_BYTES));
    record.validate_contract().unwrap();

    let error = run_single(source, move |job, emitter| {
        emitter.begin(ParallelLeafScanBegin::replace(job.source().clone()))?;
        emitter.emit_core_record(record.clone())?;
        Ok(())
    })
    .unwrap_err();

    assert!(matches!(
        error,
        ParallelLeafScanError::Worker {
            job_index: 0,
            source: TestWorkerFailure::Emission(SourceBackedRouteError {
                kind: SourceBackedRouteErrorKind::InvalidSource,
                ..
            }),
            ..
        }
    ));
}

fn run_single<F>(source: SourceKey, scan: F) -> TestRunResult<()>
where
    F: Fn(
            &ParallelLeafScanJob<()>,
            &mut ParallelLeafScanEmitter<'_, (), TestWorkerFailure>,
        ) -> TestWorkerResult
        + Sync,
{
    let temp = tempdir();
    let mut harness = SinkHarness::open(&temp.path().join("index"));
    harness.run(vec![ParallelLeafScanJob::new(source, ())], 1, scan)
}

fn publish_append_base(
    _index_root: &std::path::Path,
    source: &SourceKey,
    revision: u8,
) -> CertifiedSource {
    test_certificate(source, revision, 1, true)
}

pub(super) fn test_source(id: u8) -> SourceKey {
    SourceKey::derive(
        "parallel-leaf-test",
        "parallel_leaf_fixture",
        "parallel-leaf-fixture-v1",
        1,
        SourceAnchor::CatalogLineage([id; 32]),
    )
    .unwrap()
}

pub(super) fn test_certificate(
    source: &SourceKey,
    revision: u8,
    document_count: u64,
    appendable: bool,
) -> CertifiedSource {
    let digest = [revision; 32];
    let observation =
        SourceObservation::new(source.clone(), "parallel-leaf-revision-v1", vec![revision])
            .unwrap();
    let counts = ScannedSourceCounts {
        complete_records: document_count,
        retained_records: document_count,
        rejected_records: 0,
        ignored_records: 0,
        indexed_documents: document_count,
        certified_bytes: document_count,
    };
    let frontier = appendable.then(|| {
        SourceFrontier::new(
            "parallel-leaf-frontier-v1",
            TypedKey::U64(document_count),
            document_count,
            digest,
        )
        .unwrap()
    });
    CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        "parallel-leaf-parser-v1",
        digest,
        counts,
        frontier,
    )
    .unwrap()
}

pub(super) fn test_core_record(source: &SourceKey, sequence: u64, revision: u8) -> CoreRecord {
    let native_session_key =
        NativeSessionKey::native_id("parallel.session", TypedKey::U64(1)).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "parallel-session",
        native_session_key: &native_session_key,
    })
    .unwrap();
    let native_item_key =
        NativeItemKey::native_id("parallel.event", TypedKey::U64(sequence)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "parallel-event",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        sequence,
        "message",
        format!("parallel-leaf-parser-{revision}"),
        format!("parallel leaf Core record {sequence}"),
    )
    .unwrap();
    record.provider_session_id = Some("parallel-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(sequence));
    record.occurred_at_unix_ms = i64::try_from(sequence).ok();
    record.role = Some("user".to_owned());
    record
}
