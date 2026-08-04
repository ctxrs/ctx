use std::{
    error::Error as StdError,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::Duration,
};

use ctx_history_core::{CertifiedSource, CertifiedSourceAppend, CoreRecord, SourceKey};
use ctx_history_index::CoreRecordPreparer;
use thiserror::Error;

use super::super::{
    CoreRecordEmission, CoreRecordEmissionBatch, SourceBackedCoordinatorError,
    SourceBackedGenerationSink, SourceBackedLogicalSourceFailureFact,
    SourceBackedRecordRejectionDrafts, SourceBackedRouteError, SourceBackedRouteErrorKind,
    SourceBackedRouteResourceKind, SourceBackedRouteResources, SourceBackedSourceOutcome,
    SOURCE_BACKED_CORE_RECORD_BATCH_MAX_RECORDS,
};

const CORE_OUTPUT_RESERVATION_RETRY_DELAY: Duration = Duration::from_millis(1);

#[derive(Debug)]
pub struct ParallelLeafScanJob<L> {
    source: SourceKey,
    leaf: L,
}

impl<L> ParallelLeafScanJob<L> {
    pub fn new(source: SourceKey, leaf: L) -> Self {
        Self { source, leaf }
    }

    pub fn source(&self) -> &SourceKey {
        &self.source
    }

    pub fn leaf(&self) -> &L {
        &self.leaf
    }
}

#[derive(Debug, Clone)]
pub enum ParallelLeafScanBegin {
    Replace {
        source: SourceKey,
    },
    Append {
        source: SourceKey,
        base: Box<CertifiedSource>,
    },
}

impl ParallelLeafScanBegin {
    pub fn replace(source: SourceKey) -> Self {
        Self::Replace { source }
    }

    pub fn append(source: SourceKey, base: CertifiedSource) -> Self {
        Self::Append {
            source,
            base: Box::new(base),
        }
    }
}

#[derive(Debug)]
pub enum ParallelLeafScanComplete<R> {
    Replace {
        certificate: Box<CertifiedSource>,
        result: R,
    },
    Append {
        append: Box<CertifiedSourceAppend>,
        result: R,
    },
    Retain {
        certificate: Box<CertifiedSource>,
        result: R,
    },
    Skipped {
        result: R,
    },
    SourceFailure {
        failure: Box<SourceBackedLogicalSourceFailureFact>,
    },
}

impl<R> ParallelLeafScanComplete<R> {
    pub fn replace(certificate: CertifiedSource, result: R) -> Self {
        Self::Replace {
            certificate: Box::new(certificate),
            result,
        }
    }

    pub fn append(append: CertifiedSourceAppend, result: R) -> Self {
        Self::Append {
            append: Box::new(append),
            result,
        }
    }

    pub fn retain(certificate: CertifiedSource, result: R) -> Self {
        Self::Retain {
            certificate: Box::new(certificate),
            result,
        }
    }

    pub fn skipped(result: R) -> Self {
        Self::Skipped { result }
    }

    pub fn source_failure(
        source: SourceKey,
        base: Option<CertifiedSource>,
        failure: SourceBackedRouteError,
    ) -> Self {
        Self::source_failure_with_rejections(source, base, failure, Default::default())
    }

