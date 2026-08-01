use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Barrier, Mutex,
    },
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceAppend, CoreRecord,
    CoreRecordError, EventIdentityInput, NativeItemKey, NativeSessionKey, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceFrontier, SourceObservation, TypedKey,
    MAX_CORE_CONTENT_BYTES, MAX_ENCODED_CORE_RECORD_BYTES,
};
use ctx_history_index::{
    CommitReceipt, GenerationWriter, IndexError, VerifiedIndex, WriterOptions,
};

use super::super::{CompleteInventoryOwner, SourceBackedCoordinatorError, SourceOwner};
use super::*;

#[derive(Debug, thiserror::Error)]
#[error("injected worker failure")]
struct TestWorkerFailure;

type TestWorkerResult = Result<(), ParallelLeafScanWorkerError<TestWorkerFailure>>;
type TestRunResult<R> = Result<Vec<R>, ParallelLeafScanError<TestWorkerFailure>>;

struct SinkHarness {
    writer: GenerationWriter,
    owners: HashMap<[u8; 32], SourceOwner>,
    complete_inventories: Vec<CompleteInventoryOwner>,
    leaf_worker_budget: usize,
}

impl SinkHarness {
    fn open(index_root: &std::path::Path) -> Self {
        Self {
            writer: GenerationWriter::open(
                index_root,
                WriterOptions {
                    indexer_threads: 1,
                    memory_bytes: 15_000_000,
                },
            )
            .unwrap(),
            owners: HashMap::new(),
            complete_inventories: Vec::new(),
            leaf_worker_budget: 16,
        }
    }

    fn run<L, R, F>(
        &mut self,
        jobs: Vec<ParallelLeafScanJob<L>>,
        worker_count: usize,
        scan: F,
    ) -> TestRunResult<R>
    where
        L: Send,
        R: Send,
        F: Fn(
                &ParallelLeafScanJob<L>,
                &mut ParallelLeafScanEmitter<'_, R, TestWorkerFailure>,
            ) -> TestWorkerResult
            + Sync,
    {
        let mut sink = SourceBackedGenerationSink {
            writer: &mut self.writer,
            owners: &mut self.owners,
            complete_inventories: &mut self.complete_inventories,
            route_index: 0,
            leaf_worker_budget: self.leaf_worker_budget,
        };
        sink.run_parallel_leaf_scans(jobs, worker_count, scan)
    }

    fn commit(self) -> CommitReceipt {
        self.writer.commit(|_| true).unwrap()
    }
}

#[derive(Debug)]
struct ReplacementLeaf {
    id: u8,
    document_count: u64,
}

#[derive(Debug, PartialEq, Eq)]
struct ReplacementResult {
    id: u8,
    accepted_sequences: Vec<u64>,
}

#[derive(Debug, PartialEq, Eq)]
struct ReplacementSummary {
    results: Vec<ReplacementResult>,
    generation_id: String,
    indexed_documents: u64,
    certified_sources: usize,
    sources: Vec<CertifiedSource>,
    stored_sequences: Vec<Vec<u64>>,
}

#[test]
fn forced_one_and_four_workers_preserve_semantics_and_input_order() {
    let one = run_replacements(1);
    let four = run_replacements(4);
    let four_again = run_replacements(4);

    assert_eq!(one, four);
    assert_eq!(four, four_again);
    assert_eq!(
        one.results
            .iter()
            .map(|result| result.id)
            .collect::<Vec<_>>(),
        (0_u8..8).collect::<Vec<_>>()
    );
    assert!(one
        .results
        .iter()
        .all(|result| result.accepted_sequences == [1, 2]));
}

