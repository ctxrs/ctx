use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc, Barrier, Mutex,
    },
    time::{Duration, Instant},
};

use ctx_history_capture_model::SourceRouteIdentity;
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceAppend,
    CertifiedSourceDeletion, CertifiedSourceInventory, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceFrontier, SourceKey, SourceObservation, TypedKey, MAX_CORE_CONTENT_BYTES,
};

use crate::{
    BaseEventLookup, CaptureCommitOutcome, CaptureCommitReceipt, CaptureLifecycleOpenOutcome,
    CaptureLifecycleSink, CapturePublicationContext, CapturePublicationDisposition,
    CaptureRevalidationTarget, CaptureRouteRef, CaptureSourceAggregateRef, CompleteDocumentTree,
    CompleteInventoryOwner, CoreMaterialization, CorePreparationFailureKind, CorePreparationPort,
    DocumentInventoryAuthority, DocumentLeafFingerprint, DocumentRecordSpool,
    DocumentSourceTerminal, ImmutableCaptureSnapshot, ObservedDocumentLeaf, PresentCaptureRoute,
    ReplacementDocumentTree, SourceBackedCoordinatorError as RuntimeSourceBackedCoordinatorError,
    SourceBackedCurrentSourceProgress, SourceBackedCurrentSourceProgressStage,
    SourceBackedGenerationSink as RuntimeSourceBackedGenerationSink,
    SourceBackedLogicalSourceFailures, SourceBackedRecordProgressDelta,
    SourceBackedRecordRejectionClass, SourceBackedRecordRejectionDraft,
    SourceBackedRecordRejectionDrafts, SourceBackedRecordRejections, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteResourceKind, SourceBackedRouteResources,
    SourceBackedRouteResult, SourceOwner, VerifiedCapture, CORE_RECORD_BATCH_MAX_RECORDS,
};

use super::*;

mod document_lifecycle;

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

#[derive(Debug, thiserror::Error)]
enum FakeLifecycleError {
    #[error("fake lifecycle invariant: {0}")]
    Invariant(&'static str),
    #[error("fake Core preparation failed: {0}")]
    Preparation(String),
    #[error("fake lifecycle contract: {0}")]
    Contract(&'static str),
}

#[derive(Clone, Default)]
struct FakeLookup;

impl BaseEventLookup for FakeLookup {
    type Error = FakeLifecycleError;

    fn contains(&self, _event_id: uuid::Uuid) -> Result<bool, Self::Error> {
        Ok(false)
    }
}

#[derive(Clone, Default)]
struct FakePreparation;

#[derive(Debug)]
struct FakePrepared {
    record: CoreRecord,
    encoded_bytes: usize,
}

impl CorePreparationPort for FakePreparation {
    type Prepared = FakePrepared;
    type Draft = CoreRecord;
    type Failure = FakeLifecycleError;

    fn prepare(&self, record: CoreRecord) -> Result<Self::Prepared, Self::Failure> {
        let encoded_bytes = record
            .encode_stored()
            .map_err(|error| FakeLifecycleError::Preparation(error.to_string()))?
            .len();
        Ok(FakePrepared {
            record,
            encoded_bytes,
        })
    }

    fn prepare_draft(&self, record: CoreRecord) -> Result<Self::Draft, Self::Failure> {
        record
            .validate_contract()
            .map_err(|error| FakeLifecycleError::Preparation(error.to_string()))?;
        Ok(record)
    }

    fn materialize_draft(
        &self,
        draft: Self::Draft,
        maximum_encoded_bytes: usize,
    ) -> Result<CoreMaterialization<Self::Prepared, Self::Draft>, Self::Failure> {
        let encoded_bytes = draft
            .encode_stored()
            .map_err(|error| FakeLifecycleError::Preparation(error.to_string()))?
            .len();
        if encoded_bytes > maximum_encoded_bytes {
            return Ok(CoreMaterialization::CapacityExceeded(Box::new(draft)));
        }
        Ok(CoreMaterialization::Prepared(FakePrepared {
            record: draft,
            encoded_bytes,
        }))
    }

    fn prepared_source<'a>(&self, prepared: &'a Self::Prepared) -> &'a SourceKey {
        &prepared.record.source
    }

    fn encoded_bytes(&self, prepared: &Self::Prepared) -> usize {
        prepared.encoded_bytes
    }