    pub(crate) fn source_failure_with_rejections(
        source: SourceKey,
        base: Option<CertifiedSource>,
        failure: SourceBackedRouteError,
        record_rejections: SourceBackedRecordRejectionDrafts,
    ) -> Self {
        Self::SourceFailure {
            failure: Box::new(SourceBackedLogicalSourceFailureFact {
                source,
                retained: base,
                failure,
                record_rejections,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParallelLeafScanMessageKind {
    BeginReplace,
    BeginAppend,
    CoreRecord,
    CoreRecordBatch,
    CompleteReplace,
    CompleteAppend,
    CompleteRetain,
    CompleteSourceFailure,
}

impl std::fmt::Display for ParallelLeafScanMessageKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BeginReplace => formatter.write_str("replacement begin"),
            Self::BeginAppend => formatter.write_str("append begin"),
            Self::CoreRecord => formatter.write_str("Core record"),
            Self::CoreRecordBatch => formatter.write_str("Core record batch"),
            Self::CompleteReplace => formatter.write_str("replacement completion"),
            Self::CompleteAppend => formatter.write_str("append completion"),
            Self::CompleteRetain => formatter.write_str("retained completion"),
            Self::CompleteSourceFailure => formatter.write_str("source-failure completion"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParallelLeafScanMode {
    Replace,
    Append,
    Retain,
    Skipped,
    SourceFailure,
}

impl std::fmt::Display for ParallelLeafScanMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Replace => formatter.write_str("replacement"),
            Self::Append => formatter.write_str("append"),
            Self::Retain => formatter.write_str("retained"),
            Self::Skipped => formatter.write_str("skipped"),
            Self::SourceFailure => formatter.write_str("source failure"),
        }
    }
}

#[derive(Debug, Error)]
pub enum ParallelLeafScanProtocolError {
    #[error("parallel leaf job {job_index} began more than once")]
    DuplicateBegin { job_index: usize },
    #[error("parallel leaf job {job_index} began after completion")]
    BeginAfterCompletion { job_index: usize },
    #[error("parallel leaf job {job_index} emitted a Core record before beginning")]
    CoreRecordBeforeBegin { job_index: usize },
    #[error("parallel leaf job {job_index} emitted a Core record after completion")]
    CoreRecordAfterCompletion { job_index: usize },
    #[error("parallel leaf job {job_index} completed more than once")]
    DuplicateCompletion { job_index: usize },
    #[error("parallel leaf job {job_index} completed as {completion} without beginning")]
    MissingBegin {
        job_index: usize,
        completion: ParallelLeafScanMode,
    },
    #[error("parallel leaf job {job_index} began as {begin} but completed as {completion}")]
    CompletionModeMismatch {
        job_index: usize,
        begin: ParallelLeafScanMode,
        completion: ParallelLeafScanMode,
    },
    #[error("parallel leaf job {job_index} was skipped after beginning")]
    SkippedAfterBegin { job_index: usize },
    #[error("parallel leaf job {job_index} reported a non-local failure as a source outcome")]
    InvalidSourceFailureKind { job_index: usize },
    #[error("parallel leaf job {job_index} retained a mismatched base after source failure")]
    SourceFailureBaseMismatch { job_index: usize },
    #[error("parallel leaf job {job_index} returned without one completion")]
    MissingCompletion { job_index: usize },
    #[error("parallel leaf job {job_index} returned more than once")]
    DuplicateReturn { job_index: usize },
    #[error(
        "parallel leaf job {job_index} emitted {message} for the wrong exact source: \
         expected {expected:?}, observed {observed:?}"
    )]
    SourceMismatch {
        job_index: usize,
        message: ParallelLeafScanMessageKind,
        expected: Box<SourceKey>,
        observed: Box<SourceKey>,
    },
    #[error("parallel leaf job {job_index} emitted {message} before binding an exact source")]
    SourceNotBound {
        job_index: usize,
        message: ParallelLeafScanMessageKind,
    },
    #[error("parallel leaf job {job_index} append begin did not match the writer base")]
    AppendBaseMismatch { job_index: usize },
    #[error("parallel leaf job {job_index} append completion changed its declared base")]
    AppendCompletionBaseMismatch { job_index: usize },
    #[error("parallel leaf transport disconnected with {unfinished_jobs} unfinished jobs")]
    TransportDisconnected { unfinished_jobs: usize },
    #[error("parallel leaf transport referenced unknown job {job_index}")]
    UnknownJob { job_index: usize },
    #[error(
        "parallel leaf job {job_index} disconnected before accepting its Begin acknowledgement"
    )]
    BeginAcknowledgementDisconnected { job_index: usize },
    #[error("parallel leaf API received a logical-source failure without requesting outcomes")]
    UnexpectedSourceFailure,
    #[error(
        "parallel leaf job {job_index} was assigned to worker {expected_worker} but emitted from \
         worker {observed_worker}"
    )]
    WrongWorker {
        job_index: usize,
        expected_worker: usize,
        observed_worker: usize,
    },
}

#[derive(Debug)]
pub(super) struct ParallelLeafBeginAcknowledgement(SyncSender<()>);

impl ParallelLeafBeginAcknowledgement {
    fn rendezvous() -> (Self, Receiver<()>) {
        let (sender, receiver) = mpsc::sync_channel(0);
        (Self(sender), receiver)
    }

    pub(super) fn acknowledge(self, job_index: usize) -> Result<(), ParallelLeafScanProtocolError> {
        self.0.send(()).map_err(|_| {
            ParallelLeafScanProtocolError::BeginAcknowledgementDisconnected { job_index }
        })
    }
}

#[derive(Debug, Error)]
#[error("parallel leaf scan was cancelled")]
pub struct ParallelLeafScanCancelled;

