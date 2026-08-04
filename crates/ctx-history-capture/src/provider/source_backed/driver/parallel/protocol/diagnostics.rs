use std::error::Error as StdError;

use ctx_history_core::{CertifiedSource, SourceKey};

use super::{
    CoreRecordEmission, CoreRecordEmissionBatch, ParallelLeafProtocolMessage,
    ParallelLeafScanBegin, ParallelLeafScanComplete, ParallelLeafScanError,
    ParallelLeafScanMessageKind, ParallelLeafScanMode, ParallelLeafScanProtocolError,
    ParallelLeafSinkOperation, SourceBackedCoordinatorError, SourceBackedGenerationSink,
    SourceBackedSourceOutcome,
};

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
pub(in super::super) struct ParallelLeafJobState {
    source: Option<SourceKey>,
    pub(in super::super) worker_index: usize,
    begin: Option<AcceptedBegin>,
    pub(in super::super) completion: Option<ParallelLeafScanMode>,
    pub(in super::super) returned: bool,
}

impl ParallelLeafJobState {
    pub(in super::super) fn new(source: Option<SourceKey>, worker_index: usize) -> Self {
        Self {
            source,
            worker_index,
            begin: None,
            completion: None,
            returned: false,
        }
    }
}

pub(in super::super) fn state_mut<E>(
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

pub(in super::super) fn validate_worker<E>(
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

pub(in super::super) fn apply_parallel_leaf_message<R, E>(
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
        ParallelLeafProtocolMessage::CoreRecordBatch(batch) => {
            apply_core_record_batch(sink, job_index, state, *batch)
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

fn apply_core_record_batch<E>(
    sink: &mut SourceBackedGenerationSink<'_>,
    job_index: usize,
    state: &mut ParallelLeafJobState,
    batch: CoreRecordEmissionBatch,
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
    let source = bound_source(
        job_index,
        ParallelLeafScanMessageKind::CoreRecordBatch,
        state,
    )?;
    for emission in batch.iter() {
        validate_source(
            job_index,
            ParallelLeafScanMessageKind::CoreRecordBatch,
            source,
            emission.source(),
        )?;
    }
    sink.add_core_record_emission_batch(batch).map_err(|error| {
        sink_error(
            job_index,
            source,
            ParallelLeafSinkOperation::AddCoreRecordBatch,
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
            if let Some(base) = failure.retained.as_ref() {
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
            }
            state.completion = Some(ParallelLeafScanMode::SourceFailure);
            Ok(SourceBackedSourceOutcome::Failed(failure))
        }
    }
}

/// Applies receipt-only failure diagnostics after all ready-driven worker
/// events have been accepted. Results are indexed by input job, so this keeps
/// the bounded diagnostic prefix in canonical scan order without serializing
/// record ingestion behind a slow earlier worker.
pub(in super::super) fn finalize_parallel_leaf_diagnostics<R, E>(
    sink: &mut SourceBackedGenerationSink<'_>,
    results: &[Option<SourceBackedSourceOutcome<R>>],
) -> Result<(), ParallelLeafScanError<E>>
where
    E: StdError + 'static,
{
    for (job_index, result) in results.iter().enumerate() {
        let Some(SourceBackedSourceOutcome::Failed(failure)) = result.as_ref() else {
            continue;
        };
        let source = failure.source.clone();
        sink.record_logical_source_failure(
            source.clone(),
            failure.failure.clone(),
            failure.retained.is_some(),
        )
        .map_err(|error| {
            sink_error(
                job_index,
                &source,
                ParallelLeafSinkOperation::RecordSourceFailure,
                error,
            )
        })?;
    }
    Ok(())
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
