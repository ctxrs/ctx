use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Barrier, Mutex,
    },
    time::Duration,
};

use crate::provider::source_backed::{
    CoreRecordEmission, SourceBackedCoordinatorError, SourceBackedCoordinatorResult,
    SourceBackedLogicalSourceFailures, SourceBackedRecordProgressDelta,
    SourceBackedRecordRejectionClass, SourceBackedRecordRejectionDraft,
    SourceBackedRecordRejectionDrafts, SourceBackedRecordRejections, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteResourceKind, SourceBackedRouteResources,
    SOURCE_BACKED_CORE_RECORD_BATCH_MAX_RECORDS,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceAppend,
    CoreRecord, EventIdentityInput, NativeItemKey, NativeSessionKey, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceFrontier, SourceObservation, TypedKey,
    MAX_CORE_CONTENT_BYTES,
};
use ctx_history_index::{
    CommitReceipt, GenerationWriter, SourceRouteIdentity, VerifiedIndex, WriterOptions,
};

use super::super::{CompleteInventoryOwner, SourceOwner};
use super::*;

#[derive(Debug, thiserror::Error)]
enum TestWorkerFailure {
    #[error("injected worker failure")]
    Injected,
    #[error(transparent)]
    Emission(#[from] SourceBackedRouteError),
}

impl From<ParallelLeafScanEmitError> for ParallelLeafScanWorkerError<TestWorkerFailure> {
    fn from(error: ParallelLeafScanEmitError) -> Self {
        match error {
            ParallelLeafScanEmitError::Cancelled(error) => Self::Cancelled(error),
            ParallelLeafScanEmitError::Route(error) => {
                Self::Provider(TestWorkerFailure::Emission(error))
            }
        }
    }
}

type TestWorkerResult = Result<(), ParallelLeafScanWorkerError<TestWorkerFailure>>;
type TestRunResult<R> = Result<Vec<R>, ParallelLeafScanError<TestWorkerFailure>>;

fn test_route_identity() -> SourceRouteIdentity {
    SourceRouteIdentity::from_sha256("00".repeat(32)).unwrap()
}

struct SinkHarness {
    writer: GenerationWriter,
    owners: HashMap<[u8; 32], SourceOwner>,
    complete_inventories: Vec<CompleteInventoryOwner>,
    logical_source_failures: SourceBackedLogicalSourceFailures,
    record_rejections: SourceBackedRecordRejections,
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
            .unwrap()
            .into_writer()
            .unwrap(),
            owners: HashMap::new(),
            complete_inventories: Vec::new(),
            logical_source_failures: SourceBackedLogicalSourceFailures::default(),
            record_rejections: SourceBackedRecordRejections::default(),
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
            core_record_preparer: self.writer.core_record_preparer(),
            writer: &mut self.writer,
            owners: &mut self.owners,
            complete_inventories: &mut self.complete_inventories,
            route_index: 0,
            route_identity: test_route_identity(),
            resources: SourceBackedRouteResources::production(self.leaf_worker_budget),
            logical_source_failures: &mut self.logical_source_failures,
            record_rejections: &mut self.record_rejections,
            applied_removals: &mut Vec::new(),
            record_progress: None,
            current_source_progress: None,
        };
        sink.run_parallel_leaf_scans(jobs, worker_count, scan)
    }

    fn run_with_source_outcomes<L, R, F>(
        &mut self,
        jobs: Vec<ParallelLeafScanJob<L>>,
        worker_count: usize,
        scan: F,
    ) -> Result<Vec<SourceBackedSourceOutcome<R>>, ParallelLeafScanError<TestWorkerFailure>>
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
            core_record_preparer: self.writer.core_record_preparer(),
            writer: &mut self.writer,
            owners: &mut self.owners,
            complete_inventories: &mut self.complete_inventories,
            route_index: 0,
            route_identity: test_route_identity(),
            resources: SourceBackedRouteResources::production(self.leaf_worker_budget),
            logical_source_failures: &mut self.logical_source_failures,
            record_rejections: &mut self.record_rejections,
            applied_removals: &mut Vec::new(),
            record_progress: None,
            current_source_progress: None,
        };
        sink.run_parallel_leaf_scans_with_source_outcomes(jobs, worker_count, scan)
    }

    fn run_with_worker_state<L, R, W, I, F>(
        &mut self,
        jobs: Vec<ParallelLeafScanJob<L>>,
        worker_count: usize,
        initialize_worker: I,
        scan: F,
    ) -> TestRunResult<R>
    where
        L: Send,
        R: Send,
        W: Send,
        I: Fn(usize) -> W,
        F: Fn(
                &mut W,
                &ParallelLeafScanJob<L>,
                &mut ParallelLeafScanEmitter<'_, R, TestWorkerFailure>,
            ) -> TestWorkerResult
            + Sync,
    {
        let mut sink = SourceBackedGenerationSink {
            core_record_preparer: self.writer.core_record_preparer(),
            writer: &mut self.writer,
            owners: &mut self.owners,
            complete_inventories: &mut self.complete_inventories,
            route_index: 0,
            route_identity: test_route_identity(),
            resources: SourceBackedRouteResources::production(self.leaf_worker_budget),
            logical_source_failures: &mut self.logical_source_failures,
            record_rejections: &mut self.record_rejections,
            applied_removals: &mut Vec::new(),
            record_progress: None,
            current_source_progress: None,
        };
        sink.run_parallel_leaf_scans_with_worker_state(jobs, worker_count, initialize_worker, scan)
    }

    fn run_with_resources_and_record_progress<L, R, F>(
        &mut self,
        jobs: Vec<ParallelLeafScanJob<L>>,
        worker_count: usize,
        resources: SourceBackedRouteResources,
        report_progress: &mut dyn FnMut(
            SourceBackedRecordProgressDelta,
        ) -> SourceBackedCoordinatorResult<()>,
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
        let mut applied_removals = Vec::new();
        let mut sink = SourceBackedGenerationSink {
            core_record_preparer: self.writer.core_record_preparer(),
            writer: &mut self.writer,
            owners: &mut self.owners,
            complete_inventories: &mut self.complete_inventories,
            route_index: 0,
            route_identity: test_route_identity(),
            resources,
            logical_source_failures: &mut self.logical_source_failures,
            record_rejections: &mut self.record_rejections,
            applied_removals: &mut applied_removals,
            record_progress: Some(report_progress),
            current_source_progress: None,
        };
        sink.run_parallel_leaf_scans(jobs, worker_count, scan)
    }

    fn record_rejections(&mut self, rejections: SourceBackedRecordRejectionDrafts) {
        let mut applied_removals = Vec::new();
        let mut sink = SourceBackedGenerationSink {
            core_record_preparer: self.writer.core_record_preparer(),
            writer: &mut self.writer,
            owners: &mut self.owners,
            complete_inventories: &mut self.complete_inventories,
            route_index: 0,
            route_identity: test_route_identity(),
            resources: SourceBackedRouteResources::production(self.leaf_worker_budget),
            logical_source_failures: &mut self.logical_source_failures,
            record_rejections: &mut self.record_rejections,
            applied_removals: &mut applied_removals,
            record_progress: None,
            current_source_progress: None,
        };
        sink.record_rejections(rejections);
    }

    fn commit(self) -> CommitReceipt {
        self.writer.commit(|_| true).unwrap()
    }
}