#[derive(Debug, Error)]
pub enum ParallelLeafScanWorkerError<E>
where
    E: StdError + 'static,
{
    #[error("provider leaf scan failed: {0}")]
    Provider(#[source] E),
    #[error(transparent)]
    Cancelled(#[from] ParallelLeafScanCancelled),
}

impl<E> ParallelLeafScanWorkerError<E>
where
    E: StdError + 'static,
{
    pub fn provider(error: E) -> Self {
        Self::Provider(error)
    }
}

impl From<ParallelLeafScanEmitError> for ParallelLeafScanWorkerError<SourceBackedRouteError> {
    fn from(error: ParallelLeafScanEmitError) -> Self {
        match error {
            ParallelLeafScanEmitError::Cancelled(cancelled) => Self::Cancelled(cancelled),
            ParallelLeafScanEmitError::Route(error) => Self::Provider(error),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParallelLeafSinkOperation {
    BeginReplace,
    BeginAppend,
    AddCoreRecord,
    AddCoreRecordBatch,
    CompleteReplace,
    CompleteAppend,
    RetainSource,
    RecordSourceFailure,
}

impl std::fmt::Display for ParallelLeafSinkOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BeginReplace => formatter.write_str("begin replacement"),
            Self::BeginAppend => formatter.write_str("begin append"),
            Self::AddCoreRecord => formatter.write_str("add Core record"),
            Self::AddCoreRecordBatch => formatter.write_str("add Core record batch"),
            Self::CompleteReplace => formatter.write_str("complete replacement"),
            Self::CompleteAppend => formatter.write_str("complete append"),
            Self::RetainSource => formatter.write_str("retain source"),
            Self::RecordSourceFailure => formatter.write_str("record source failure"),
        }
    }
}

#[derive(Debug, Error)]
pub enum ParallelLeafScanError<E>
where
    E: StdError + 'static,
{
    #[error("parallel leaf scan requested zero workers for {job_count} jobs")]
    InvalidWorkerCount { job_count: usize },
    #[error("failed to spawn parallel leaf worker {worker_index}: {source}")]
    WorkerSpawn {
        worker_index: usize,
        #[source]
        source: std::io::Error,
    },
    #[error("parallel leaf worker {worker_index} failed on job {job_index}: {source}")]
    Worker {
        worker_index: usize,
        job_index: usize,
        #[source]
        source: E,
    },
    #[error("parallel leaf worker {worker_index} panicked on job {job_index}")]
    WorkerPanicked {
        worker_index: usize,
        job_index: usize,
    },
    #[error(
        "parallel leaf worker {worker_index} cancelled job {job_index} without a peer failure"
    )]
    WorkerCancelled {
        worker_index: usize,
        job_index: usize,
    },
    #[error("parallel leaf worker {worker_index} panicked outside its guarded scan")]
    WorkerJoinPanicked { worker_index: usize },
    #[error(transparent)]
    Protocol(#[from] ParallelLeafScanProtocolError),
    #[error(
        "parallel leaf job {job_index} failed to {operation} for source {source_id}: {source}"
    )]
    Sink {
        job_index: usize,
        source_id: String,
        operation: ParallelLeafSinkOperation,
        #[source]
        source: SourceBackedCoordinatorError,
    },
}

#[derive(Debug)]
pub(super) enum ParallelLeafProtocolMessage<R> {
    Begin {
        begin: Box<ParallelLeafScanBegin>,
        acknowledgement: ParallelLeafBeginAcknowledgement,
    },
    CoreRecord(Box<CoreRecordEmission>),
    CoreRecordBatch(Box<CoreRecordEmissionBatch>),
    Complete(Box<ParallelLeafScanComplete<R>>),
}

#[derive(Debug)]
pub(super) enum ParallelLeafWorkerEvent<R, E> {
    Protocol {
        worker_index: usize,
        job_index: usize,
        message: Box<ParallelLeafProtocolMessage<R>>,
    },
    Returned {
        worker_index: usize,
        job_index: usize,
    },
    Failed {
        worker_index: usize,
        job_index: usize,
        error: E,
    },
    Panicked {
        worker_index: usize,
        job_index: usize,
    },
    Cancelled {
        worker_index: usize,
        job_index: usize,
    },
}

pub struct ParallelLeafScanEmitter<'sender, R, E> {
    pub(super) worker_index: usize,
    pub(super) job_index: usize,
    pub(super) sender: &'sender SyncSender<ParallelLeafWorkerEvent<R, E>>,
    pub(super) cancellation: &'sender AtomicBool,
    pub(super) resources: SourceBackedRouteResources,
    pub(super) core_record_preparer: CoreRecordPreparer,
}

