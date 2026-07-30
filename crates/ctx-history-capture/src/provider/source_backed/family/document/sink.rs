use std::io::{BufReader, Seek, SeekFrom, Write};

use super::*;

/// The only write surface available while one changed document is projected.
///
/// Ordinary documents stream directly to the active generation. Logical
/// snapshots spool to an anonymous private file until their completed
/// certificate proves whether the live Tantivy generation needs a mutation.
pub(crate) struct ChangedDocumentSink<'sink, 'writer> {
    target: ChangedDocumentTarget<'sink, 'writer>,
    deferred: Option<DeferredLexicalDocuments>,
    logical_base: Option<CertifiedSource>,
    source: Option<SourceKey>,
    emitted_documents: u64,
}

struct DeferredLexicalDocuments {
    file: std::fs::File,
}

impl DeferredLexicalDocuments {
    fn new() -> SourceBackedRouteResult<Self> {
        let file = tempfile::tempfile().map_err(|error| {
            document_internal(format!(
                "could not create private logical-snapshot staging file: {error}"
            ))
        })?;
        Ok(Self { file })
    }

    fn push(&mut self, document: &LexicalDocument) -> SourceBackedRouteResult<()> {
        document
            .validate_contract()
            .map_err(document_contract_error)?;
        serde_json::to_writer(&mut self.file, document).map_err(|error| {
            document_internal(format!(
                "could not encode logical-snapshot staging document: {error}"
            ))
        })?;
        self.file.write_all(b"\n").map_err(|error| {
            document_internal(format!(
                "could not delimit logical-snapshot staging document: {error}"
            ))
        })
    }

    fn replay(
        mut self,
        mut emit: impl FnMut(LexicalDocument) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedRouteResult<()> {
        self.file.flush().map_err(|error| {
            document_internal(format!(
                "could not flush logical-snapshot staging documents: {error}"
            ))
        })?;
        self.file.seek(SeekFrom::Start(0)).map_err(|error| {
            document_internal(format!(
                "could not rewind logical-snapshot staging documents: {error}"
            ))
        })?;
        let reader = BufReader::new(self.file);
        for document in serde_json::Deserializer::from_reader(reader).into_iter::<LexicalDocument>()
        {
            let document = document.map_err(|error| {
                document_internal(format!(
                    "could not decode logical-snapshot staging document: {error}"
                ))
            })?;
            emit(document)?;
        }
        Ok(())
    }
}

enum ChangedDocumentTarget<'sink, 'writer> {
    Generation(&'sink mut SourceBackedGenerationSink<'writer>),
    Parallel(&'sink mut ParallelLeafScanEmitter<'writer, CertifiedSource, SourceBackedRouteError>),
}

impl<'sink, 'writer> ChangedDocumentSink<'sink, 'writer> {
    pub(super) fn new(sink: &'sink mut SourceBackedGenerationSink<'writer>) -> Self {
        Self {
            target: ChangedDocumentTarget::Generation(sink),
            deferred: None,
            logical_base: None,
            source: None,
            emitted_documents: 0,
        }
    }

    pub(super) fn logical(
        sink: &'sink mut SourceBackedGenerationSink<'writer>,
    ) -> SourceBackedRouteResult<Self> {
        Ok(Self {
            target: ChangedDocumentTarget::Generation(sink),
            deferred: Some(DeferredLexicalDocuments::new()?),
            logical_base: None,
            source: None,
            emitted_documents: 0,
        })
    }

    pub(super) fn parallel(
        emitter: &'sink mut ParallelLeafScanEmitter<
            'writer,
            CertifiedSource,
            SourceBackedRouteError,
        >,
    ) -> Self {
        Self {
            target: ChangedDocumentTarget::Parallel(emitter),
            deferred: None,
            logical_base: None,
            source: None,
            emitted_documents: 0,
        }
    }

    pub(super) fn parallel_logical(
        emitter: &'sink mut ParallelLeafScanEmitter<
            'writer,
            CertifiedSource,
            SourceBackedRouteError,
        >,
        logical_base: Option<CertifiedSource>,
    ) -> SourceBackedRouteResult<Self> {
        Ok(Self {
            target: ChangedDocumentTarget::Parallel(emitter),
            deferred: Some(DeferredLexicalDocuments::new()?),
            logical_base,
            source: None,
            emitted_documents: 0,
        })
    }