struct TestWorkerState {
    worker_index: usize,
    jobs_seen: usize,
    dropped: Arc<AtomicUsize>,
}

impl Drop for TestWorkerState {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn worker_state_is_initialized_once_per_stripe_and_reused_in_order() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let jobs = (0_u8..8)
        .map(|id| {
            let source = test_source(id);
            ParallelLeafScanJob::new(
                source,
                ReplacementLeaf {
                    id,
                    document_count: 0,
                },
            )
        })
        .collect::<Vec<_>>();
    let initialized = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let initialized_for_workers = Arc::clone(&initialized);
    let dropped_for_workers = Arc::clone(&dropped);
    let mut harness = SinkHarness::open(&temp.path().join("index"));
    let results = harness
        .run_with_worker_state(
            jobs,
            3,
            move |worker_index| {
                initialized_for_workers.fetch_add(1, Ordering::SeqCst);
                TestWorkerState {
                    worker_index,
                    jobs_seen: 0,
                    dropped: Arc::clone(&dropped_for_workers),
                }
            },
            |worker, job, emitter| {
                assert_eq!(usize::from(job.leaf().id) % 3, worker.worker_index);
                worker.jobs_seen = worker.jobs_seen.saturating_add(1);
                let source = job.source().clone();
                emitter.begin(ParallelLeafScanBegin::replace(source.clone()))?;
                emitter.complete(ParallelLeafScanComplete::replace(
                    test_certificate(&source, 1, 0, false),
                    (worker.worker_index, worker.jobs_seen),
                ))?;
                Ok(())
            },
        )
        .unwrap();