#[derive(Debug, Error)]
pub enum ParallelLeafScanEmitError {
    #[error(transparent)]
    Cancelled(#[from] ParallelLeafScanCancelled),
    #[error(transparent)]
    Route(#[from] SourceBackedRouteError),
}

impl<R, E> ParallelLeafScanEmitter<'_, R, E> {
    pub fn begin(&mut self, begin: ParallelLeafScanBegin) -> Result<(), ParallelLeafScanCancelled> {
        let (acknowledgement, applied) = ParallelLeafBeginAcknowledgement::rendezvous();
        self.send(ParallelLeafProtocolMessage::Begin {
            begin: Box::new(begin),
            acknowledgement,
        })?;
        applied.recv().map_err(|_| ParallelLeafScanCancelled)?;
        self.require_not_cancelled()
    }

    pub fn emit_core_record(
        &mut self,
        record: CoreRecord,
    ) -> Result<(), ParallelLeafScanEmitError> {
        let emission =
            CoreRecordEmission::new(record, &self.resources, &self.core_record_preparer)?;
        self.emit_core_record_emission(emission)
    }

    pub(crate) fn emit_core_record_emission(
        &mut self,
        emission: CoreRecordEmission,
    ) -> Result<(), ParallelLeafScanEmitError> {
        self.send(ParallelLeafProtocolMessage::CoreRecord(Box::new(emission)))?;
        Ok(())
    }

    /// Prepares and reserves one bounded provider page on this worker, then
    /// transports each max-64 Core-record chunk through one rendezvous with
    /// the coordinator. Ordinary JSONL pages therefore use one message, while
    /// projectors that fan one physical page out further remain bounded. The
    /// shared live-byte budget is backpressure rather than a batch admission
    /// ceiling: a worker flushes its current batch before waiting cancelably
    /// for an individually admissible next record.
    pub fn emit_core_records(
        &mut self,
        records: Vec<CoreRecord>,
    ) -> Result<(), ParallelLeafScanEmitError> {
        let mut emissions = Vec::with_capacity(
            records
                .len()
                .min(SOURCE_BACKED_CORE_RECORD_BATCH_MAX_RECORDS),
        );
        for record in records {
            self.require_not_cancelled()?;
            let prepared = CoreRecordEmission::prepare(record, &self.core_record_preparer)?;
            let prepared_bytes = prepared.encoded_core_bytes();
            let maximum_bytes = self
                .resources
                .maximum_bytes(SourceBackedRouteResourceKind::CoreOutput);

            let reservation = loop {
                self.require_not_cancelled()?;
                match self
                    .resources
                    .reserve(SourceBackedRouteResourceKind::CoreOutput, prepared_bytes)
                {
                    Ok(reservation) => break reservation,
                    Err(error)
                        if error.kind == SourceBackedRouteErrorKind::ResourceUnavailable
                            && u64::try_from(prepared_bytes)
                                .ok()
                                .is_some_and(|bytes| bytes <= maximum_bytes) =>
                    {
                        if !emissions.is_empty() {
                            self.emit_core_record_batch(&mut emissions)?;
                        } else {
                            thread::sleep(CORE_OUTPUT_RESERVATION_RETRY_DELAY);
                        }
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            emissions.push(CoreRecordEmission::from_prepared_and_reservation(
                prepared,
                reservation,
            ));
            if emissions.len() == SOURCE_BACKED_CORE_RECORD_BATCH_MAX_RECORDS {
                self.emit_core_record_batch(&mut emissions)?;
            }
        }
        self.emit_core_record_batch(&mut emissions)?;
        Ok(())
    }

    fn emit_core_record_batch(
        &mut self,
        emissions: &mut Vec<CoreRecordEmission>,
    ) -> Result<(), ParallelLeafScanEmitError> {
        if emissions.is_empty() {
            return Ok(());
        }
        let batch = CoreRecordEmissionBatch::from_emissions(std::mem::take(emissions))?;
        self.send(ParallelLeafProtocolMessage::CoreRecordBatch(Box::new(
            batch,
        )))?;
        Ok(())
    }

    fn require_not_cancelled(&self) -> Result<(), ParallelLeafScanCancelled> {
        if self.is_cancelled() {
            return Err(ParallelLeafScanCancelled);
        }
        Ok(())
    }

    pub fn complete(
        &mut self,
        completion: ParallelLeafScanComplete<R>,
    ) -> Result<(), ParallelLeafScanCancelled> {
        self.send(ParallelLeafProtocolMessage::Complete(Box::new(completion)))
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.load(Ordering::Acquire)
    }

    pub(crate) fn route_resources(&self) -> SourceBackedRouteResources {
        self.resources.clone()
    }

    fn send(
        &self,
        message: ParallelLeafProtocolMessage<R>,
    ) -> Result<(), ParallelLeafScanCancelled> {
        if self.is_cancelled() {
            return Err(ParallelLeafScanCancelled);
        }
        self.sender
            .send(ParallelLeafWorkerEvent::Protocol {
                worker_index: self.worker_index,
                job_index: self.job_index,
                message: Box::new(message),
            })
            .map_err(|_| ParallelLeafScanCancelled)
    }
}

mod diagnostics;

pub(super) use diagnostics::{
    apply_parallel_leaf_message, finalize_parallel_leaf_diagnostics, state_mut, validate_worker,
    ParallelLeafJobState,
};
