use super::*;
use crate::CaptureLifecycleSink;
use ctx_history_core::{CertifiedSource, CertifiedSourceAppend, CoreRecord, SourceKey};

/// Static, capture-owned staging storage used by logical document leaves.
///
/// Implementations may use concrete storage, but the runtime only owns the
/// bounded lifecycle protocol and never performs I/O itself.
pub trait DocumentRecordSpool: Sized + Send + 'static {
    fn new(resources: SourceBackedRouteResources) -> SourceBackedRouteResult<Self>;

    fn push(&mut self, record: CoreRecord) -> SourceBackedRouteResult<()>;

    fn replay(
        self,
        emit: impl FnMut(CoreRecord) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedRouteResult<()>;
}

/// The only write surface available while one changed document is projected.
pub struct ChangedDocumentSink<'sink, 'writer, L: CaptureLifecycleSink, S: DocumentRecordSpool> {
    target: ChangedDocumentTarget<'sink, 'writer, L>,
    deferred: Option<S>,
    logical_base: Option<CertifiedSource>,
    source: Option<SourceKey>,
    emitted_core_records: u64,
    record_rejections: SourceBackedRecordRejectionDrafts,
}

enum ChangedDocumentTarget<'sink, 'writer, L: CaptureLifecycleSink> {
    Generation(&'sink mut SourceBackedGenerationSink<'writer, L>),
    Parallel(
        &'sink mut ParallelLeafScanEmitter<
            'writer,
            DocumentLeafCompletion,
            SourceBackedRouteError,
            L::Preparation,
        >,
    ),
}

pub(super) struct ValidatedDocumentTerminal {
    terminal: DocumentSourceTerminal,
    certificate: CertifiedSource,
    append: Option<CertifiedSourceAppend>,
    replay_fingerprint: Option<DocumentLeafFingerprint>,
}

