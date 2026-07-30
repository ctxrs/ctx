use std::io::{BufReader, Seek, SeekFrom, Write};

use super::*;

/// One logical source may stage no more documents than the provider-neutral
/// source-inventory entry ceiling.
const LOGICAL_SNAPSHOT_SPOOL_MAX_DOCUMENTS: usize =
    crate::PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES;
/// This matches the existing bounded source-document catalog byte ceiling.
///
/// Independent logical SQLite scans are capped at four workers, so live
/// disposable spool storage is bounded to one GiB across the worker set.
const LOGICAL_SNAPSHOT_SPOOL_MAX_ENCODED_BYTES: usize = 256 * 1024 * 1024;

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
    budget: DeferredLexicalDocumentBudget,
    #[cfg(test)]
    cleanup_path: Option<tempfile::TempPath>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeferredLexicalDocumentLimits {
    documents: usize,
    encoded_bytes: usize,
}

impl DeferredLexicalDocumentLimits {
    const PRODUCTION: Self = Self {
        documents: LOGICAL_SNAPSHOT_SPOOL_MAX_DOCUMENTS,
        encoded_bytes: LOGICAL_SNAPSHOT_SPOOL_MAX_ENCODED_BYTES,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeferredLexicalDocumentBound {
    DocumentCount,
    EncodedBytes,
}

impl std::fmt::Display for DeferredLexicalDocumentBound {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::DocumentCount => "document-count",
            Self::EncodedBytes => "encoded-byte",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
enum DeferredLexicalDocumentAdmissionError {
    #[error(
        "logical-snapshot lexical spool {bound} bound exceeded: \
         maximum {maximum}, observed {observed}"
    )]
    Bounds {
        bound: DeferredLexicalDocumentBound,
        maximum: usize,
        observed: usize,
    },
    #[error("logical-snapshot lexical spool {bound} accounting overflowed")]
    Arithmetic { bound: DeferredLexicalDocumentBound },
}

#[derive(Debug)]
struct DeferredLexicalDocumentBudget {
    limits: DeferredLexicalDocumentLimits,
    documents: usize,
    encoded_bytes: usize,
}

impl DeferredLexicalDocumentBudget {
    fn new(limits: DeferredLexicalDocumentLimits) -> Self {
        Self {
            limits,
            documents: 0,
            encoded_bytes: 0,
        }
    }

    fn admit_document(&mut self) -> Result<(), DeferredLexicalDocumentAdmissionError> {
        let observed = self.documents.checked_add(1).ok_or(
            DeferredLexicalDocumentAdmissionError::Arithmetic {
                bound: DeferredLexicalDocumentBound::DocumentCount,
            },
        )?;
        if observed > self.limits.documents {
            return Err(DeferredLexicalDocumentAdmissionError::Bounds {
                bound: DeferredLexicalDocumentBound::DocumentCount,
                maximum: self.limits.documents,
                observed,
            });
        }
        self.documents = observed;
        Ok(())
    }

    fn check_encoded_bytes(
        &self,
        bytes: usize,
    ) -> Result<usize, DeferredLexicalDocumentAdmissionError> {
        let observed = self.encoded_bytes.checked_add(bytes).ok_or(
            DeferredLexicalDocumentAdmissionError::Arithmetic {
                bound: DeferredLexicalDocumentBound::EncodedBytes,
            },
        )?;
        if observed > self.limits.encoded_bytes {
            return Err(DeferredLexicalDocumentAdmissionError::Bounds {
                bound: DeferredLexicalDocumentBound::EncodedBytes,
                maximum: self.limits.encoded_bytes,
                observed,
            });
        }
        Ok(observed)
    }

    fn commit_encoded_bytes(
        &mut self,
        bytes: usize,
    ) -> Result<(), DeferredLexicalDocumentAdmissionError> {
        self.encoded_bytes = self.check_encoded_bytes(bytes)?;
        Ok(())
    }
}

struct BoundedDeferredWriter<'writer> {
    file: &'writer mut std::fs::File,
    budget: &'writer mut DeferredLexicalDocumentBudget,
    admission_error: Option<DeferredLexicalDocumentAdmissionError>,
}