    assert_eq!(initialized.load(Ordering::SeqCst), 3);
    assert_eq!(dropped.load(Ordering::SeqCst), 3);
    assert_eq!(
        results,
        vec![
            (0, 1),
            (1, 1),
            (2, 1),
            (0, 2),
            (1, 2),
            (2, 2),
            (0, 3),
            (1, 3)
        ]
    );
}

#[test]
fn worker_state_is_dropped_for_every_stripe_after_provider_failure() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let jobs = (0_u8..4)
        .map(|id| {
            let source = test_source(id);
            ParallelLeafScanJob::new(
                source,
                ReplacementLeaf {
                    id,
                    document_count: 0,
                },
            )
        })
        .collect::<Vec<_>>();
    let initialized = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let initialized_for_workers = Arc::clone(&initialized);
    let dropped_for_workers = Arc::clone(&dropped);
    let mut harness = SinkHarness::open(&temp.path().join("index"));
    let error = harness
        .run_with_worker_state(
            jobs,
            2,
            move |worker_index| {
                initialized_for_workers.fetch_add(1, Ordering::SeqCst);
                TestWorkerState {
                    worker_index,
                    jobs_seen: 0,
                    dropped: Arc::clone(&dropped_for_workers),
                }
            },
            |worker, job, emitter| {
                if job.leaf().id == 0 {
                    return Err(ParallelLeafScanWorkerError::provider(
                        TestWorkerFailure::Injected,
                    ));
                }
                worker.jobs_seen = worker.jobs_seen.saturating_add(1);
                let source = job.source().clone();
                emitter.begin(ParallelLeafScanBegin::replace(source.clone()))?;
                emitter.complete(ParallelLeafScanComplete::replace(
                    test_certificate(&source, 1, 0, false),
                    (),
                ))?;
                Ok(())
            },
        )
        .unwrap_err();

    assert!(matches!(
        error,
        ParallelLeafScanError::Worker {
            source: TestWorkerFailure::Injected,
            ..
        }
    ));
    assert_eq!(initialized.load(Ordering::SeqCst), 2);
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
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
            emitter.complete(ParallelLeafScanComplete::replace(
                test_certificate(
                    &source,
                    job.leaf().id.saturating_add(10),
                    job.leaf().document_count,
                    false,
                ),
                ReplacementResult {
                    id: job.leaf().id,
                    accepted_sequences,
                },
            ))?;
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
fn ready_driven_transport_accepts_a_later_worker_while_the_first_job_is_withheld() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let mut harness = SinkHarness::open(&temp.path().join("index"));
    let jobs = (0_u8..4)
        .map(|id| ParallelLeafScanJob::new(test_source(id.saturating_add(40)), id))
        .collect();
    let rendezvous = Arc::new(Barrier::new(2));
    let scan_rendezvous = Arc::clone(&rendezvous);
    let (later_accepted_sender, later_accepted_receiver) = mpsc::channel();
    let later_accepted_receiver = Mutex::new(later_accepted_receiver);

    let results = harness
        .run(jobs, 2, move |job, emitter| {
            if *job.leaf() < 2 {
                scan_rendezvous.wait();
            }
            if *job.leaf() == 0 {
                let receiver = later_accepted_receiver.lock().unwrap();
                for expected in [1, 3] {
                    let accepted = receiver.recv_timeout(Duration::from_secs(2)).map_err(|_| {
                        ParallelLeafScanWorkerError::provider(TestWorkerFailure::Injected)
                    })?;
                    assert_eq!(accepted, expected);
                }
            }
            emitter.complete(ParallelLeafScanComplete::Skipped {
                result: *job.leaf(),
            })?;
            if *job.leaf() % 2 == 1 {
                later_accepted_sender.send(*job.leaf()).unwrap();
            }
            Ok(())
        })
        .unwrap();

    assert_eq!(results, [0, 1, 2, 3]);
}

