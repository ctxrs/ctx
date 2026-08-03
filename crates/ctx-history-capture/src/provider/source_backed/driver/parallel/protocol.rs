use std::{
    error::Error as StdError,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
    },
};

use ctx_history_core::{CertifiedSource, CertifiedSourceAppend, CoreRecord, SourceKey};
use ctx_history_index::CoreRecordPreparer;
use thiserror::Error;

use super::super::{
    CoreRecordEmission, SourceBackedCoordinatorError, SourceBackedGenerationSink,
    SourceBackedLogicalSourceFailureFact, SourceBackedRecordRejectionDrafts,
    SourceBackedRouteError, SourceBackedRouteResources, SourceBackedSourceOutcome,
};

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
    Begin(Box<ParallelLeafScanBegin>),
    CoreRecord(Box<CoreRecordEmission>),
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
        self.send(ParallelLeafProtocolMessage::Begin(Box::new(begin)))
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

#[derive(Debug)]
enum AcceptedBegin {
    Replace,
    Append { base: Box<CertifiedSource> },
}

impl AcceptedBegin {
    fn mode(&self) -> ParallelLeafScanMode {
        match self {
            Self::Replace => ParallelLeafScanMode::Replace,
            Self::Append { .. } => ParallelLeafScanMode::Append,
        }
    }
}

#[derive(Debug)]
pub(super) struct ParallelLeafJobState {
    source: Option<SourceKey>,
    pub(super) worker_index: usize,
    begin: Option<AcceptedBegin>,
    pub(super) completion: Option<ParallelLeafScanMode>,
    pub(super) returned: bool,
}

impl ParallelLeafJobState {
    pub(super) fn new(source: Option<SourceKey>, worker_index: usize) -> Self {
        Self {
            source,
            worker_index,
            begin: None,
            completion: None,
            returned: false,
        }
    }
}

pub(super) fn state_mut<E>(
    states: &mut [ParallelLeafJobState],
    job_index: usize,
) -> Result<&mut ParallelLeafJobState, ParallelLeafScanError<E>>
where
    E: StdError + 'static,
{
    states.get_mut(job_index).ok_or_else(|| {
        ParallelLeafScanError::Protocol(ParallelLeafScanProtocolError::UnknownJob { job_index })
    })
}

pub(super) fn validate_worker<E>(
    states: &[ParallelLeafJobState],
    job_index: usize,
    worker_index: usize,
) -> Result<(), ParallelLeafScanError<E>>
where
    E: StdError + 'static,
{
    let state = states.get(job_index).ok_or_else(|| {
        ParallelLeafScanError::Protocol(ParallelLeafScanProtocolError::UnknownJob { job_index })
    })?;
    if state.worker_index != worker_index {
        return Err(ParallelLeafScanProtocolError::WrongWorker {
            job_index,
            expected_worker: state.worker_index,
            observed_worker: worker_index,
        }
        .into());
    }
    Ok(())
}

pub(super) fn apply_parallel_leaf_message<R, E>(
    sink: &mut SourceBackedGenerationSink<'_>,
    job_index: usize,
    message: ParallelLeafProtocolMessage<R>,
    states: &mut [ParallelLeafJobState],
    results: &mut [Option<SourceBackedSourceOutcome<R>>],
) -> Result<(), ParallelLeafScanError<E>>
where
    E: StdError + 'static,
{
    let state = state_mut(states, job_index)?;
    match message {
        ParallelLeafProtocolMessage::Begin(begin) => apply_begin(sink, job_index, state, *begin),
        ParallelLeafProtocolMessage::CoreRecord(record) => {
            apply_core_record(sink, job_index, state, *record)
        }
        ParallelLeafProtocolMessage::Complete(completion) => {
            let result = apply_completion(sink, job_index, state, *completion)?;
            let slot = results.get_mut(job_index).ok_or({
                ParallelLeafScanProtocolError::TransportDisconnected {
                    unfinished_jobs: states.len(),
                }
            })?;
            *slot = Some(result);
            Ok(())
        }
    }
}

fn apply_begin<E>(
    sink: &mut SourceBackedGenerationSink<'_>,
    job_index: usize,
    state: &mut ParallelLeafJobState,
    begin: ParallelLeafScanBegin,
) -> Result<(), ParallelLeafScanError<E>>
where
    E: StdError + 'static,
{
    if state.begin.is_some() {
        return Err(ParallelLeafScanProtocolError::DuplicateBegin { job_index }.into());
    }
    if state.completion.is_some() {
        return Err(ParallelLeafScanProtocolError::BeginAfterCompletion { job_index }.into());
    }
    match begin {
        ParallelLeafScanBegin::Replace { source } => {
            bind_source(
                job_index,
                ParallelLeafScanMessageKind::BeginReplace,
                state,
                &source,
            )?;
            let exact_source = source.clone();
            sink.begin_source(source).map_err(|error| {
                sink_error(
                    job_index,
                    &exact_source,
                    ParallelLeafSinkOperation::BeginReplace,
                    error,
                )
            })?;
            state.begin = Some(AcceptedBegin::Replace);
        }
        ParallelLeafScanBegin::Append { source, base } => {
            bind_source(
                job_index,
                ParallelLeafScanMessageKind::BeginAppend,
                state,
                &source,
            )?;
            validate_source(
                job_index,
                ParallelLeafScanMessageKind::BeginAppend,
                bound_source(job_index, ParallelLeafScanMessageKind::BeginAppend, state)?,
                base.observation().source(),
            )?;
            let exact_source = source.clone();
            let writer_base = sink.begin_source_append(source).map_err(|error| {
                sink_error(
                    job_index,
                    &exact_source,
                    ParallelLeafSinkOperation::BeginAppend,
                    error,
                )
            })?;
            if writer_base != base.as_ref() {
                return Err(ParallelLeafScanProtocolError::AppendBaseMismatch { job_index }.into());
            }
            state.begin = Some(AcceptedBegin::Append { base });
        }
    }
    Ok(())
}