impl BoundedDeferredWriter<'_> {
    fn reject(&mut self, error: DeferredLexicalDocumentAdmissionError) -> std::io::Error {
        self.admission_error = Some(error);
        std::io::Error::new(std::io::ErrorKind::InvalidData, error)
    }

    fn check(&mut self, bytes: usize) -> std::io::Result<()> {
        self.budget
            .check_encoded_bytes(bytes)
            .map(|_| ())
            .map_err(|error| self.reject(error))
    }

    fn commit(&mut self, bytes: usize) -> std::io::Result<()> {
        self.budget
            .commit_encoded_bytes(bytes)
            .map_err(|error| self.reject(error))
    }

    fn take_admission_error(&mut self) -> Option<DeferredLexicalDocumentAdmissionError> {
        self.admission_error.take()
    }
}

impl Write for BoundedDeferredWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.check(buffer.len())?;
        let written = self.file.write(buffer)?;
        self.commit(written)?;
        Ok(written)
    }

    fn write_all(&mut self, buffer: &[u8]) -> std::io::Result<()> {
        self.check(buffer.len())?;
        self.file.write_all(buffer)?;
        self.commit(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl DeferredLexicalDocuments {
    fn new() -> SourceBackedRouteResult<Self> {
        let file = tempfile::tempfile().map_err(|error| {
            document_internal(format!(
                "could not create private logical-snapshot staging file: {error}"
            ))
        })?;
        Ok(Self {
            file,
            budget: DeferredLexicalDocumentBudget::new(DeferredLexicalDocumentLimits::PRODUCTION),
            #[cfg(test)]
            cleanup_path: None,
        })
    }

    #[cfg(test)]
    fn test_with_limits(
        directory: &std::path::Path,
        limits: DeferredLexicalDocumentLimits,
    ) -> SourceBackedRouteResult<(Self, std::path::PathBuf)> {
        let named = tempfile::NamedTempFile::new_in(directory).map_err(|error| {
            document_internal(format!(
                "could not create test logical-snapshot staging file: {error}"
            ))
        })?;
        let path = named.path().to_path_buf();
        let (file, cleanup_path) = named.into_parts();
        Ok((
            Self {
                file,
                budget: DeferredLexicalDocumentBudget::new(limits),
                cleanup_path: Some(cleanup_path),
            },
            path,
        ))
    }

    fn push(&mut self, document: &LexicalDocument) -> SourceBackedRouteResult<()> {
        document
            .validate_contract()
            .map_err(document_contract_error)?;
        self.budget
            .admit_document()
            .map_err(document_spool_admission_error)?;
        let mut writer = BoundedDeferredWriter {
            file: &mut self.file,
            budget: &mut self.budget,
            admission_error: None,
        };
        let encoded = serde_json::to_writer(&mut writer, document);
        if let Some(error) = writer.take_admission_error() {
            return Err(document_spool_admission_error(error));
        }
        encoded.map_err(|error| {
            document_internal(format!(
                "could not encode logical-snapshot staging document: {error}"
            ))
        })?;
        let delimited = writer.write_all(b"\n");
        if let Some(error) = writer.take_admission_error() {
            return Err(document_spool_admission_error(error));
        }
        delimited.map_err(|error| {
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
        #[cfg(test)]
        let _cleanup_path = self.cleanup_path.take();
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

fn document_spool_admission_error(
    error: DeferredLexicalDocumentAdmissionError,
) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
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

#[cfg(test)]
mod tests {
    use ctx_history_core::{
        derive_event_id, derive_session_id, CaptureProvider, EventIdentityInput,
        LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
        SessionIdentityInput, SourceAnchor, SourceRecordLocator, TypedKey,
    };
    use sha2::{Digest, Sha256};

    use super::*;

    fn lexical_document(sequence: u64, body: &str) -> LexicalDocument {
        let source = SourceKey::derive(
            CaptureProvider::Auggie.as_str(),
            "synthetic_logical_sqlite",
            "synthetic-logical-sqlite-v1",
            1,
            SourceAnchor::CatalogLineage([7; 32]),
        )
        .unwrap();
        let native_session_key =
            NativeSessionKey::native_id("synthetic.session", TypedKey::U64(1)).unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "synthetic-session",
            native_session_key: &native_session_key,
        })
        .unwrap();
        let native_item_key =
            NativeItemKey::native_id("synthetic.message", TypedKey::U64(sequence)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: "synthetic-message",
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .unwrap();
        let digest: [u8; 32] = Sha256::digest(body.as_bytes()).into();
        let locator = SourceRecordLocator::new(
            source.clone(),
            NativeRecordCoordinate::Document {
                object_key: TypedKey::U64(sequence),
                json_pointer: Some("/message".to_owned()),
            },
            LocatorRevisionPolicy::ExactSourceRevision,
            Some(digest),
            digest,
        )
        .unwrap();
        LexicalDocument {
            event_id,
            session_id,
            parent_session_id: None,
            root_session_id: session_id,
            source,
            locator,
            provider_session_id: Some("synthetic-session".to_owned()),
            branch: None,
            source_path: Some("/synthetic/logical.sqlite".to_owned()),
            agent_type: "primary".to_owned(),
            is_primary: true,
            event_sequence: sequence,
            occurred_at_unix_ms: Some(sequence as i64),
            event_type: "message".to_owned(),
            role: Some("user".to_owned()),
            body: body.to_owned(),
            workspace: None,
            cwd: None,
            touched_files: Vec::new(),
        }
    }

    fn encoded_frame_bytes(document: &LexicalDocument) -> usize {
        serde_json::to_vec(document).unwrap().len() + 1
    }

    #[test]
    fn logical_spool_admits_n_documents_and_rejects_n_plus_one_before_writing() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let (mut spool, path) = DeferredLexicalDocuments::test_with_limits(
            temp.path(),
            DeferredLexicalDocumentLimits {
                documents: 2,
                encoded_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        spool.push(&lexical_document(1, "first")).unwrap();
        spool.push(&lexical_document(2, "second")).unwrap();
        let admitted_bytes = std::fs::metadata(&path).unwrap().len();

        let error = spool
            .push(&lexical_document(3, "not admitted"))
            .unwrap_err();
        assert_eq!(error.kind, SourceBackedRouteErrorKind::InvalidSource);
        assert!(error.detail.contains(
            "logical-snapshot lexical spool document-count bound exceeded: \
             maximum 2, observed 3"
        ));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), admitted_bytes);

        drop(spool);
        assert!(!path.exists());
    }

    #[test]
    fn logical_spool_counts_framing_and_rejects_one_oversized_document() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let document = lexical_document(1, "frame must count");
        let encoded_document_bytes = serde_json::to_vec(&document).unwrap().len();
        let (mut spool, path) = DeferredLexicalDocuments::test_with_limits(
            temp.path(),
            DeferredLexicalDocumentLimits {
                documents: 1,
                encoded_bytes: encoded_document_bytes,
            },
        )
        .unwrap();

        let error = spool.push(&document).unwrap_err();
        assert_eq!(error.kind, SourceBackedRouteErrorKind::InvalidSource);
        assert!(error.detail.contains(&format!(
            "logical-snapshot lexical spool encoded-byte bound exceeded: maximum \
             {encoded_document_bytes}, observed {}",
            encoded_document_bytes + 1
        )));
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            encoded_document_bytes as u64
        );

        drop(spool);
        assert!(!path.exists());
    }

    #[test]
    fn logical_spool_arithmetic_error_is_invalid_source_and_cleans_up() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let (mut spool, path) = DeferredLexicalDocuments::test_with_limits(
            temp.path(),
            DeferredLexicalDocumentLimits {
                documents: 1,
                encoded_bytes: usize::MAX,
            },
        )
        .unwrap();
        spool.budget.encoded_bytes = usize::MAX;

        let error = spool.push(&lexical_document(1, "overflow")).unwrap_err();
        assert_eq!(error.kind, SourceBackedRouteErrorKind::InvalidSource);
        assert_eq!(
            error.detail,
            "logical-snapshot lexical spool encoded-byte accounting overflowed"
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);

        drop(spool);
        assert!(!path.exists());
    }

    #[test]
    fn logical_spool_replays_streamed_documents_at_the_exact_byte_bound() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let documents = [
            lexical_document(1, "first replay"),
            lexical_document(2, "second replay"),
        ];
        let expected_bytes = documents.iter().map(encoded_frame_bytes).sum();
        let (mut spool, path) = DeferredLexicalDocuments::test_with_limits(
            temp.path(),
            DeferredLexicalDocumentLimits {
                documents: documents.len(),
                encoded_bytes: expected_bytes,
            },
        )
        .unwrap();
        for document in &documents {
            spool.push(document).unwrap();
        }
        assert_eq!(spool.budget.documents, documents.len());
        assert_eq!(spool.budget.encoded_bytes, expected_bytes);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            expected_bytes as u64
        );

        let mut replayed = Vec::new();
        spool
            .replay(|document| {
                replayed.push((document.event_id, document.event_sequence, document.body));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            replayed,
            documents
                .iter()
                .map(|document| (
                    document.event_id,
                    document.event_sequence,
                    document.body.clone()
                ))
                .collect::<Vec<_>>()
        );
        assert!(!path.exists());
    }
}