    fn failure_kind(&self, _failure: &Self::Failure) -> CorePreparationFailureKind {
        CorePreparationFailureKind::InvalidSource
    }
}

#[derive(Clone, Default)]
struct FakeSnapshot {
    sources: Vec<CertifiedSource>,
    route_identity: Option<SourceRouteIdentity>,
    route_sources: Vec<SourceKey>,
    records: Vec<CoreRecord>,
}

impl FakeSnapshot {
    fn stored_sequences(&self, source: &SourceKey) -> Vec<u64> {
        self.records
            .iter()
            .filter(|record| record.source.exact_descriptor_eq(source))
            .map(|record| record.event_sequence)
            .collect()
    }
}

impl ImmutableCaptureSnapshot for FakeSnapshot {
    fn sources(&self) -> &[CertifiedSource] {
        &self.sources
    }

    fn source_aggregates(&self) -> impl ExactSizeIterator<Item = CaptureSourceAggregateRef<'_>> {
        std::iter::empty()
    }

    fn source_routes(&self) -> impl ExactSizeIterator<Item = CaptureRouteRef<'_>> {
        self.route_identity
            .as_ref()
            .map(|identity| CaptureRouteRef::new(identity, &self.route_sources, false))
            .into_iter()
    }

    fn source_route(&self, route_identity: &SourceRouteIdentity) -> Option<CaptureRouteRef<'_>> {
        self.route_identity
            .as_ref()
            .filter(|identity| *identity == route_identity)
            .map(|identity| CaptureRouteRef::new(identity, &self.route_sources, false))
    }
}

#[derive(Default)]
struct FakeLifecycle {
    base_sources: Vec<CertifiedSource>,
    current_source: Option<SourceKey>,
    records: Vec<CoreRecord>,
    certified_sources: Vec<CertifiedSource>,
    retained_unstaged_routes: usize,
    add_prepared_delay: Duration,
    add_prepared_gate: Option<AddPreparedGate>,
}

#[derive(Clone)]
struct AddPreparedGate {
    entered: Arc<AtomicBool>,
    release: Arc<Mutex<mpsc::Receiver<bool>>>,
}

impl AddPreparedGate {
    fn channel() -> (Self, Arc<AtomicBool>, mpsc::SyncSender<bool>) {
        let entered = Arc::new(AtomicBool::new(false));
        let (release, wait_for_release) = mpsc::sync_channel(1);
        (
            Self {
                entered: Arc::clone(&entered),
                release: Arc::new(Mutex::new(wait_for_release)),
            },
            entered,
            release,
        )
    }
}

impl FakeLifecycle {
    fn with_base(base: CertifiedSource) -> Self {
        Self {
            base_sources: vec![base],
            ..Self::default()
        }
    }

    fn with_add_prepared_delay(delay: Duration) -> Self {
        Self {
            add_prepared_delay: delay,
            ..Self::default()
        }
    }

    fn with_add_prepared_gate(gate: AddPreparedGate) -> Self {
        Self {
            add_prepared_gate: Some(gate),
            ..Self::default()
        }
    }

    fn snapshot(&self) -> FakeSnapshot {
        let mut sources = self.certified_sources.clone();
        sources.sort_by(|left, right| {
            left.observation()
                .source()
                .cmp(right.observation().source())
        });
        FakeSnapshot {
            route_identity: Some(test_route_identity()),
            route_sources: sources
                .iter()
                .map(|source| source.observation().source().clone())
                .collect(),
            sources,
            records: self.records.clone(),
        }
    }

    fn commit_receipt(self) -> CaptureCommitReceipt<FakeSnapshot> {
        let snapshot = self.snapshot();
        let indexed_documents = snapshot
            .sources
            .iter()
            .map(|source| source.counts().indexed_documents)
            .sum();
        CaptureCommitReceipt::new(
            "fake-generation".to_owned(),
            1,
            indexed_documents,
            snapshot.sources.len(),
            snapshot
                .sources
                .iter()
                .map(|source| source.counts().certified_bytes)
                .sum(),
            snapshot,
        )
    }
}

impl CaptureLifecycleSink for FakeLifecycle {
    type Error = FakeLifecycleError;
    type OpenOptions = ();
    type BaseLookup = FakeLookup;
    type Preparation = FakePreparation;
    type PinnedAppendBase = CertifiedSource;
    type CommittedSnapshot = FakeSnapshot;
    type VerifiedPublication = ();
    type Snapshot<'a> = FakeSnapshot;