fn run_replacements(worker_count: usize) -> ReplacementSummary {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_root = temp.path().join("index");
    let sources = (0_u8..8).map(test_source).collect::<Vec<_>>();
    let jobs = sources
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, source)| {
            ParallelLeafScanJob::new(
                source,
                ReplacementLeaf {
                    id: u8::try_from(index).unwrap(),
                    document_count: 2,
                },
            )
        })
        .collect();
    let mut harness = SinkHarness::open(&index_root);
    let results = harness
        .run(jobs, worker_count, |job, emitter| {
            let source = job.source().clone();
            emitter.begin(ParallelLeafScanBegin::Replace {
                source: source.clone(),
            })?;
            let mut accepted_sequences = Vec::new();
            for sequence in 1..=job.leaf().document_count {
                emitter.emit_core_record(test_core_record(
                    &source,
                    sequence,
                    job.leaf().id.saturating_add(10),
                ))?;
                accepted_sequences.push(sequence);
            }
            emitter.complete(ParallelLeafScanComplete::Replace {
                certificate: test_certificate(
                    &source,
                    job.leaf().id.saturating_add(10),
                    job.leaf().document_count,
                    false,
                ),
                result: ReplacementResult {
                    id: job.leaf().id,
                    accepted_sequences,
                },
            })?;
            Ok(())
        })
        .unwrap();
    let commit = harness.commit();
    let verified = VerifiedIndex::open(&index_root).unwrap();
    let stored_sequences = sources
        .iter()
        .map(|source| {
            verified
                .source_event_page(source, None, 8)
                .unwrap()
                .items
                .into_iter()
                .map(|event| event.event_sequence)
                .collect()
        })
        .collect();
    ReplacementSummary {
        results,
        generation_id: commit.generation_id.clone(),
        indexed_documents: commit.indexed_documents,
        certified_sources: commit.certified_sources,
        sources: commit.manifest().sources.clone(),
        stored_sequences,
    }
}

