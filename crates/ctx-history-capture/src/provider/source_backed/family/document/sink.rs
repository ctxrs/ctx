use std::io::{BufReader, Seek, SeekFrom, Write};

use super::*;

/// One logical source may stage no more Core records than the provider-neutral
/// source-inventory entry ceiling.
const LOGICAL_SNAPSHOT_SPOOL_MAX_CORE_RECORDS: usize =
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
    deferred: Option<DeferredCoreRecords>,
    logical_base: Option<CertifiedSource>,
    source: Option<SourceKey>,
    emitted_core_records: u64,
}

struct DeferredCoreRecords {
    file: std::fs::File,
    budget: DeferredCoreRecordBudget,
    #[cfg(test)]
    cleanup_path: Option<tempfile::TempPath>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeferredCoreRecordLimits {
    core_records: usize,
    encoded_bytes: usize,
}

impl DeferredCoreRecordLimits {
    const PRODUCTION: Self = Self {
        core_records: LOGICAL_SNAPSHOT_SPOOL_MAX_CORE_RECORDS,
        encoded_bytes: LOGICAL_SNAPSHOT_SPOOL_MAX_ENCODED_BYTES,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeferredCoreRecordBound {
    CoreRecordCount,
    EncodedBytes,
}

impl std::fmt::Display for DeferredCoreRecordBound {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CoreRecordCount => "core-record-count",
            Self::EncodedBytes => "encoded-byte",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
enum DeferredCoreRecordAdmissionError {
    #[error(
        "logical-snapshot Core-record spool {bound} bound exceeded: \
         maximum {maximum}, observed {observed}"
    )]
    Bounds {
        bound: DeferredCoreRecordBound,
        maximum: usize,
        observed: usize,
    },
    #[error("logical-snapshot Core-record spool {bound} accounting overflowed")]
    Arithmetic { bound: DeferredCoreRecordBound },
}

#[derive(Debug)]
struct DeferredCoreRecordBudget {
    limits: DeferredCoreRecordLimits,
    core_records: usize,
    encoded_bytes: usize,
}

impl DeferredCoreRecordBudget {
    fn new(limits: DeferredCoreRecordLimits) -> Self {
        Self {
            limits,
            core_records: 0,
            encoded_bytes: 0,
        }
    }

    fn admit_core_record(&mut self) -> Result<(), DeferredCoreRecordAdmissionError> {
        let observed = self.core_records.checked_add(1).ok_or(
            DeferredCoreRecordAdmissionError::Arithmetic {
                bound: DeferredCoreRecordBound::CoreRecordCount,
            },
        )?;
        if observed > self.limits.core_records {
            return Err(DeferredCoreRecordAdmissionError::Bounds {
                bound: DeferredCoreRecordBound::CoreRecordCount,
                maximum: self.limits.core_records,
                observed,
            });
        }
        self.core_records = observed;
        Ok(())
    }

    fn check_encoded_bytes(&self, bytes: usize) -> Result<usize, DeferredCoreRecordAdmissionError> {
        let observed = self.encoded_bytes.checked_add(bytes).ok_or(
            DeferredCoreRecordAdmissionError::Arithmetic {
                bound: DeferredCoreRecordBound::EncodedBytes,
            },
        )?;
        if observed > self.limits.encoded_bytes {
            return Err(DeferredCoreRecordAdmissionError::Bounds {
                bound: DeferredCoreRecordBound::EncodedBytes,
                maximum: self.limits.encoded_bytes,
                observed,
            });
        }
        Ok(observed)
    }

    fn commit_encoded_bytes(
        &mut self,
        bytes: usize,
    ) -> Result<(), DeferredCoreRecordAdmissionError> {
        self.encoded_bytes = self.check_encoded_bytes(bytes)?;
        Ok(())
    }
}

struct BoundedDeferredWriter<'writer> {
    file: &'writer mut std::fs::File,
    budget: &'writer mut DeferredCoreRecordBudget,
    admission_error: Option<DeferredCoreRecordAdmissionError>,
}

impl BoundedDeferredWriter<'_> {
    fn reject(&mut self, error: DeferredCoreRecordAdmissionError) -> std::io::Error {
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

    fn take_admission_error(&mut self) -> Option<DeferredCoreRecordAdmissionError> {
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

impl DeferredCoreRecords {
    fn new() -> SourceBackedRouteResult<Self> {
        let file = tempfile::tempfile().map_err(|error| {
            document_internal(format!(
                "could not create private logical-snapshot staging file: {error}"
            ))
        })?;
        Ok(Self {
            file,
            budget: DeferredCoreRecordBudget::new(DeferredCoreRecordLimits::PRODUCTION),
            #[cfg(test)]
            cleanup_path: None,
        })
    }

    #[cfg(test)]
    fn test_with_limits(
        directory: &std::path::Path,
        limits: DeferredCoreRecordLimits,
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
                budget: DeferredCoreRecordBudget::new(limits),
                cleanup_path: Some(cleanup_path),
            },
            path,
        ))
    }