    pub(crate) fn begin_source(&mut self, source: SourceKey) -> SourceBackedRouteResult<()> {
        if self.source.is_some() {
            return Err(document_internal(
                "document adapter began more than one source for one observed leaf",
            ));
        }
        if self.deferred.is_none() {
            match &mut self.target {
                ChangedDocumentTarget::Generation(sink) => sink
                    .begin_source(source.clone())
                    .map_err(route_coordinator_error)?,
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

    pub(crate) fn emit_document(
        &mut self,
        document: LexicalDocument,
    ) -> SourceBackedRouteResult<()> {
        let source = self.source.as_ref().ok_or_else(|| {
            document_internal("document adapter emitted before beginning its source")
        })?;
        if !document.source.exact_descriptor_eq(source)
            || !document.locator.source().exact_descriptor_eq(source)
        {
            return Err(document_changed(
                "document adapter emitted a row outside its active exact source",
            ));
        }
        if let Some(deferred) = self.deferred.as_mut() {
            deferred.push(&document)?;
        } else {
            match &mut self.target {
                ChangedDocumentTarget::Generation(sink) => {
                    sink.add_document(document)
                        .map_err(route_coordinator_error)?;
                }
                ChangedDocumentTarget::Parallel(emitter) => {
                    emitter.emit_document(document).map_err(|_| {
                        document_internal("independent document leaf scan was cancelled")
                    })?;
                }
            }
        }
        self.emitted_documents = self
            .emitted_documents
            .checked_add(1)
            .ok_or_else(|| document_internal("document emission count overflowed"))?;
        Ok(())
    }

    pub(super) fn source(&self) -> SourceBackedRouteResult<&SourceKey> {
        self.source
            .as_ref()
            .ok_or_else(|| document_internal("document adapter did not begin its source"))
    }

    pub(super) fn finish(
        mut self,
        terminal: DocumentSourceTerminal,
        replay_fingerprint: Option<DocumentLeafFingerprint>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        let source = self.source()?;
        if !terminal.source.exact_descriptor_eq(source)
            || !terminal.opening.source().exact_descriptor_eq(source)
            || !terminal.closing.source().exact_descriptor_eq(source)
        {
            return Err(document_changed(
                "document terminal changed its active exact source descriptor",
            ));
        }
        if terminal.counts.indexed_documents != self.emitted_documents {
            return Err(document_changed(
                "document terminal indexed count did not match forwarded documents",
            ));
        }
        let certificate = terminal.certify(replay_fingerprint)?;
        let exact_base = match &mut self.target {
            ChangedDocumentTarget::Generation(sink) => sink
                .base_source(certificate.observation().source())
                .filter(|base| *base == &certificate)
                .cloned(),
            ChangedDocumentTarget::Parallel(_) => {
                self.logical_base.take().filter(|base| base == &certificate)
            }
        };
        if let Some(deferred) = self.deferred.take() {
            if exact_base.is_some() {
                match &mut self.target {
                    ChangedDocumentTarget::Generation(sink) => sink
                        .retain_source(certificate.clone())
                        .map_err(route_coordinator_error)?,
                    ChangedDocumentTarget::Parallel(emitter) => emitter
                        .complete(ParallelLeafScanComplete::retain(
                            certificate.clone(),
                            certificate.clone(),
                        ))
                        .map_err(|_| {
                            document_internal("independent document leaf scan was cancelled")
                        })?,
                }
                return Ok(certificate);
            }
            let source = certificate.observation().source().clone();
            match &mut self.target {
                ChangedDocumentTarget::Generation(sink) => {
                    sink.begin_source(source).map_err(route_coordinator_error)?;
                    deferred.replay(|document| {
                        sink.add_document(document).map_err(route_coordinator_error)
                    })?;
                    sink.certify_source(certificate.clone())
                        .map_err(route_coordinator_error)?;
                }
                ChangedDocumentTarget::Parallel(emitter) => {
                    emitter
                        .begin(ParallelLeafScanBegin::replace(source))
                        .map_err(|_| {
                            document_internal("independent document leaf scan was cancelled")
                        })?;
                    deferred.replay(|document| {
                        emitter.emit_document(document).map_err(|_| {
                            document_internal("independent document leaf scan was cancelled")
                        })
                    })?;
                    emitter
                        .complete(ParallelLeafScanComplete::replace(
                            certificate.clone(),
                            certificate.clone(),
                        ))
                        .map_err(|_| {
                            document_internal("independent document leaf scan was cancelled")
                        })?;
                }
            }
            return Ok(certificate);
        }
        match &mut self.target {
            ChangedDocumentTarget::Generation(sink) => sink
                .certify_source(certificate.clone())
                .map_err(route_coordinator_error)?,
            ChangedDocumentTarget::Parallel(emitter) => emitter
                .complete(ParallelLeafScanComplete::replace(
                    certificate.clone(),
                    certificate.clone(),
                ))
                .map_err(|_| document_internal("independent document leaf scan was cancelled"))?,
        }
        Ok(certificate)
    }
}