#[test]
fn a_barrier_proves_all_forced_workers_scan_concurrently() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let mut harness = SinkHarness::open(&temp.path().join("index"));
    let jobs = (0_u8..8)
        .map(|id| ParallelLeafScanJob::new(test_source(id), id))
        .collect();
    let barrier = Arc::new(Barrier::new(4));
    let scan_barrier = Arc::clone(&barrier);
    let observed_workers = Arc::new(Mutex::new(HashSet::new()));
    let scan_workers = Arc::clone(&observed_workers);

    let results = harness
        .run(jobs, 4, move |job, emitter| {
            let thread = std::thread::current();
            scan_workers
                .lock()
                .unwrap()
                .insert((thread.id(), thread.name().unwrap_or_default().to_owned()));
            scan_barrier.wait();
            emitter.complete(ParallelLeafScanComplete::Skipped {
                result: *job.leaf(),
            })?;
            Ok(())
        })
        .unwrap();

    assert_eq!(results, (0_u8..8).collect::<Vec<_>>());
    let observed_workers = Arc::try_unwrap(observed_workers)
        .unwrap()
        .into_inner()
        .unwrap();
    assert_eq!(observed_workers.len(), 4);
    assert_eq!(
        observed_workers
            .into_iter()
            .map(|(_, name)| name)
            .collect::<HashSet<_>>(),
        (0..4).map(source_worker_thread_name).collect()
    );
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
fn append_and_skipped_jobs_use_typed_lifecycles_and_ordered_results() {
    let temp = crate::test_support_paths::tempdir().unwrap();
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
    let mut harness = SinkHarness::open(&index_root);

    let results = harness
        .run(jobs, 2, |job, emitter| {
            if *job.leaf() {
                emitter.begin(ParallelLeafScanBegin::Append {
                    source: job.source().clone(),
                    base: Box::new(base.clone()),
                })?;
                emitter.emit_core_record(test_core_record(job.source(), 2, 12))?;
                emitter.complete(ParallelLeafScanComplete::Append {
                    append: append.clone(),
                    result: "append",
                })?;
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
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_root = temp.path().join("index");
    let source = test_source(3);
    let _base = publish_append_base(&index_root, &source, 31);
    let wrong_base = test_certificate(&source, 32, 1, true);
    let mut harness = SinkHarness::open(&index_root);
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
    let temp = crate::test_support_paths::tempdir().unwrap();
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
                return Err(ParallelLeafScanWorkerError::provider(TestWorkerFailure));
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

struct PanicOnDrop;

impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        panic!("injected unguarded worker drop panic");
    }
}

#[test]
fn unguarded_worker_panic_is_reported_from_mandatory_join() {
    let temp = crate::test_support_paths::tempdir().unwrap();
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
fn worker_budget_reserves_indexers_runtime_and_caps_scanners() {
    assert_eq!(leaf_worker_budget_for_parallelism(8, 32), 16);
    assert_eq!(leaf_worker_budget_for_parallelism(usize::MAX, 32), 16);
    assert_eq!(leaf_worker_budget_for_parallelism(4, 10), 4);
    assert_eq!(leaf_worker_budget_for_parallelism(8, 4), 1);

    let temp = crate::test_support_paths::tempdir().unwrap();
    let mut harness = SinkHarness::open(&temp.path().join("index"));
    harness.leaf_worker_budget = 6;
    let sink = SourceBackedGenerationSink {
        writer: &mut harness.writer,
        owners: &mut harness.owners,
        complete_inventories: &mut harness.complete_inventories,
        route_index: 0,
        leaf_worker_budget: harness.leaf_worker_budget,
    };
    assert_eq!(sink.recommended_leaf_workers(0), 0);
    assert_eq!(sink.recommended_leaf_workers(2), 2);
    assert_eq!(sink.recommended_leaf_workers(20), 6);
}

#[test]
fn core_record_transport_is_a_zero_capacity_one_record_rendezvous() {
    let source = test_source(14);
    let record = test_core_record(&source, 1, 141);
    let (sender, receiver) =
        mpsc::sync_channel::<ParallelLeafWorkerEvent<(), TestWorkerFailure>>(0);
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
        assert!(matches!(
            event,
            ParallelLeafWorkerEvent::Protocol {
                worker_index: 0,
                job_index: 0,
                message: ParallelLeafProtocolMessage::CoreRecord(_),
            }
        ));
        finished_receiver.recv().unwrap();
    });
}

#[test]
fn oversized_valid_core_record_is_rejected_typed_by_the_generation_writer() {
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
        ParallelLeafScanError::Sink {
            job_index: 0,
            operation: ParallelLeafSinkOperation::AddCoreRecord,
            source: SourceBackedCoordinatorError::Index(IndexError::CoreRecord(
                CoreRecordError::FieldTooLarge {
                    field: "encoded_core_record",
                    maximum: MAX_ENCODED_CORE_RECORD_BYTES,
                    ..
                }
            )),
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
    let temp = crate::test_support_paths::tempdir().unwrap();
    let mut harness = SinkHarness::open(&temp.path().join("index"));
    harness.run(vec![ParallelLeafScanJob::new(source, ())], 1, scan)
}

fn publish_append_base(
    index_root: &std::path::Path,
    source: &SourceKey,
    revision: u8,
) -> CertifiedSource {
    let certificate = test_certificate(source, revision, 1, true);
    let mut writer = GenerationWriter::open(
        index_root,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(test_core_record(source, 1, revision))
        .unwrap();
    writer.certify_source(certificate.clone()).unwrap();
    writer.commit(|_| true).unwrap();
    certificate
}

fn test_source(id: u8) -> SourceKey {
    SourceKey::derive(
        "parallel-leaf-test",
        "parallel_leaf_fixture",
        "parallel-leaf-fixture-v1",
        1,
        SourceAnchor::CatalogLineage([id; 32]),
    )
    .unwrap()
}

fn test_certificate(
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

fn test_core_record(source: &SourceKey, sequence: u64, revision: u8) -> CoreRecord {
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
        session_id,
        source.clone(),
        sequence,
        "message",
        "primary",
        true,
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