#[derive(Debug, PartialEq, Eq)]
struct FailureOrderingSummary {
    failed_outcomes: Vec<bool>,
    diagnostic_sources: Vec<SourceKey>,
    rejection_lines: Vec<u64>,
    omitted_failures: usize,
    omitted_rejections: usize,
}

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
    let temp = crate::test_support_paths::tempdir().unwrap();
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
    let temp = crate::test_support_paths::tempdir().unwrap();
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
fn begin_rendezvous_blocks_worker_until_coordinator_acknowledgement() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let harness = SinkHarness::open(&temp.path().join("index"));
    let source = test_source(36);
    let cancellation = AtomicBool::new(false);
    let resources = SourceBackedRouteResources::production(1);
    let preparer = harness.writer.core_record_preparer();
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
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_root = temp.path().join("index");
    let retained_source = test_source(29);
    let _ = publish_append_base(&index_root, &retained_source, 29);
    let retained_generation = VerifiedIndex::open(&index_root)
        .unwrap()
        .generation_id()
        .to_owned();
    let mut harness = SinkHarness::open(&index_root);
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
    drop(harness);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        retained_generation
    );
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
        core_record_preparer: harness.writer.core_record_preparer(),
        writer: &mut harness.writer,
        owners: &mut harness.owners,
        complete_inventories: &mut harness.complete_inventories,
        route_index: 0,
        route_identity: test_route_identity(),
        resources: SourceBackedRouteResources::production(harness.leaf_worker_budget),
        logical_source_failures: &mut harness.logical_source_failures,
        record_rejections: &mut harness.record_rejections,
        applied_removals: &mut Vec::new(),
        record_progress: None,
        current_source_progress: None,
    };
    assert_eq!(sink.recommended_leaf_workers(0), 0);
    assert_eq!(sink.recommended_leaf_workers(2), 2);
    assert_eq!(sink.recommended_leaf_workers(20), 6);
}

#[test]
fn core_record_transport_is_a_zero_capacity_one_record_rendezvous() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let harness = SinkHarness::open(&temp.path().join("index"));
    let core_record_preparer = harness.writer.core_record_preparer();
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
                resources: SourceBackedRouteResources::production(1),
                core_record_preparer,
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
        assert!(matches!(
            *message,
            ParallelLeafProtocolMessage::CoreRecord(_)
        ));
        finished_receiver.recv().unwrap();
    });
}

#[test]
fn core_record_batch_transport_is_one_bounded_zero_capacity_rendezvous() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let harness = SinkHarness::open(&temp.path().join("index"));
    let core_record_preparer = harness.writer.core_record_preparer();
    let source = test_source(32);
    let records = (1..=3)
        .map(|sequence| test_core_record(&source, sequence, 32))
        .collect::<Vec<_>>();
    let resources = SourceBackedRouteResources::production(1);
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
                resources: resources.clone(),
                core_record_preparer: core_record_preparer.clone(),
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
        let ParallelLeafProtocolMessage::CoreRecordBatch(batch) = *message else {
            panic!("expected one Core-record batch");
        };
        assert_eq!(batch.len(), 3);
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
    };
    let error = emitter.emit_core_records(vec![invalid_record]).unwrap_err();
    assert!(matches!(error, ParallelLeafScanEmitError::Cancelled(_)));
    assert_eq!(
        resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        0
    );
}

#[test]
fn batch_emitter_streams_records_that_fit_individually_but_not_together() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let harness = SinkHarness::open(&temp.path().join("index"));
    let core_record_preparer = harness.writer.core_record_preparer();
    let source = test_source(35);
    let records = (1..=2)
        .map(|sequence| test_core_record(&source, sequence, 35))
        .collect::<Vec<_>>();
    let maximum_record_bytes = records
        .iter()
        .cloned()
        .map(|record| {
            u64::try_from(
                core_record_preparer
                    .prepare(record)
                    .unwrap()
                    .encoded_core_bytes(),
            )
            .unwrap()
        })
        .max()
        .unwrap();
    let resources = SourceBackedRouteResources::for_test(1, maximum_record_bytes, u64::MAX);
    let (sender, receiver) =
        mpsc::sync_channel::<ParallelLeafWorkerEvent<(), TestWorkerFailure>>(0);
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
            };
            finished_sender
                .send(emitter.emit_core_records(records))
                .unwrap();
        });

        for _ in 0..2 {
            let ParallelLeafWorkerEvent::Protocol { message, .. } = receiver.recv().unwrap() else {
                panic!("expected a protocol event");
            };
            let ParallelLeafProtocolMessage::CoreRecordBatch(batch) = *message else {
                panic!("expected a Core-record batch");
            };
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
    let temp = crate::test_support_paths::tempdir().unwrap();
    let harness = SinkHarness::open(&temp.path().join("index"));
    let core_record_preparer = harness.writer.core_record_preparer();
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
            u64::try_from(
                core_record_preparer
                    .prepare(record)
                    .unwrap()
                    .encoded_core_bytes(),
            )
            .unwrap()
        })
        .max()
        .unwrap();
    let resources = SourceBackedRouteResources::for_test(2, maximum_record_bytes, u64::MAX);
    let (sender, receiver) =
        mpsc::sync_channel::<ParallelLeafWorkerEvent<(), TestWorkerFailure>>(0);
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
                ParallelLeafProtocolMessage::CoreRecordBatch(_)
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
    let temp = crate::test_support_paths::tempdir().unwrap();
    let harness = SinkHarness::open(&temp.path().join("index"));
    let core_record_preparer = harness.writer.core_record_preparer();
    let source = test_source(33);
    let records = (1..=u64::try_from(SOURCE_BACKED_CORE_RECORD_BATCH_MAX_RECORDS + 1).unwrap())
        .map(|sequence| test_core_record(&source, sequence, 33))
        .collect::<Vec<_>>();
    let (sender, receiver) =
        mpsc::sync_channel::<ParallelLeafWorkerEvent<(), TestWorkerFailure>>(0);
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
            };
            emitter.emit_core_records(records).unwrap();
        });

        let mut batch_lengths = Vec::new();
        for _ in 0..2 {
            let ParallelLeafWorkerEvent::Protocol { message, .. } = receiver.recv().unwrap() else {
                panic!("expected a protocol event");
            };
            let ParallelLeafProtocolMessage::CoreRecordBatch(batch) = *message else {
                panic!("expected a Core-record batch");
            };
            batch_lengths.push(batch.len());
        }
        assert_eq!(
            batch_lengths,
            [SOURCE_BACKED_CORE_RECORD_BATCH_MAX_RECORDS, 1]
        );
    });
}