fn apply_core_record<E>(
    sink: &mut SourceBackedGenerationSink<'_>,
    job_index: usize,
    state: &mut ParallelLeafJobState,
    emission: CoreRecordEmission,
) -> Result<(), ParallelLeafScanError<E>>
where
    E: StdError + 'static,
{
    if state.completion.is_some() {
        return Err(ParallelLeafScanProtocolError::CoreRecordAfterCompletion { job_index }.into());
    }
    if state.begin.is_none() {
        return Err(ParallelLeafScanProtocolError::CoreRecordBeforeBegin { job_index }.into());
    }
    let source = bound_source(job_index, ParallelLeafScanMessageKind::CoreRecord, state)?;
    validate_source(
        job_index,
        ParallelLeafScanMessageKind::CoreRecord,
        source,
        emission.source(),
    )?;
    sink.add_core_record_emission(emission).map_err(|error| {
        sink_error(
            job_index,
            source,
            ParallelLeafSinkOperation::AddCoreRecord,
            error,
        )
    })
}

fn apply_completion<R, E>(
    sink: &mut SourceBackedGenerationSink<'_>,
    job_index: usize,
    state: &mut ParallelLeafJobState,
    completion: ParallelLeafScanComplete<R>,
) -> Result<SourceBackedSourceOutcome<R>, ParallelLeafScanError<E>>
where
    E: StdError + 'static,
{
    if state.completion.is_some() {
        return Err(ParallelLeafScanProtocolError::DuplicateCompletion { job_index }.into());
    }
    match completion {
        ParallelLeafScanComplete::Replace {
            certificate,
            result,
        } => {
            require_begin_mode(job_index, state, ParallelLeafScanMode::Replace)?;
            let source = bound_source(
                job_index,
                ParallelLeafScanMessageKind::CompleteReplace,
                state,
            )?;
            validate_source(
                job_index,
                ParallelLeafScanMessageKind::CompleteReplace,
                source,
                certificate.observation().source(),
            )?;
            sink.certify_source(*certificate).map_err(|error| {
                sink_error(
                    job_index,
                    source,
                    ParallelLeafSinkOperation::CompleteReplace,
                    error,
                )
            })?;
            state.completion = Some(ParallelLeafScanMode::Replace);
            Ok(SourceBackedSourceOutcome::Success(result))
        }
        ParallelLeafScanComplete::Append { append, result } => {
            require_begin_mode(job_index, state, ParallelLeafScanMode::Append)?;
            let source = bound_source(
                job_index,
                ParallelLeafScanMessageKind::CompleteAppend,
                state,
            )?;
            validate_source(
                job_index,
                ParallelLeafScanMessageKind::CompleteAppend,
                source,
                append.current().observation().source(),
            )?;
            validate_source(
                job_index,
                ParallelLeafScanMessageKind::CompleteAppend,
                source,
                append.base().observation().source(),
            )?;
            let Some(AcceptedBegin::Append { base }) = state.begin.as_ref() else {
                return Err(ParallelLeafScanProtocolError::CompletionModeMismatch {
                    job_index,
                    begin: state
                        .begin
                        .as_ref()
                        .map_or(ParallelLeafScanMode::Skipped, AcceptedBegin::mode),
                    completion: ParallelLeafScanMode::Append,
                }
                .into());
            };
            if append.base() != base.as_ref() {
                return Err(
                    ParallelLeafScanProtocolError::AppendCompletionBaseMismatch { job_index }
                        .into(),
                );
            }
            sink.certify_source_append(*append).map_err(|error| {
                sink_error(
                    job_index,
                    source,
                    ParallelLeafSinkOperation::CompleteAppend,
                    error,
                )
            })?;
            state.completion = Some(ParallelLeafScanMode::Append);
            Ok(SourceBackedSourceOutcome::Success(result))
        }
        ParallelLeafScanComplete::Retain {
            certificate,
            result,
        } => {
            if state.begin.is_some() {
                return Err(ParallelLeafScanProtocolError::SkippedAfterBegin { job_index }.into());
            }
            let source = bound_source(
                job_index,
                ParallelLeafScanMessageKind::CompleteRetain,
                state,
            )?;
            validate_source(
                job_index,
                ParallelLeafScanMessageKind::CompleteRetain,
                source,
                certificate.observation().source(),
            )?;
            sink.retain_source(*certificate).map_err(|error| {
                sink_error(
                    job_index,
                    source,
                    ParallelLeafSinkOperation::RetainSource,
                    error,
                )
            })?;
            state.completion = Some(ParallelLeafScanMode::Retain);
            Ok(SourceBackedSourceOutcome::Success(result))
        }
        ParallelLeafScanComplete::Skipped { result } => {
            if state.begin.is_some() {
                return Err(ParallelLeafScanProtocolError::SkippedAfterBegin { job_index }.into());
            }
            state.completion = Some(ParallelLeafScanMode::Skipped);
            Ok(SourceBackedSourceOutcome::Success(result))
        }
        ParallelLeafScanComplete::SourceFailure { failure } => {
            let mut failure = failure;
            if state.begin.is_some() {
                return Err(ParallelLeafScanProtocolError::SkippedAfterBegin { job_index }.into());
            }
            if !failure.failure.kind.is_logical_source_failure() {
                return Err(
                    ParallelLeafScanProtocolError::InvalidSourceFailureKind { job_index }.into(),
                );
            }
            let bound = bound_source(
                job_index,
                ParallelLeafScanMessageKind::CompleteSourceFailure,
                state,
            )?;
            validate_source(
                job_index,
                ParallelLeafScanMessageKind::CompleteSourceFailure,
                bound,
                &failure.source,
            )?;
            let carried_forward = if let Some(base) = failure.retained.as_ref() {
                if base.observation().source() != bound {
                    return Err(ParallelLeafScanProtocolError::SourceFailureBaseMismatch {
                        job_index,
                    }
                    .into());
                }
                sink.retain_source(base.clone()).map_err(|error| {
                    sink_error(
                        job_index,
                        bound,
                        ParallelLeafSinkOperation::RetainSource,
                        error,
                    )
                })?;
                true
            } else {
                false
            };
            sink.record_logical_source_failure(
                failure.source.clone(),
                failure.failure.clone(),
                carried_forward,
            )
            .map_err(|error| {
                sink_error(
                    job_index,
                    bound,
                    ParallelLeafSinkOperation::RecordSourceFailure,
                    error,
                )
            })?;
            sink.record_rejections(std::mem::take(&mut failure.record_rejections));
            state.completion = Some(ParallelLeafScanMode::SourceFailure);
            Ok(SourceBackedSourceOutcome::Failed(failure))
        }
    }
}