impl<'sink, 'writer, L, S> ChangedDocumentSink<'sink, 'writer, L, S>
where
    L: CaptureLifecycleSink,
    S: DocumentRecordSpool,
{
    pub(super) fn new(sink: &'sink mut SourceBackedGenerationSink<'writer, L>) -> Self {
        Self {
            target: ChangedDocumentTarget::Generation(sink),
            deferred: None,
            logical_base: None,
            source: None,
            emitted_core_records: 0,
            record_rejections: Default::default(),
        }
    }

    pub(super) fn logical(
        sink: &'sink mut SourceBackedGenerationSink<'writer, L>,
    ) -> SourceBackedRouteResult<Self> {
        let deferred = S::new(sink.route_resources())?;
        Ok(Self {
            target: ChangedDocumentTarget::Generation(sink),
            deferred: Some(deferred),
            logical_base: None,
            source: None,
            emitted_core_records: 0,
            record_rejections: Default::default(),
        })
    }

    pub(super) fn parallel_logical(
        emitter: &'sink mut ParallelLeafScanEmitter<
            'writer,
            DocumentLeafCompletion,
            SourceBackedRouteError,
            L::Preparation,
        >,
        logical_base: Option<CertifiedSource>,
    ) -> SourceBackedRouteResult<Self> {
        let deferred = S::new(emitter.route_resources())?;
        Ok(Self {
            target: ChangedDocumentTarget::Parallel(emitter),
            deferred: Some(deferred),
            logical_base,
            source: None,
            emitted_core_records: 0,
            record_rejections: Default::default(),
        })
    }

    pub fn begin_source(&mut self, source: SourceKey) -> SourceBackedRouteResult<()> {
        if self.source.is_some() {
            return Err(document_internal(
                "document adapter began more than one source for one observed leaf",
            ));
        }
        if self.deferred.is_none() {
            match &mut self.target {
                ChangedDocumentTarget::Generation(sink) => sink
                    .begin_source(source.clone())
                    .map_err(route_coordinator_error::<L>)?,
                ChangedDocumentTarget::Parallel(emitter) => emitter
                    .begin(ParallelLeafScanBegin::replace(source.clone()))
                    .map_err(|_| {
                        document_internal("independent document leaf scan was cancelled")
                    })?,
            }
        }
        self.source = Some(source);
        Ok(())
    }

    pub fn emit_core_record(&mut self, record: CoreRecord) -> SourceBackedRouteResult<()> {
        let source = self.source.as_ref().ok_or_else(|| {
            document_internal("document adapter emitted before beginning its source")
        })?;
        if !record.source.exact_descriptor_eq(source) {
            return Err(document_changed(
                "document adapter emitted a row outside its active exact source",
            ));
        }
        if let Some(deferred) = self.deferred.as_mut() {
            deferred.push(record)?;
        } else {
            match &mut self.target {
                ChangedDocumentTarget::Generation(sink) => sink
                    .add_core_record(record)
                    .map_err(route_coordinator_error::<L>)?,
                ChangedDocumentTarget::Parallel(emitter) => emitter
                    .emit_core_record(record)
                    .map_err(document_emit_error)?,
            }
        }
        self.emitted_core_records = self
            .emitted_core_records
            .checked_add(1)
            .ok_or_else(|| document_internal("document emission count overflowed"))?;
        Ok(())
    }

    pub fn record_rejections(&mut self, rejections: SourceBackedRecordRejectionDrafts) {
        self.record_rejections.merge(rejections);
    }

    pub fn record_rejection(&mut self, rejection: SourceBackedRecordRejectionDraft) {
        self.record_rejections.record(rejection);
    }

    pub(super) fn take_record_rejections(&mut self) -> SourceBackedRecordRejectionDrafts {
        std::mem::take(&mut self.record_rejections)
    }

    pub(super) fn preserve_record_rejections_on_failure(&mut self) {
        let rejections = std::mem::take(&mut self.record_rejections);
        if let ChangedDocumentTarget::Generation(sink) = &mut self.target {
            sink.record_failed_attempt_rejections(rejections);
        }
    }

    pub fn report_completed_bytes(&mut self, bytes: u64) -> SourceBackedRouteResult<()> {
        match &mut self.target {
            ChangedDocumentTarget::Generation(sink) => sink
                .report_completed_bytes(bytes)
                .map_err(route_coordinator_error::<L>),
            ChangedDocumentTarget::Parallel(_) => Err(document_internal(
                "parallel document leaves cannot report source byte progress",
            )),
        }
    }

    pub fn report_current_source_progress(
        &mut self,
        progress: SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()> {
        match &mut self.target {
            ChangedDocumentTarget::Generation(sink) => {
                sink.report_current_source_progress(progress)
            }
            ChangedDocumentTarget::Parallel(_) => Err(document_internal(
                "parallel document leaves cannot report current-source progress",
            )),
        }
    }

    pub(super) fn source(&self) -> SourceBackedRouteResult<&SourceKey> {
        self.source
            .as_ref()
            .ok_or_else(|| document_internal("document adapter did not begin its source"))
    }

    pub(super) fn preflight_terminal(
        &self,
        terminal: DocumentSourceTerminal,
        replay_fingerprint: Option<DocumentLeafFingerprint>,
        append_base: Option<&DocumentAppendBase<L>>,
    ) -> SourceBackedRouteResult<ValidatedDocumentTerminal> {
        let source = self.source()?.clone();
        if !terminal.source.exact_descriptor_eq(&source)
            || !terminal.opening.source().exact_descriptor_eq(&source)
            || !terminal.closing.source().exact_descriptor_eq(&source)
        {
            return Err(document_changed(
                "document terminal changed its active exact source descriptor",
            ));
        }
        let expected_emitted =
            append_base.map_or(Some(terminal.counts.indexed_documents), |base| {
                terminal
                    .counts
                    .indexed_documents
                    .checked_sub(base.certificate().counts().indexed_documents)
            });
        if expected_emitted != Some(self.emitted_core_records) {
            return Err(document_changed(
                "document terminal indexed count did not match forwarded Core records",
            ));
        }
        let certificate = terminal.certify(replay_fingerprint)?;
        let append = append_base
            .map(|base| certify_document_append(base.certificate(), certificate.clone()))
            .transpose()?;
        reject_all_rejected_document_source(&terminal)?;
        Ok(ValidatedDocumentTerminal {
            terminal,
            certificate,
            append,
            replay_fingerprint,
        })
    }

    pub(super) fn finish(
        mut self,
        validated: ValidatedDocumentTerminal,
        append_base: Option<DocumentAppendBase<L>>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        let ValidatedDocumentTerminal {
            terminal,
            certificate,
            append,
            replay_fingerprint,
        } = validated;
        let source = terminal.source.clone();
        let logical_base = self.deferred.as_ref().and_then(|_| {
            append_base
                .as_ref()
                .map(|base| base.certificate().clone())
                .or_else(|| self.logical_base.clone())
                .or_else(|| match &mut self.target {
                    ChangedDocumentTarget::Generation(sink) => {
                        sink.base_source(&terminal.source).cloned()
                    }
                    ChangedDocumentTarget::Parallel(_) => None,
                })
        });
        let retain_core_records = self.deferred.is_some()
            && logical_base.as_ref().is_some_and(|base| {
                terminal_matches_base(base, &terminal)
                    && replay_fingerprint.is_none_or(|fingerprint| {
                        document_frontier_fingerprint(base) == Some(fingerprint)
                    })
            });
        if let Some(deferred) = self.deferred.take() {
            if let Some(base) = append_base {
                let certificate_base = base.certificate().clone();
                let append = append.ok_or_else(|| {
                    document_internal("preflighted document append proof disappeared")
                })?;
                match &mut self.target {
                    ChangedDocumentTarget::Generation(sink) => {
                        match base {
                            DocumentAppendBase::Generation(base) => sink
                                .begin_source_append_from_base(base)
                                .map_err(route_coordinator_error::<L>)?,
                            DocumentAppendBase::Certificate(_) => sink
                                .begin_source_append(source.clone())
                                .map_err(route_coordinator_error::<L>)?,
                        };
                        deferred.replay(|record| {
                            sink.add_core_record(record)
                                .map_err(route_coordinator_error::<L>)
                        })?;
                        sink.certify_source_append(append)
                            .map_err(route_coordinator_error::<L>)?;
                    }
                    ChangedDocumentTarget::Parallel(emitter) => {
                        emitter
                            .begin(ParallelLeafScanBegin::append(
                                source.clone(),
                                certificate_base,
                            ))
                            .map_err(|_| {
                                document_internal("independent document leaf scan was cancelled")
                            })?;
                        deferred.replay(|record| {
                            emitter
                                .emit_core_record(record)
                                .map_err(document_emit_error)
                        })?;
                        emitter
                            .complete(ParallelLeafScanComplete::append(
                                append,
                                DocumentLeafCompletion {
                                    certificate: certificate.clone(),
                                    record_rejections: std::mem::take(&mut self.record_rejections),
                                },
                            ))
                            .map_err(|_| {
                                document_internal("independent document leaf scan was cancelled")
                            })?;
                    }
                }
                return Ok(certificate);
            }
            if retain_core_records {
                let completion = DocumentLeafCompletion {
                    certificate: certificate.clone(),
                    record_rejections: std::mem::take(&mut self.record_rejections),
                };
                match &mut self.target {
                    ChangedDocumentTarget::Generation(sink) => {
                        sink.retain_source(certificate.clone())
                            .map_err(route_coordinator_error::<L>)?;
                        sink.record_rejections(completion.record_rejections);
                    }
                    ChangedDocumentTarget::Parallel(emitter) => emitter
                        .complete(ParallelLeafScanComplete::retain(
                            certificate.clone(),
                            completion,
                        ))
                        .map_err(|_| {
                            document_internal("independent document leaf scan was cancelled")
                        })?,
                }
                return Ok(certificate);
            }
            let source = certificate.observation().source().clone();
            let completion = DocumentLeafCompletion {
                certificate: certificate.clone(),
                record_rejections: std::mem::take(&mut self.record_rejections),
            };
            match &mut self.target {
                ChangedDocumentTarget::Generation(sink) => {
                    sink.begin_source(source)
                        .map_err(route_coordinator_error::<L>)?;
                    deferred.replay(|record| {
                        sink.add_core_record(record)
                            .map_err(route_coordinator_error::<L>)
                    })?;
                    sink.certify_source(certificate.clone())
                        .map_err(route_coordinator_error::<L>)?;
                    sink.record_rejections(completion.record_rejections);
                }
                ChangedDocumentTarget::Parallel(emitter) => {
                    emitter
                        .begin(ParallelLeafScanBegin::replace(source))
                        .map_err(|_| {
                            document_internal("independent document leaf scan was cancelled")
                        })?;
                    deferred.replay(|record| {
                        emitter
                            .emit_core_record(record)
                            .map_err(document_emit_error)
                    })?;
                    emitter
                        .complete(ParallelLeafScanComplete::replace(
                            certificate.clone(),
                            completion,
                        ))
                        .map_err(|_| {
                            document_internal("independent document leaf scan was cancelled")
                        })?;
                }
            }
            return Ok(certificate);
        }
        let completion = DocumentLeafCompletion {
            certificate: certificate.clone(),
            record_rejections: std::mem::take(&mut self.record_rejections),
        };
        match &mut self.target {
            ChangedDocumentTarget::Generation(sink) => {
                sink.certify_source(certificate.clone())
                    .map_err(route_coordinator_error::<L>)?;
                sink.record_rejections(completion.record_rejections);
            }
            ChangedDocumentTarget::Parallel(emitter) => emitter
                .complete(ParallelLeafScanComplete::replace(
                    certificate.clone(),
                    completion,
                ))
                .map_err(|_| document_internal("independent document leaf scan was cancelled"))?,
        }
        Ok(certificate)
    }
}

fn certify_document_append(
    base: &CertifiedSource,
    current: CertifiedSource,
) -> SourceBackedRouteResult<CertifiedSourceAppend> {
    let frontier = base
        .frontier()
        .ok_or_else(|| document_changed("document append base has no certified frontier"))?;
    CertifiedSourceAppend::certify(
        base,
        current,
        frontier.certified_prefix_bytes(),
        *frontier.certified_prefix_digest(),
    )
    .map_err(|error| document_changed(error.to_string()))
}

fn route_coordinator_error<L: CaptureLifecycleSink>(
    error: SourceBackedCoordinatorError<L::Error>,
) -> SourceBackedRouteError {
    match error {
        SourceBackedCoordinatorError::CoreEmission(source) => source,
        error => {
            SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, error.to_string())
        }
    }
}

fn document_emit_error(error: ParallelLeafScanEmitError) -> SourceBackedRouteError {
    match error {
        ParallelLeafScanEmitError::Route(error) => error,
        ParallelLeafScanEmitError::Cancelled(_) => {
            document_internal("independent document leaf scan was cancelled")
        }
    }
}

fn terminal_matches_base(base: &CertifiedSource, terminal: &DocumentSourceTerminal) -> bool {
    base.observation() == &terminal.opening
        && terminal.opening == terminal.closing
        && base.parser_revision() == terminal.parser_revision
        && base.content_digest() == &terminal.content_digest
        && base.counts() == terminal.counts
}