#[test]
fn batch_application_preserves_order_accounts_progress_and_releases_reservations() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_root = temp.path().join("index");
    let source = test_source(34);
    let records = (1..=3)
        .map(|sequence| test_core_record(&source, sequence, 34))
        .collect::<Vec<_>>();
    let mut harness = SinkHarness::open(&index_root);
    let preparer = harness.writer.core_record_preparer();
    let prepared_sizes = records
        .iter()
        .cloned()
        .map(|record| {
            u64::try_from(preparer.prepare(record).unwrap().encoded_core_bytes()).unwrap()
        })
        .collect::<Vec<_>>();
    let total_prepared_bytes = prepared_sizes.iter().copied().sum::<u64>();

    let resources = SourceBackedRouteResources::for_test(1, total_prepared_bytes, u64::MAX);
    let progress_resources = resources.clone();
    let mut progress = Vec::new();
    let mut live_bytes_after_acceptance = Vec::new();
    let mut report_progress = |delta| {
        progress.push(delta);
        live_bytes_after_acceptance
            .push(progress_resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput));
        Ok(())
    };
    let job_source = source.clone();
    let emitted_records = records.clone();
    let results = harness
        .run_with_resources_and_record_progress::<_, (), _>(
            vec![ParallelLeafScanJob::new(source.clone(), ())],
            1,
            resources.clone(),
            &mut report_progress,
            move |job, emitter| {
                emitter.begin(ParallelLeafScanBegin::replace(job.source().clone()))?;
                emitter.emit_core_records(emitted_records.clone())?;
                emitter.complete(ParallelLeafScanComplete::replace(
                    test_certificate(&job_source, 34, 3, false),
                    (),
                ))?;
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(results, [()]);
    assert_eq!(
        progress,
        vec![
            SourceBackedRecordProgressDelta {
                accepted_records: 1,
                completed_bytes: 0,
            };
            3
        ]
    );
    assert_eq!(
        live_bytes_after_acceptance,
        [prepared_sizes[1] + prepared_sizes[2], prepared_sizes[2], 0,],
        "each record reservation must be released immediately after writer acceptance"
    );
    assert_eq!(
        resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        0
    );

    let batch_commit = harness.commit();

    let reference_root = temp.path().join("reference-index");
    let mut reference = SinkHarness::open(&reference_root);
    let reference_source = source.clone();
    let reference_records = records.clone();
    reference
        .run(
            vec![ParallelLeafScanJob::new(source, ())],
            1,
            move |job, emitter| {
                emitter.begin(ParallelLeafScanBegin::replace(job.source().clone()))?;
                for record in reference_records.clone() {
                    emitter.emit_core_record(record)?;
                }
                emitter.complete(ParallelLeafScanComplete::replace(
                    test_certificate(&reference_source, 34, 3, false),
                    (),
                ))?;
                Ok(())
            },
        )
        .unwrap();
    let reference_commit = reference.commit();
    assert_eq!(
        batch_commit.generation_id, reference_commit.generation_id,
        "one batch must preserve the canonical order of the equivalent single-record emissions"
    );
}

#[test]
fn batch_validates_every_source_before_writing_and_propagates_progress_errors() {
    let temp = crate::test_support_paths::tempdir().unwrap();
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
        if accepted == 2 {
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
    assert!(matches!(
        error,
        ParallelLeafScanError::Sink {
            operation: ParallelLeafSinkOperation::AddCoreRecordBatch,
            source: SourceBackedCoordinatorError::Progress(SourceBackedRouteError {
                detail,
                ..
            }),
            ..
        } if detail == "injected batch progress failure"
    ));
    assert_eq!(accepted, 2);
    assert_eq!(
        resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        0,
        "a coordinator error must release the accepted and unconsumed batch reservations"
    );
}

#[test]
fn five_prior_repository_certificates_are_counted_before_output_admission() {
    use ctx_history_core::{
        RepositoryAbstention, RepositoryAbstentionReason, RepositoryBinding, RepositoryEvidence,
        RepositoryEvidenceConfidence, RepositoryEvidenceKind, RepositoryLocalRootAuthorization,
        CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
        CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
    };

    let temp = crate::test_support_paths::tempdir().unwrap();
    let index = temp.path().join("index");
    let source = test_source(19);
    let mut prior = test_core_record(&source, 1, 191);
    prior.repository_bindings = (0_u8..5)
        .map(|index| RepositoryBinding {
            binding_id: format!("binding-{index}"),
            logical_repository_id: format!("local:repository-{index}"),
            checkout_id: Some(format!("checkout-{index}")),
            worktree_id: Some(format!("worktree-{index}")),
            aliases: Vec::new(),
            git_object_format: None,
            local_root_authorization: Some(RepositoryLocalRootAuthorization {
                local_root: format!("/repository/{index}"),
                local_root_authorization_fingerprint_revision:
                    CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
                local_root_authorization_fingerprint: [index.saturating_add(1); 32],
                observed_at_unix_ms: i64::from(index).saturating_add(1),
            }),
            evidence: vec![RepositoryEvidence {
                kind: RepositoryEvidenceKind::DeclaredToolWorkdir,
                confidence: RepositoryEvidenceConfidence::High,
            }],
            association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
        })
        .collect();
    prior.validate_contract().unwrap();

    let mut initial = GenerationWriter::open(&index, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial.add_core_record(prior).unwrap();
    initial
        .certify_source(test_certificate(&source, 1, 1, false))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let mut replacement = GenerationWriter::open(&index, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replacement.begin_source(source.clone()).unwrap();
    let mut uncertified = test_core_record(&source, 1, 191);
    uncertified.repository_abstentions = vec![RepositoryAbstention {
        evidence_kind: RepositoryEvidenceKind::DeclaredToolWorkdir,
        reason: RepositoryAbstentionReason::CandidateMissingBeforeCertification,
        detail: None,
        association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    }];
    uncertified.validate_contract().unwrap();
    let uncertified_bytes = uncertified.encode_stored().unwrap().len();
    let preparer = replacement.core_record_preparer();
    let prepared_bytes = preparer
        .prepare(uncertified.clone())
        .unwrap()
        .encoded_core_bytes();
    assert!(prepared_bytes > uncertified_bytes);

    let one_under = SourceBackedRouteResources::for_test(
        4,
        u64::try_from(prepared_bytes - 1).unwrap(),
        u64::MAX,
    );
    let error = CoreRecordEmission::new(uncertified.clone(), &one_under, &preparer).unwrap_err();
    assert_eq!(error.kind, SourceBackedRouteErrorKind::ResourceUnavailable);
    assert_eq!(
        one_under.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        0
    );

    let exact =
        SourceBackedRouteResources::for_test(1, u64::try_from(prepared_bytes).unwrap(), u64::MAX);
    let emission = CoreRecordEmission::new(uncertified, &exact, &preparer).unwrap();
    assert_eq!(
        exact.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        u64::try_from(prepared_bytes).unwrap()
    );
    let (prepared, reservation) = emission.into_prepared();
    replacement.add_prepared_core_record(prepared).unwrap();
    assert_eq!(
        exact.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        u64::try_from(prepared_bytes).unwrap(),
        "the reservation must outlive writer acceptance"
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
    .unwrap()
    .into_writer()
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