fn require_begin_mode<E>(
    job_index: usize,
    state: &ParallelLeafJobState,
    completion: ParallelLeafScanMode,
) -> Result<(), ParallelLeafScanError<E>>
where
    E: StdError + 'static,
{
    let begin = state.begin.as_ref().ok_or_else(|| {
        ParallelLeafScanError::Protocol(ParallelLeafScanProtocolError::MissingBegin {
            job_index,
            completion,
        })
    })?;
    if begin.mode() != completion {
        return Err(ParallelLeafScanProtocolError::CompletionModeMismatch {
            job_index,
            begin: begin.mode(),
            completion,
        }
        .into());
    }
    Ok(())
}

fn bind_source<E>(
    job_index: usize,
    message: ParallelLeafScanMessageKind,
    state: &mut ParallelLeafJobState,
    observed: &SourceKey,
) -> Result<(), ParallelLeafScanError<E>>
where
    E: StdError + 'static,
{
    if let Some(expected) = state.source.as_ref() {
        validate_source(job_index, message, expected, observed)
    } else {
        state.source = Some(observed.clone());
        Ok(())
    }
}

fn bound_source<E>(
    job_index: usize,
    message: ParallelLeafScanMessageKind,
    state: &ParallelLeafJobState,
) -> Result<&SourceKey, ParallelLeafScanError<E>>
where
    E: StdError + 'static,
{
    state
        .source
        .as_ref()
        .ok_or_else(|| ParallelLeafScanProtocolError::SourceNotBound { job_index, message }.into())
}

fn validate_source<E>(
    job_index: usize,
    message: ParallelLeafScanMessageKind,
    expected: &SourceKey,
    observed: &SourceKey,
) -> Result<(), ParallelLeafScanError<E>>
where
    E: StdError + 'static,
{
    if !expected.exact_descriptor_eq(observed) {
        return Err(ParallelLeafScanProtocolError::SourceMismatch {
            job_index,
            message,
            expected: Box::new(expected.clone()),
            observed: Box::new(observed.clone()),
        }
        .into());
    }
    Ok(())
}

fn sink_error<E>(
    job_index: usize,
    source: &SourceKey,
    operation: ParallelLeafSinkOperation,
    error: SourceBackedCoordinatorError,
) -> ParallelLeafScanError<E>
where
    E: StdError + 'static,
{
    ParallelLeafScanError::Sink {
        job_index,
        source_id: source.identity().to_string(),
        operation,
        source: error,
    }
}