    fn push(&mut self, record: &CoreRecord) -> SourceBackedRouteResult<()> {
        self.budget
            .admit_core_record()
            .map_err(document_spool_admission_error)?;
        let mut writer = BoundedDeferredWriter {
            file: &mut self.file,
            budget: &mut self.budget,
            admission_error: None,
        };
        let encoded = serde_json::to_writer(&mut writer, record);
        if let Some(error) = writer.take_admission_error() {
            return Err(document_spool_admission_error(error));
        }
        encoded.map_err(|error| {
            document_internal(format!(
                "could not encode logical-snapshot staging Core record: {error}"
            ))
        })?;
        let delimited = writer.write_all(b"\n");
        if let Some(error) = writer.take_admission_error() {
            return Err(document_spool_admission_error(error));
        }
        delimited.map_err(|error| {
            document_internal(format!(
                "could not delimit logical-snapshot staging Core record: {error}"
            ))
        })
    }

    fn replay(
        mut self,
        mut emit: impl FnMut(CoreRecord) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedRouteResult<()> {
        self.file.flush().map_err(|error| {
            document_internal(format!(
                "could not flush logical-snapshot staging Core records: {error}"
            ))
        })?;
        self.file.seek(SeekFrom::Start(0)).map_err(|error| {
            document_internal(format!(
                "could not rewind logical-snapshot staging Core records: {error}"
            ))
        })?;
        #[cfg(test)]
        let _cleanup_path = self.cleanup_path.take();
        let reader = BufReader::new(self.file);
        for record in serde_json::Deserializer::from_reader(reader).into_iter::<CoreRecord>() {
            let record = record.map_err(|error| {
                document_internal(format!(
                    "could not decode logical-snapshot staging Core record: {error}"
                ))
            })?;
            emit(record)?;
        }
        Ok(())
    }
}

fn document_spool_admission_error(
    error: DeferredCoreRecordAdmissionError,
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
            emitted_core_records: 0,
        }
    }

    pub(super) fn logical(
        sink: &'sink mut SourceBackedGenerationSink<'writer>,
    ) -> SourceBackedRouteResult<Self> {
        Ok(Self {
            target: ChangedDocumentTarget::Generation(sink),
            deferred: Some(DeferredCoreRecords::new()?),
            logical_base: None,
            source: None,
            emitted_core_records: 0,
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
            emitted_core_records: 0,
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
            deferred: Some(DeferredCoreRecords::new()?),
            logical_base,
            source: None,
            emitted_core_records: 0,
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

    pub(crate) fn emit_core_record(&mut self, record: CoreRecord) -> SourceBackedRouteResult<()> {
        let source = self.source.as_ref().ok_or_else(|| {
            document_internal("document adapter emitted before beginning its source")
        })?;
        if !record.source.exact_descriptor_eq(source) {
            return Err(document_changed(
                "document adapter emitted a row outside its active exact source",
            ));
        }
        if let Some(deferred) = self.deferred.as_mut() {
            deferred.push(&record)?;
        } else {
            match &mut self.target {
                ChangedDocumentTarget::Generation(sink) => {
                    sink.add_core_record(record)
                        .map_err(route_coordinator_error)?;
                }
                ChangedDocumentTarget::Parallel(emitter) => {
                    emitter.emit_core_record(record).map_err(|_| {
                        document_internal("independent document leaf scan was cancelled")
                    })?;
                }
            }
        }
        self.emitted_core_records = self
            .emitted_core_records
            .checked_add(1)
            .ok_or_else(|| document_internal("document emission count overflowed"))?;
        Ok(())
    }

    pub(crate) fn report_completed_bytes(&mut self, bytes: u64) -> SourceBackedRouteResult<()> {
        match &mut self.target {
            ChangedDocumentTarget::Generation(sink) => sink
                .report_completed_bytes(bytes)
                .map_err(route_coordinator_error),
            ChangedDocumentTarget::Parallel(_) => Err(document_internal(
                "parallel document leaves cannot report source byte progress",
            )),
        }
    }

    pub(crate) fn report_current_source_progress(
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
        if terminal.counts.indexed_documents != self.emitted_core_records {
            return Err(document_changed(
                "document terminal indexed count did not match forwarded Core records",
            ));
        }
        let logical_base = self
            .logical_base
            .clone()
            .or_else(|| match &mut self.target {
                ChangedDocumentTarget::Generation(sink) => {
                    sink.base_source(&terminal.source).cloned()
                }
                ChangedDocumentTarget::Parallel(_) => None,
            });
        let retain_core_records = self.deferred.is_some()
            && logical_base
                .as_ref()
                .is_some_and(|base| terminal_matches_base(base, &terminal));
        let certificate = terminal.certify(replay_fingerprint)?;
        if let Some(deferred) = self.deferred.take() {
            if retain_core_records {
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
                    deferred.replay(|record| {
                        sink.add_core_record(record)
                            .map_err(route_coordinator_error)
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
                    deferred.replay(|record| {
                        emitter.emit_core_record(record).map_err(|_| {
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

fn terminal_matches_base(base: &CertifiedSource, terminal: &DocumentSourceTerminal) -> bool {
    base.observation() == &terminal.opening
        && terminal.opening == terminal.closing
        && base.parser_revision() == terminal.parser_revision
        && base.content_digest() == &terminal.content_digest
        && base.counts() == terminal.counts
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{
        derive_event_id, derive_session_id, CaptureProvider, EventIdentityInput, NativeItemKey,
        NativeSessionKey, SessionIdentityInput, SourceAnchor, TypedKey,
    };

    use super::*;

    fn core_record(sequence: u64, body: &str) -> CoreRecord {
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
        let mut record = CoreRecord::new_selected(
            event_id,
            session_id,
            session_id,
            source,
            sequence,
            "message",
            "primary",
            true,
            "synthetic-core-record-v1",
            body,
        )
        .unwrap();
        record.provider_session_id = Some("synthetic-session".to_owned());
        record.native_event_id = Some(TypedKey::U64(sequence));
        record.occurred_at_unix_ms = Some(sequence as i64);
        record.role = Some("user".to_owned());
        record
    }

    fn encoded_frame_bytes(record: &CoreRecord) -> usize {
        serde_json::to_vec(record).unwrap().len() + 1
    }

    #[test]
    fn logical_spool_admits_n_core_records_and_rejects_n_plus_one_before_writing() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let (mut spool, path) = DeferredCoreRecords::test_with_limits(
            temp.path(),
            DeferredCoreRecordLimits {
                core_records: 2,
                encoded_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        spool.push(&core_record(1, "first")).unwrap();
        spool.push(&core_record(2, "second")).unwrap();
        let admitted_bytes = std::fs::metadata(&path).unwrap().len();

        let error = spool.push(&core_record(3, "not admitted")).unwrap_err();
        assert_eq!(error.kind, SourceBackedRouteErrorKind::InvalidSource);
        assert!(error.detail.contains(
            "logical-snapshot Core-record spool core-record-count bound exceeded: \
             maximum 2, observed 3"
        ));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), admitted_bytes);

        drop(spool);
        assert!(!path.exists());
    }

    #[test]
    fn logical_spool_counts_framing_and_rejects_one_oversized_core_record() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let record = core_record(1, "frame must count");
        let encoded_record_bytes = serde_json::to_vec(&record).unwrap().len();
        let (mut spool, path) = DeferredCoreRecords::test_with_limits(
            temp.path(),
            DeferredCoreRecordLimits {
                core_records: 1,
                encoded_bytes: encoded_record_bytes,
            },
        )
        .unwrap();

        let error = spool.push(&record).unwrap_err();
        assert_eq!(error.kind, SourceBackedRouteErrorKind::InvalidSource);
        assert!(error.detail.contains(&format!(
            "logical-snapshot Core-record spool encoded-byte bound exceeded: maximum \
             {encoded_record_bytes}, observed {}",
            encoded_record_bytes + 1
        )));
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            encoded_record_bytes as u64
        );

        drop(spool);
        assert!(!path.exists());
    }

    #[test]
    fn logical_spool_arithmetic_error_is_invalid_source_and_cleans_up() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let (mut spool, path) = DeferredCoreRecords::test_with_limits(
            temp.path(),
            DeferredCoreRecordLimits {
                core_records: 1,
                encoded_bytes: usize::MAX,
            },
        )
        .unwrap();
        spool.budget.encoded_bytes = usize::MAX;

        let error = spool.push(&core_record(1, "overflow")).unwrap_err();
        assert_eq!(error.kind, SourceBackedRouteErrorKind::InvalidSource);
        assert_eq!(
            error.detail,
            "logical-snapshot Core-record spool encoded-byte accounting overflowed"
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);

        drop(spool);
        assert!(!path.exists());
    }

    #[test]
    fn logical_spool_replays_streamed_core_records_at_the_exact_byte_bound() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let records = [
            core_record(1, "first replay"),
            core_record(2, "second replay"),
        ];
        let expected_bytes = records.iter().map(encoded_frame_bytes).sum();
        let (mut spool, path) = DeferredCoreRecords::test_with_limits(
            temp.path(),
            DeferredCoreRecordLimits {
                core_records: records.len(),
                encoded_bytes: expected_bytes,
            },
        )
        .unwrap();
        for record in &records {
            spool.push(record).unwrap();
        }
        assert_eq!(spool.budget.core_records, records.len());
        assert_eq!(spool.budget.encoded_bytes, expected_bytes);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            expected_bytes as u64
        );

        let mut replayed = Vec::new();
        spool
            .replay(|record| {
                replayed.push((
                    record.event_id,
                    record.event_sequence,
                    record.content.normalized_body,
                ));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            replayed,
            records
                .iter()
                .map(|record| (
                    record.event_id,
                    record.event_sequence,
                    record.content.normalized_body.clone()
                ))
                .collect::<Vec<_>>()
        );
        assert!(!path.exists());
    }
}