    fn invariant_error(detail: &'static str) -> Self::Error {
        FakeLifecycleError::Invariant(detail)
    }

    fn open(
        _root: &Path,
        _options: Self::OpenOptions,
    ) -> Result<CaptureLifecycleOpenOutcome<Self>, Self::Error> {
        Ok(CaptureLifecycleOpenOutcome::Ready(Self::default()))
    }

    fn base_snapshot(&self) -> Option<Self::Snapshot<'_>> {
        (!self.base_sources.is_empty()).then(|| FakeSnapshot {
            sources: self.base_sources.clone(),
            route_identity: Some(test_route_identity()),
            route_sources: self
                .base_sources
                .iter()
                .map(|source| source.observation().source().clone())
                .collect(),
            records: Vec::new(),
        })
    }

    fn base_source(&self, source: &SourceKey) -> Option<&CertifiedSource> {
        self.base_sources
            .iter()
            .find(|candidate| candidate.observation().source().exact_descriptor_eq(source))
    }

    fn pinned_append_base(
        &self,
        _route_identity: &SourceRouteIdentity,
        source: &SourceKey,
    ) -> Option<Self::PinnedAppendBase> {
        self.base_source(source).cloned()
    }

    fn pinned_append_base_source(base: &Self::PinnedAppendBase) -> &CertifiedSource {
        base
    }

    fn base_event_lookup(&self) -> Self::BaseLookup {
        FakeLookup
    }

    fn core_preparation(&self) -> Self::Preparation {
        FakePreparation
    }

    fn set_route_plan(
        &mut self,
        _selected: BTreeSet<SourceRouteIdentity>,
        _carried_from_base: BTreeSet<SourceRouteIdentity>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn begin_route_stage(
        &mut self,
        _route_identity: SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn retain_unstaged_route_members(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        self.retained_unstaged_routes = self.retained_unstaged_routes.saturating_add(1);
        Ok(())
    }

    fn route_retains_unstaged_members(&self, _route_identity: &SourceRouteIdentity) -> bool {
        false
    }

    fn register_route_revalidation(
        &mut self,
        _route_identity: SourceRouteIdentity,
        _revalidate: impl Fn() -> bool + Send + 'static,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn visit_revalidation_targets<E>(
        &self,
        mut visit: impl for<'a> FnMut(CaptureRevalidationTarget<'a>) -> Result<(), E>,
    ) -> Result<Result<(), E>, Self::Error> {
        for source in &self.certified_sources {
            if let Err(error) = visit(CaptureRevalidationTarget::Source(source)) {
                return Ok(Err(error));
            }
        }
        Ok(Ok(()))
    }

    fn finish_route_stage(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn rollback_route_stage(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        self.current_source = None;
        Ok(())
    }

    fn authorize_carried_route_retirement(
        &mut self,
        _replacement_route: &SourceRouteIdentity,
        _retired_route: &SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn retire_carried_route(
        &mut self,
        _replacement_route: &SourceRouteIdentity,
        _retired_route: &SourceRouteIdentity,
    ) -> Result<Vec<SourceKey>, Self::Error> {
        Ok(Vec::new())
    }

    fn begin_source_replace(&mut self, source: SourceKey) -> Result<(), Self::Error> {
        self.current_source = Some(source);
        Ok(())
    }

    fn begin_source_append(&mut self, source: SourceKey) -> Result<&CertifiedSource, Self::Error> {
        self.current_source = Some(source.clone());
        self.base_source(&source)
            .ok_or(FakeLifecycleError::Contract("append source has no base"))
    }

    fn begin_source_append_from_base(
        &mut self,
        base: Self::PinnedAppendBase,
    ) -> Result<&CertifiedSource, Self::Error> {
        self.begin_source_append(base.observation().source().clone())
    }

    fn add_prepared(&mut self, prepared: FakePrepared) -> Result<(), Self::Error> {
        if let Some(gate) = &self.add_prepared_gate {
            if !gate.entered.swap(true, Ordering::SeqCst) {
                let admit =
                    gate.release.lock().unwrap().recv().map_err(|_| {
                        FakeLifecycleError::Contract("add_prepared gate disconnected")
                    })?;
                if !admit {
                    return Err(FakeLifecycleError::Contract(
                        "injected gated add_prepared failure",
                    ));
                }
            }
        }
        if !self.add_prepared_delay.is_zero() {
            std::thread::sleep(self.add_prepared_delay);
        }
        self.records.push(prepared.record);
        Ok(())
    }

    fn certify_source(&mut self, certificate: CertifiedSource) -> Result<(), Self::Error> {
        self.certified_sources.push(certificate);
        self.current_source = None;
        Ok(())
    }

    fn certify_source_append(&mut self, append: CertifiedSourceAppend) -> Result<(), Self::Error> {
        self.certified_sources.push(append.into_current());
        self.current_source = None;
        Ok(())
    }

    fn retain_source(&mut self, certificate: CertifiedSource) -> Result<(), Self::Error> {
        self.certified_sources.push(certificate);
        Ok(())
    }

    fn certify_complete_inventory(
        &mut self,
        _inventory: CertifiedSourceInventory,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn delete_source(
        &mut self,
        _deletion: CertifiedSourceDeletion,
        _inventory: CertifiedSourceInventory,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn carry_failed_route(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> Result<bool, Self::Error> {
        Ok(false)
    }

    fn observe_missing_route(
        &mut self,
        _route_identity: SourceRouteIdentity,
        _observed_at_unix_ms: u64,
        _revalidate_missing: impl Fn() -> bool + Send + 'static,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn set_present_routes(
        &mut self,
        _routes: impl IntoIterator<Item = PresentCaptureRoute>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn commit<F, I>(
        self,
        _revalidate: F,
        _revalidate_inventory: I,
    ) -> Result<CaptureCommitReceipt<Self::CommittedSnapshot>, Self::Error>
    where
        F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
    {
        Ok(self.commit_receipt())
    }

    fn commit_with_metadata<F, I, M>(
        self,
        _revalidate: F,
        _revalidate_inventory: I,
        metadata_factory: M,
    ) -> Result<CaptureCommitOutcome<Self::CommittedSnapshot, Self::VerifiedPublication>, Self::Error>
    where
        F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
        M: for<'a> FnOnce(
            CapturePublicationContext<'a, Self::Snapshot<'a>>,
        ) -> Result<Vec<u8>, Self::Error>,
    {
        let snapshot = self.snapshot();
        metadata_factory(CapturePublicationContext::new(
            "fake-generation",
            snapshot.clone(),
        ))?;
        Ok(CaptureCommitOutcome::new(
            self.commit_receipt(),
            CapturePublicationDisposition::Published,
            VerifiedCapture::new(()),
        ))
    }
}

type SourceBackedCoordinatorError = RuntimeSourceBackedCoordinatorError<FakeLifecycleError>;
type SourceBackedCoordinatorResult<T> = Result<T, SourceBackedCoordinatorError>;
type SourceBackedGenerationSink<'writer> =
    RuntimeSourceBackedGenerationSink<'writer, FakeLifecycle>;
type ParallelLeafScanEmitter<'sender, R, E> =
    crate::ParallelLeafScanEmitter<'sender, R, E, FakePreparation>;
type ParallelLeafScanError<E> = crate::ParallelLeafScanError<E, FakeLifecycleError>;
type CoreRecordEmission = crate::CorePreparedCapture<FakePreparation>;
type CoreRecordEmissionBatchBuilder = crate::CorePreparedBatchBuilder<FakePreparation>;
const SOURCE_BACKED_CORE_RECORD_BATCH_MAX_RECORDS: usize = CORE_RECORD_BATCH_MAX_RECORDS;

struct TestDir(PathBuf);

impl TestDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

fn tempdir() -> TestDir {
    TestDir(PathBuf::from("/ctx-runtime-no-io-test"))
}

type TestWorkerResult = Result<(), ParallelLeafScanWorkerError<TestWorkerFailure>>;
type TestRunResult<R> = Result<Vec<R>, ParallelLeafScanError<TestWorkerFailure>>;

fn test_route_identity() -> SourceRouteIdentity {
    SourceRouteIdentity::from_sha256("00".repeat(32)).unwrap()
}

struct SinkHarness {
    writer: FakeLifecycle,
    owners: HashMap<[u8; 32], SourceOwner>,
    complete_inventories: Vec<CompleteInventoryOwner>,
    applied_removals: Vec<crate::SourceBackedCertifiedRemoval>,
    logical_source_failures: SourceBackedLogicalSourceFailures,
    record_rejections: SourceBackedRecordRejections,
    leaf_worker_budget: usize,
}

impl SinkHarness {
    fn open(_index_root: &Path) -> Self {
        Self::with_lifecycle(FakeLifecycle::default())
    }

    fn with_base(base: CertifiedSource) -> Self {
        Self::with_lifecycle(FakeLifecycle::with_base(base))
    }

    fn with_lifecycle(writer: FakeLifecycle) -> Self {
        Self {
            writer,
            owners: HashMap::new(),
            complete_inventories: Vec::new(),
            applied_removals: Vec::new(),
            logical_source_failures: SourceBackedLogicalSourceFailures::default(),
            record_rejections: SourceBackedRecordRejections::default(),
            leaf_worker_budget: 16,
        }
    }

    fn sink(&mut self) -> SourceBackedGenerationSink<'_> {
        RuntimeSourceBackedGenerationSink::new(
            &mut self.writer,
            &mut self.owners,
            &mut self.complete_inventories,
            &mut self.applied_removals,
            0,
            test_route_identity(),
            None,
            SourceBackedRouteResources::production(self.leaf_worker_budget),
            &mut self.logical_source_failures,
            &mut self.record_rejections,
            None,
            None,
            None,
        )
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
        self.sink()
            .run_parallel_leaf_scans(jobs, worker_count, scan)
    }

    fn run_with_existing_worker_states<L, R, W, F>(
        &mut self,
        jobs: Vec<ParallelLeafScanJob<L>>,
        worker_states: &mut [W],
        scan: F,
    ) -> TestRunResult<R>
    where
        L: Send,
        R: Send,
        W: Send,
        F: Fn(
                &mut W,
                &ParallelLeafScanJob<L>,
                &mut ParallelLeafScanEmitter<'_, R, TestWorkerFailure>,
            ) -> TestWorkerResult
            + Sync,
    {
        self.sink()
            .run_parallel_leaf_scans_with_worker_states(jobs, worker_states, scan)
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
        self.sink()
            .run_parallel_leaf_scans_with_source_outcomes(jobs, worker_count, scan)
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
        let mut worker_states = (0..worker_count).map(initialize_worker).collect::<Vec<_>>();
        self.run_with_existing_worker_states(jobs, &mut worker_states, scan)
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
        let core_record_preparer = self.writer.core_preparation();
        let mut sink = RuntimeSourceBackedGenerationSink {
            lifecycle: &mut self.writer,
            core_record_preparer,
            owners: &mut self.owners,
            complete_inventories: &mut self.complete_inventories,
            applied_removals: &mut applied_removals,
            route_index: 0,
            route_identity: test_route_identity(),
            base_route_aliases: BTreeSet::new(),
            base_route_control: None,
            resources,
            logical_source_failures: &mut self.logical_source_failures,
            record_rejections: &mut self.record_rejections,
            record_progress: Some(report_progress),
            current_source_progress: None,
            intermediate_progress_last_emitted_at: None,
            intermediate_progress_pending_stage: None,
            last_progress_session_id: None,
            exact_scan_total_bytes: None,
            exact_scan_accounting_enabled: false,
        };
        sink.run_parallel_leaf_scans(jobs, worker_count, scan)
    }

    fn run_with_resources_and_progress<L, R, F>(
        &mut self,
        jobs: Vec<ParallelLeafScanJob<L>>,
        worker_count: usize,
        resources: SourceBackedRouteResources,
        report_record_progress: &mut dyn FnMut(
            SourceBackedRecordProgressDelta,
        ) -> SourceBackedCoordinatorResult<()>,
        report_current_source_progress: &mut dyn FnMut(
            SourceBackedCurrentSourceProgress,
        ) -> SourceBackedRouteResult<()>,
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
        let core_record_preparer = self.writer.core_preparation();
        let mut sink = RuntimeSourceBackedGenerationSink {
            lifecycle: &mut self.writer,
            core_record_preparer,
            owners: &mut self.owners,
            complete_inventories: &mut self.complete_inventories,
            applied_removals: &mut applied_removals,
            route_index: 0,
            route_identity: test_route_identity(),
            base_route_aliases: BTreeSet::new(),
            base_route_control: None,
            resources,
            logical_source_failures: &mut self.logical_source_failures,
            record_rejections: &mut self.record_rejections,
            record_progress: Some(report_record_progress),
            current_source_progress: Some(report_current_source_progress),
            intermediate_progress_last_emitted_at: None,
            intermediate_progress_pending_stage: None,
            last_progress_session_id: None,
            exact_scan_total_bytes: None,
            exact_scan_accounting_enabled: false,
        };
        sink.run_parallel_leaf_scans(jobs, worker_count, scan)
    }

    fn record_rejections(&mut self, rejections: SourceBackedRecordRejectionDrafts) {
        self.sink().record_rejections(rejections);
    }

    fn commit(self) -> CaptureCommitReceipt<FakeSnapshot> {
        self.writer.commit(|_| true, |_| true).unwrap()
    }
}

#[derive(Default)]
struct NoIoDocumentSpool {
    records: Vec<CoreRecord>,
}

impl DocumentRecordSpool for NoIoDocumentSpool {
    fn new(_resources: SourceBackedRouteResources) -> SourceBackedRouteResult<Self> {
        Ok(Self::default())
    }

    fn push(&mut self, record: CoreRecord) -> SourceBackedRouteResult<()> {
        self.records.push(record);
        Ok(())
    }

    fn replay(
        self,
        mut emit: impl FnMut(CoreRecord) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedRouteResult<()> {
        for record in self.records {
            emit(record)?;
        }
        Ok(())
    }
}

#[derive(Clone)]
struct NoIoDocumentAdapter {
    leaves: Arc<Mutex<Vec<(u8, u8)>>>,
    partial: bool,
}

impl NoIoDocumentAdapter {
    fn new(leaves: &[(u8, u8)], partial: bool) -> Self {
        Self {
            leaves: Arc::new(Mutex::new(leaves.to_vec())),
            partial,
        }
    }

    fn source(id: u8) -> SourceKey {
        test_source(id)
    }

    fn tree(&self) -> CompleteDocumentTree<(u8, u8), ()> {
        let leaves = self
            .leaves
            .lock()
            .unwrap()
            .iter()
            .copied()
            .map(|leaf @ (id, revision)| {
                ObservedDocumentLeaf::new(DocumentLeafFingerprint::new([id ^ revision; 32]), leaf)
            })
            .collect::<Vec<_>>();
        let fingerprint = [leaves.len() as u8; 32];
        if self.partial {
            CompleteDocumentTree::new_partial(fingerprint, leaves, ())
        } else {
            CompleteDocumentTree::new(fingerprint, leaves, ())
        }
    }
}

impl ReplacementDocumentTree for NoIoDocumentAdapter {
    type Lifecycle = FakeLifecycle;
    type Spool = NoIoDocumentSpool;
    type RouteControl = ();
    type Leaf = (u8, u8);
    type TreeAuthority = ();

    fn parser_revision(&self) -> &'static str {
        "no-io-document-v1"
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        source.provider() == "parallel-leaf-test"
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        Ok(self.tree())
    }

    fn scan_changed(
        &self,
        _authority: &Self::TreeAuthority,
        (id, revision): &Self::Leaf,
        sink: &mut crate::ChangedDocumentSink<'_, '_, Self::Lifecycle, Self::Spool>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let source = Self::source(*id);
        sink.begin_source(source.clone())?;
        sink.emit_core_record(test_core_record(&source, 0, *revision))?;
        let observation = SourceObservation::new(
            source.clone(),
            "no-io-document-revision-v1",
            vec![*revision],
        )
        .unwrap();
        Ok(DocumentSourceTerminal {
            source,
            opening: observation.clone(),
            closing: observation,
            parser_revision: self.parser_revision(),
            content_digest: [*revision; 32],
            counts: ScannedSourceCounts {
                complete_records: 1,
                retained_records: 1,
                indexed_documents: 1,
                certified_bytes: 1,
                ..ScannedSourceCounts::default()
            },
        })
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        Ok(if self.tree().leaves.len() == tree.leaves.len() {
            tree.tree_fingerprint
        } else {
            [0xff; 32]
        })
    }
}

#[test]
fn no_io_document_runtime_certifies_complete_and_partial_lifecycles() {
    for partial in [false, true] {
        let adapter = NoIoDocumentAdapter::new(&[(1, 1), (2, 1)], partial);
        let driver = crate::replacement_document_tree_driver(
            DocumentInventoryAuthority::new("parallel-leaf-test".to_owned(), [7; 32]),
            adapter,
        );
        let mut harness = SinkHarness::open(Path::new("/unused"));
        (driver.scan)(&mut harness.sink()).unwrap();
        assert_eq!(harness.writer.certified_sources.len(), 2);
        assert_eq!(harness.writer.records.len(), 2);
        assert_eq!(
            harness.writer.retained_unstaged_routes,
            usize::from(partial)
        );
        assert!((driver.revalidate_at_publication.as_ref().unwrap())());
    }
}

#[test]
fn no_io_document_runtime_rejects_duplicate_fingerprints_before_staging() {
    let adapter = NoIoDocumentAdapter::new(&[(1, 1), (2, 2)], false);
    let driver = crate::replacement_document_tree_driver(
        DocumentInventoryAuthority::new("parallel-leaf-test".to_owned(), [8; 32]),
        adapter,
    );
    let mut harness = SinkHarness::open(Path::new("/unused"));
    let error = (driver.scan)(&mut harness.sink()).unwrap_err();
    assert_eq!(error.kind, SourceBackedRouteErrorKind::SourceChanged);
    assert!(error.detail.contains("duplicate physical leaf"));
    assert!(harness.writer.records.is_empty());
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
    let temp = tempdir();
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
fn borrowed_worker_state_slots_survive_wide_narrow_wide_phases() {
    fn jobs(ids: std::ops::Range<u8>) -> Vec<ParallelLeafScanJob<u8>> {
        ids.map(|id| ParallelLeafScanJob::new(test_source(id), id))
            .collect()
    }

    let temp = tempdir();
    let mut harness = SinkHarness::open(&temp.path().join("index"));
    let mut worker_states = vec![0_usize; 4];
    let scan =
        |worker: &mut usize,
         job: &ParallelLeafScanJob<u8>,
         emitter: &mut ParallelLeafScanEmitter<'_, usize, TestWorkerFailure>| {
            *worker = worker.saturating_add(1);
            emitter.complete(ParallelLeafScanComplete::Skipped { result: *worker })?;
            let _ = job;
            Ok(())
        };

    let first = harness
        .run_with_existing_worker_states(jobs(0..4), &mut worker_states, scan)
        .unwrap();
    let second = harness
        .run_with_existing_worker_states(jobs(4..5), &mut worker_states, scan)
        .unwrap();
    let third = harness
        .run_with_existing_worker_states(jobs(5..9), &mut worker_states, scan)
        .unwrap();

    assert_eq!(first, [1, 1, 1, 1]);
    assert_eq!(second, [2]);
    assert_eq!(third, [3, 2, 2, 2]);
    assert_eq!(worker_states, [3, 2, 2, 2]);
}

#[test]
fn worker_affinity_pins_a_dependency_component_across_phase_widths() {
    let temp = tempdir();
    let mut harness = SinkHarness::open(&temp.path().join("index"));
    let mut worker_states = vec![0_usize; 4];
    let scan =
        |worker: &mut usize,
         _job: &ParallelLeafScanJob<u8>,
         emitter: &mut ParallelLeafScanEmitter<'_, usize, TestWorkerFailure>| {
            *worker = worker.saturating_add(1);
            emitter.complete(ParallelLeafScanComplete::Skipped { result: *worker })?;
            Ok(())
        };
    let root = vec![ParallelLeafScanJob::new(test_source(10), 10).with_worker_affinity(3)];
    let children = (11_u8..15)
        .map(|id| ParallelLeafScanJob::new(test_source(id), id).with_worker_affinity(3))
        .collect::<Vec<_>>();

    assert_eq!(
        harness
            .run_with_existing_worker_states(root, &mut worker_states, scan)
            .unwrap(),
        [1]
    );
    assert_eq!(
        harness
            .run_with_existing_worker_states(children, &mut worker_states, scan)
            .unwrap(),
        [2, 3, 4, 5]
    );
    assert_eq!(worker_states, [0, 0, 0, 5]);
}

#[test]
fn worker_state_is_dropped_for_every_stripe_after_provider_failure() {
    let temp = tempdir();
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
    let temp = tempdir();
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
    let stored_sequences = sources
        .iter()
        .map(|source| commit.snapshot().stored_sequences(source))
        .collect();
    ReplacementSummary {
        results,
        generation_id: commit.generation_id.clone(),
        indexed_documents: commit.indexed_documents,
        certified_sources: commit.certified_sources,
        sources: commit.snapshot().sources().to_vec(),
        stored_sequences,
    }
}

#[test]
fn a_barrier_proves_all_forced_workers_scan_concurrently() {
    let temp = tempdir();
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
    let temp = tempdir();
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

mod additional;
mod progress_visibility;
use additional::{test_certificate, test_core_record, test_source};
