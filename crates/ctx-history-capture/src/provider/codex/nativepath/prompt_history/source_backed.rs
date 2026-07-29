//! Source-backed projection for Codex's ordinary `history.jsonl` prompt log.
//!
//! The catalog lineage supplied by discovery is durable identity. The path is
//! only a route used to acquire one retained ordinary-file capability. Parsing,
//! certification, exact hydration, and final route checks all use that
//! capability rather than reopening a canonicalized pathname.

use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceAppend,
    ContentSourceResolver, EventHydrationRequest, EventIdentityInput, HydratedProviderRecord,
    HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, PositionStability, ProjectionContractError,
    ScannedSourceCounts, SessionHydrationRequest, SessionIdentityInput, SourceAnchor,
    SourceFrontier, SourceKey, SourceObservation, SourceRecordLocator, SourceResolverContractError,
    StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::PromptLine;
use crate::{
    common::io::{open_provider_source_file, OpenedProviderSourceFile},
    CaptureError, MAX_PROVIDER_JSONL_LINE_BYTES,
};

const SOURCE_FORMAT: &str = "codex_history_jsonl";
const SOURCE_SCHEMA_VARIANT: &str = "codex-prompt-history-jsonl-v1";
const SOURCE_IDENTITY_VERSION: u32 = 1;
const SOURCE_REVISION_KIND: &str = "codex-prompt-history-ordinary-file-v1";
const PARSER_REVISION: &str = "codex-prompt-history-source-backed-v1";
const FRONTIER_KIND: &str = "codex-prompt-history-jsonl-frontier-v1";
const SESSION_KEY_NAMESPACE: &str = "codex.prompt-history.session";
const EVENT_POSITION_KIND: &str = "codex.prompt-history.raw-ordinal";
const LOGICAL_SESSION_KIND: &str = "codex-prompt-history-session";
const LOGICAL_EVENT_KIND: &str = "codex-prompt-history-event";
const CHECKPOINT_VERSION: u32 = 1;
const PAGE_MAX_DOCUMENTS: usize = 64;
const PAGE_MAX_RETAINED_BYTES: usize = 1024 * 1024;
const DOCUMENT_METADATA_MAX_BYTES: usize = 64 * 1024;
const MAX_HYDRATED_RECORD_BYTES: u64 = MAX_PROVIDER_JSONL_LINE_BYTES as u64 + 2;

#[derive(Debug, Error)]
pub(crate) enum CodexPromptHistorySourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Codex prompt-history prior certificate belongs to another source")]
    PriorSourceMismatch,
    #[error("Codex prompt-history source changed while it was being scanned")]
    SourceChanged,
    #[error("Codex prompt-history source-backed checkpoint is malformed or incompatible")]
    InvalidCheckpoint,
    #[error("Codex prompt-history source-backed counters overflowed or did not reconcile")]
    CountMismatch,
    #[error("Codex prompt-history source-backed document exceeds its page bound")]
    DocumentTooLarge,
    #[error("Codex prompt-history resolver received conflicting routes for one source")]
    DuplicateResolverSource,
    #[error("Codex prompt-history locator source is not registered with this resolver")]
    LocatorSourceNotFound,
    #[error("Codex prompt-history source-backed locator is malformed")]
    InvalidLocator,
    #[error("Codex prompt-history locator range exceeds the bounded JSONL record size")]
    LocatorRangeTooLarge,
    #[error("Codex prompt-history locator range is no longer present")]
    LocatorRangeMissing,
    #[error("Codex prompt-history locator record digest no longer matches provider bytes")]
    LocatorDigestMismatch,
    #[error("Codex prompt-history locator no longer decodes to the indexed provider event")]
    LocatorRecordMismatch,
}

pub(crate) type CodexPromptHistorySourceBackedResultV0<T> =
    Result<T, CodexPromptHistorySourceBackedErrorV0>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexPromptHistorySourceBackedInputV0 {
    path: PathBuf,
    catalog_lineage: [u8; 32],
}

impl CodexPromptHistorySourceBackedInputV0 {
    pub(crate) fn explicit(path: impl Into<PathBuf>, catalog_lineage: [u8; 32]) -> Self {
        Self {
            path: path.into(),
            catalog_lineage,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn source_key(&self) -> CodexPromptHistorySourceBackedResultV0<SourceKey> {
        Ok(SourceKey::derive(
            CaptureProvider::Codex.as_str(),
            SOURCE_FORMAT,
            SOURCE_SCHEMA_VARIANT,
            SOURCE_IDENTITY_VERSION,
            SourceAnchor::CatalogLineage(self.catalog_lineage),
        )?)
    }
}

/// One retained capability for an explicitly selected Codex prompt-history file.
#[derive(Debug, Clone)]
pub(crate) struct CodexPromptHistorySourceBackedSourceV0 {
    input: CodexPromptHistorySourceBackedInputV0,
    source: SourceKey,
    opened: Arc<OpenedProviderSourceFile>,
}

impl CodexPromptHistorySourceBackedSourceV0 {
    pub(crate) fn input(&self) -> &CodexPromptHistorySourceBackedInputV0 {
        &self.input
    }

    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn path(&self) -> &Path {
        self.input.path()
    }
}

#[derive(Debug, Clone)]
pub(crate) enum CodexPromptHistorySourceBackedDispositionV0 {
    Cold,
    Unchanged,
    Append { proof: CertifiedSourceAppend },
    Replacement,
}

#[derive(Debug)]
pub(crate) struct CodexPromptHistorySourceBackedPageV0 {
    pub(crate) source: SourceKey,
    pub(crate) documents: Vec<LexicalDocument>,
    pub(crate) retained_bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct CodexPromptHistorySourceBackedScanV0 {
    pub(crate) source: CodexPromptHistorySourceBackedSourceV0,
    pub(crate) certificate: CertifiedSource,
    pub(crate) disposition: CodexPromptHistorySourceBackedDispositionV0,
    pub(crate) emitted_documents: u64,
    pub(crate) terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CheckpointV0 {
    version: u32,
    certified_prefix_bytes: u64,
    complete_records: u64,
    terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ObservationWireV0 {
    length: u64,
    modified_after_epoch: bool,
    modified_seconds: u64,
    modified_nanos: u32,
    readonly: bool,
    whole_source_digest: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScanAnalysis {
    counts: ScannedSourceCounts,
    content_digest: [u8; 32],
    whole_source_digest: [u8; 32],
    terminal: bool,
}

#[derive(Debug)]
struct RetainedPromptRecord {
    line: PromptLine,
    byte_offset: u64,
    byte_length: u64,
    physical_ordinal: u64,
    record_digest: [u8; 32],
}

#[derive(Debug)]
struct PageEmitter<'a, Emit>
where
    Emit: FnMut(CodexPromptHistorySourceBackedPageV0) -> CodexPromptHistorySourceBackedResultV0<()>,
{
    source: &'a CodexPromptHistorySourceBackedSourceV0,
    emit: Emit,
    documents: Vec<LexicalDocument>,
    retained_bytes: usize,
    emitted_documents: u64,
}

impl<'a, Emit> PageEmitter<'a, Emit>
where
    Emit: FnMut(CodexPromptHistorySourceBackedPageV0) -> CodexPromptHistorySourceBackedResultV0<()>,
{
    fn new(source: &'a CodexPromptHistorySourceBackedSourceV0, emit: Emit) -> Self {
        Self {
            source,
            emit,
            documents: Vec::new(),
            retained_bytes: 0,
            emitted_documents: 0,
        }
    }

    fn push(&mut self, document: LexicalDocument) -> CodexPromptHistorySourceBackedResultV0<()> {
        let retained = retained_document_bytes(&document);
        if retained > PAGE_MAX_RETAINED_BYTES {
            return Err(CodexPromptHistorySourceBackedErrorV0::DocumentTooLarge);
        }
        if !self.documents.is_empty()
            && (self.documents.len() == PAGE_MAX_DOCUMENTS
                || self.retained_bytes.saturating_add(retained) > PAGE_MAX_RETAINED_BYTES)
        {
            self.flush()?;
        }
        self.retained_bytes = self.retained_bytes.saturating_add(retained);
        self.documents.push(document);
        Ok(())
    }

    fn flush(&mut self) -> CodexPromptHistorySourceBackedResultV0<()> {
        if self.documents.is_empty() {
            return Ok(());
        }
        let documents = std::mem::take(&mut self.documents);
        let retained_bytes = std::mem::take(&mut self.retained_bytes);
        self.emitted_documents = self
            .emitted_documents
            .checked_add(
                u64::try_from(documents.len())
                    .map_err(|_| CodexPromptHistorySourceBackedErrorV0::CountMismatch)?,
            )
            .ok_or(CodexPromptHistorySourceBackedErrorV0::CountMismatch)?;
        (self.emit)(CodexPromptHistorySourceBackedPageV0 {
            source: self.source.source.clone(),
            documents,
            retained_bytes,
        })
    }

    fn finish(mut self) -> CodexPromptHistorySourceBackedResultV0<u64> {
        self.flush()?;
        Ok(self.emitted_documents)
    }
}

/// Acquires one retained ordinary-file capability without a canonicalize/check/open sequence.
pub(crate) fn observe_codex_prompt_history_source_backed_explicit_v0(
    input: &CodexPromptHistorySourceBackedInputV0,
) -> CodexPromptHistorySourceBackedResultV0<CodexPromptHistorySourceBackedSourceV0> {
    let path = absolute_lexical_path(input.path())?;
    let opened = Arc::new(open_provider_source_file(&path)?);
    opened.revalidate()?;
    Ok(CodexPromptHistorySourceBackedSourceV0 {
        input: CodexPromptHistorySourceBackedInputV0 {
            path,
            catalog_lineage: input.catalog_lineage,
        },
        source: input.source_key()?,
        opened,
    })
}

/// Convenience entrypoint for a selected explicit source.
pub(crate) fn scan_codex_prompt_history_source_backed_explicit_v0(
    input: &CodexPromptHistorySourceBackedInputV0,
    prior: Option<&CertifiedSource>,
    emit: impl FnMut(CodexPromptHistorySourceBackedPageV0) -> CodexPromptHistorySourceBackedResultV0<()>,
) -> CodexPromptHistorySourceBackedResultV0<CodexPromptHistorySourceBackedScanV0> {
    let source = observe_codex_prompt_history_source_backed_explicit_v0(input)?;
    scan_codex_prompt_history_source_backed_v0(source, prior, emit)
}

/// Scans through the retained source capability and emits bounded lexical pages.
pub(crate) fn scan_codex_prompt_history_source_backed_v0(
    source: CodexPromptHistorySourceBackedSourceV0,
    prior: Option<&CertifiedSource>,
    emit: impl FnMut(CodexPromptHistorySourceBackedPageV0) -> CodexPromptHistorySourceBackedResultV0<()>,
) -> CodexPromptHistorySourceBackedResultV0<CodexPromptHistorySourceBackedScanV0> {
    if let Some(prior) = prior {
        prior.validate_contract()?;
        if !source
            .source
            .exact_descriptor_eq(prior.observation().source())
        {
            return Err(CodexPromptHistorySourceBackedErrorV0::PriorSourceMismatch);
        }
    }

    let opening_metadata = source.opened.metadata().clone();
    let analysis = walk_complete_records(&source.opened, |_| Ok(()))?;
    source.opened.revalidate()?;
    let closing_metadata = source.opened.file().metadata()?;
    if opening_metadata.len() != closing_metadata.len()
        || opening_metadata.modified().ok() != closing_metadata.modified().ok()
        || opening_metadata.permissions().readonly() != closing_metadata.permissions().readonly()
    {
        return Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged);
    }

    let observation_wire = observation_wire(&opening_metadata, analysis.whole_source_digest)?;
    let observation = SourceObservation::new(
        source.source.clone(),
        SOURCE_REVISION_KIND,
        serde_json::to_vec(&observation_wire)?,
    )?;
    let checkpoint = CheckpointV0 {
        version: CHECKPOINT_VERSION,
        certified_prefix_bytes: analysis.counts.certified_bytes,
        complete_records: analysis.counts.complete_records,
        terminal: analysis.terminal,
    };
    let frontier = SourceFrontier::new(
        FRONTIER_KIND,
        TypedKey::bytes(serde_json::to_vec(&checkpoint)?)?,
        analysis.counts.certified_bytes,
        analysis.content_digest,
    )?;
    let certificate = CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        PARSER_REVISION,
        analysis.content_digest,
        analysis.counts,
        Some(frontier),
    )?;

    let (disposition, emit_from_byte) = classify_disposition(&source, prior, &certificate)?;
    let emitted_documents = if matches!(
        disposition,
        CodexPromptHistorySourceBackedDispositionV0::Unchanged
    ) {
        0
    } else {
        let mut pages = PageEmitter::new(&source, emit);
        let projection_analysis = walk_complete_records(&source.opened, |record| {
            if record.byte_offset >= emit_from_byte {
                pages.push(lexical_document(&source, record)?)
            } else {
                Ok(())
            }
        })?;
        if projection_analysis != analysis {
            return Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged);
        }
        pages.finish()?
    };
    source.opened.revalidate()?;

    Ok(CodexPromptHistorySourceBackedScanV0 {
        source,
        certificate,
        disposition,
        emitted_documents,
        terminal: analysis.terminal,
    })
}

pub(crate) fn revalidate_codex_prompt_history_source_backed_v0(
    input: &CodexPromptHistorySourceBackedInputV0,
    certificate: &CertifiedSource,
) -> CodexPromptHistorySourceBackedResultV0<bool> {
    let scan =
        scan_codex_prompt_history_source_backed_explicit_v0(input, Some(certificate), |_| Ok(()))?;
    Ok(matches!(
        scan.disposition,
        CodexPromptHistorySourceBackedDispositionV0::Unchanged
    ))
}

fn classify_disposition(
    source: &CodexPromptHistorySourceBackedSourceV0,
    prior: Option<&CertifiedSource>,
    current: &CertifiedSource,
) -> CodexPromptHistorySourceBackedResultV0<(CodexPromptHistorySourceBackedDispositionV0, u64)> {
    let Some(prior) = prior else {
        return Ok((CodexPromptHistorySourceBackedDispositionV0::Cold, 0));
    };
    if prior == current {
        let checkpoint = decode_checkpoint(prior)?;
        return Ok((
            CodexPromptHistorySourceBackedDispositionV0::Unchanged,
            checkpoint.certified_prefix_bytes,
        ));
    }
    if prior.parser_revision() == PARSER_REVISION {
        if let Ok(checkpoint) = decode_checkpoint(prior) {
            if current.counts().certified_bytes >= checkpoint.certified_prefix_bytes {
                if let Some(prefix_digest) =
                    hash_opened_prefix(&source.opened, checkpoint.certified_prefix_bytes)?
                {
                    if let Ok(proof) = CertifiedSourceAppend::certify(
                        prior,
                        current.clone(),
                        checkpoint.certified_prefix_bytes,
                        prefix_digest,
                    ) {
                        return Ok((
                            CodexPromptHistorySourceBackedDispositionV0::Append { proof },
                            checkpoint.certified_prefix_bytes,
                        ));
                    }
                }
            }
        }
    }
    Ok((CodexPromptHistorySourceBackedDispositionV0::Replacement, 0))
}

fn walk_complete_records(
    source: &OpenedProviderSourceFile,
    mut retained: impl FnMut(&RetainedPromptRecord) -> CodexPromptHistorySourceBackedResultV0<()>,
) -> CodexPromptHistorySourceBackedResultV0<ScanAnalysis> {
    let mut reader = BufReader::new(opened_file_from_start(source)?);
    let mut whole = Sha256::new();
    let mut complete = Sha256::new();
    let mut offset = 0_u64;
    let mut ordinal = 0_u64;
    let mut retained_records = 0_u64;
    let mut rejected_records = 0_u64;
    let mut ignored_records = 0_u64;
    let mut certified_bytes = 0_u64;
    let mut terminal = true;

    loop {
        let record_offset = offset;
        let complete_before = complete.clone();
        let mut record_digest = Sha256::new();
        let mut bytes = Vec::new();
        let mut observed = 0_usize;
        let mut saw_any = false;
        let mut terminated = false;
        while !terminated {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                break;
            }
            saw_any = true;
            let take = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index.saturating_add(1));
            let chunk = &available[..take];
            whole.update(chunk);
            complete.update(chunk);
            record_digest.update(chunk);
            observed = observed
                .checked_add(chunk.len())
                .ok_or(CodexPromptHistorySourceBackedErrorV0::CountMismatch)?;
            if bytes.len() <= MAX_PROVIDER_JSONL_LINE_BYTES {
                let remaining = MAX_PROVIDER_JSONL_LINE_BYTES
                    .saturating_add(1)
                    .saturating_sub(bytes.len());
                bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            }
            offset = offset
                .checked_add(
                    u64::try_from(chunk.len())
                        .map_err(|_| CodexPromptHistorySourceBackedErrorV0::CountMismatch)?,
                )
                .ok_or(CodexPromptHistorySourceBackedErrorV0::CountMismatch)?;
            terminated = chunk.last() == Some(&b'\n');
            reader.consume(take);
        }
        if !saw_any {
            break;
        }
        if !terminated {
            complete = complete_before;
            terminal = false;
            break;
        }

        let classification = if observed > MAX_PROVIDER_JSONL_LINE_BYTES {
            RecordClassification::Rejected
        } else {
            let body = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
            let body = body.strip_suffix(b"\r").unwrap_or(body);
            if body.iter().all(u8::is_ascii_whitespace) {
                RecordClassification::Ignored
            } else {
                match serde_json::from_slice::<PromptLine>(body) {
                    Ok(line)
                        if !line.session_id.trim().is_empty()
                            && chrono::DateTime::from_timestamp(line.ts, 0).is_some() =>
                    {
                        RecordClassification::Retained(line)
                    }
                    _ => RecordClassification::Rejected,
                }
            }
        };
        match classification {
            RecordClassification::Retained(line) => {
                let byte_length = u64::try_from(observed)
                    .map_err(|_| CodexPromptHistorySourceBackedErrorV0::CountMismatch)?;
                retained(&RetainedPromptRecord {
                    line,
                    byte_offset: record_offset,
                    byte_length,
                    physical_ordinal: ordinal,
                    record_digest: record_digest.finalize().into(),
                })?;
                retained_records = retained_records
                    .checked_add(1)
                    .ok_or(CodexPromptHistorySourceBackedErrorV0::CountMismatch)?;
            }
            RecordClassification::Rejected => {
                rejected_records = rejected_records
                    .checked_add(1)
                    .ok_or(CodexPromptHistorySourceBackedErrorV0::CountMismatch)?;
            }
            RecordClassification::Ignored => {
                ignored_records = ignored_records
                    .checked_add(1)
                    .ok_or(CodexPromptHistorySourceBackedErrorV0::CountMismatch)?;
            }
        }
        ordinal = ordinal
            .checked_add(1)
            .ok_or(CodexPromptHistorySourceBackedErrorV0::CountMismatch)?;
        certified_bytes = offset;
    }

    if offset != source.len() {
        return Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged);
    }
    source.revalidate()?;
    let counts = ScannedSourceCounts {
        complete_records: ordinal,
        retained_records,
        rejected_records,
        ignored_records,
        indexed_documents: retained_records,
        certified_bytes,
    };
    Ok(ScanAnalysis {
        counts,
        content_digest: complete.finalize().into(),
        whole_source_digest: whole.finalize().into(),
        terminal,
    })
}

enum RecordClassification {
    Retained(PromptLine),
    Rejected,
    Ignored,
}

fn opened_file_from_start(
    source: &OpenedProviderSourceFile,
) -> CodexPromptHistorySourceBackedResultV0<File> {
    let mut file = source.file().try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}

fn hash_opened_prefix(
    source: &OpenedProviderSourceFile,
    target: u64,
) -> CodexPromptHistorySourceBackedResultV0<Option<[u8; 32]>> {
    if target > source.len() {
        return Ok(None);
    }
    let mut file = opened_file_from_start(source)?;
    let mut remaining = target;
    let mut digest = Sha256::new();
    let mut bytes = [0_u8; 64 * 1024];
    while remaining > 0 {
        let take = bytes
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let count = file.read(&mut bytes[..take])?;
        if count == 0 {
            return Ok(None);
        }
        digest.update(&bytes[..count]);
        remaining = remaining.saturating_sub(
            u64::try_from(count)
                .map_err(|_| CodexPromptHistorySourceBackedErrorV0::CountMismatch)?,
        );
    }
    source.revalidate()?;
    Ok(Some(digest.finalize().into()))
}

fn observation_wire(
    metadata: &std::fs::Metadata,
    whole_source_digest: [u8; 32],
) -> CodexPromptHistorySourceBackedResultV0<ObservationWireV0> {
    let (modified_after_epoch, duration) = match metadata.modified()?.duration_since(UNIX_EPOCH) {
        Ok(duration) => (true, duration),
        Err(error) => (false, error.duration()),
    };
    Ok(ObservationWireV0 {
        length: metadata.len(),
        modified_after_epoch,
        modified_seconds: duration.as_secs(),
        modified_nanos: duration.subsec_nanos(),
        readonly: metadata.permissions().readonly(),
        whole_source_digest,
    })
}

fn lexical_document(
    source: &CodexPromptHistorySourceBackedSourceV0,
    record: &RetainedPromptRecord,
) -> CodexPromptHistorySourceBackedResultV0<LexicalDocument> {
    let session_id = stable_session_id(&source.source, &record.line.session_id)?;
    let native_item_key = NativeItemKey::certified_position(
        EVENT_POSITION_KIND,
        TypedKey::U64(record.physical_ordinal),
        PositionStability::AppendStable,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &source.source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let locator = SourceRecordLocator::new(
        source.source.clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: record.byte_offset,
            byte_length: record.byte_length,
            physical_ordinal: record.physical_ordinal,
            native_session_key: Some(TypedKey::utf8(&record.line.session_id)?),
            native_event_key: Some(TypedKey::U64(record.physical_ordinal)),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        record.record_digest,
    )?;
    let body = prompt_lexical_body(&record.line.text);
    let occurred_at_unix_ms =
        chrono::DateTime::from_timestamp(record.line.ts, 0).map(|value| value.timestamp_millis());
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.source.clone(),
        locator,
        provider_session_id: bounded_metadata(&record.line.session_id),
        branch: None,
        source_path: source.path().to_str().and_then(bounded_metadata),
        agent_type: "codex".to_owned(),
        is_primary: true,
        event_sequence: record.physical_ordinal,
        occurred_at_unix_ms,
        event_type: "message".to_owned(),
        role: Some("user".to_owned()),
        body,
        workspace: None,
        cwd: None,
        touched_files: Vec::new(),
    })
}

fn prompt_lexical_body(text: &str) -> String {
    if text.is_empty() {
        "message".to_owned()
    } else {
        text.to_owned()
    }
}

fn stable_session_id(
    source: &SourceKey,
    native_session_id: &str,
) -> CodexPromptHistorySourceBackedResultV0<StableEntityId> {
    let native_session_key =
        NativeSessionKey::native_id(SESSION_KEY_NAMESPACE, TypedKey::utf8(native_session_id)?)?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn decode_checkpoint(
    certificate: &CertifiedSource,
) -> CodexPromptHistorySourceBackedResultV0<CheckpointV0> {
    if certificate.parser_revision() != PARSER_REVISION {
        return Err(CodexPromptHistorySourceBackedErrorV0::InvalidCheckpoint);
    }
    let frontier = certificate
        .frontier()
        .ok_or(CodexPromptHistorySourceBackedErrorV0::InvalidCheckpoint)?;
    if frontier.checkpoint_kind() != FRONTIER_KIND {
        return Err(CodexPromptHistorySourceBackedErrorV0::InvalidCheckpoint);
    }
    let TypedKey::Bytes(bytes) = frontier.checkpoint() else {
        return Err(CodexPromptHistorySourceBackedErrorV0::InvalidCheckpoint);
    };
    let checkpoint: CheckpointV0 = serde_json::from_slice(bytes)
        .map_err(|_| CodexPromptHistorySourceBackedErrorV0::InvalidCheckpoint)?;
    if checkpoint.version != CHECKPOINT_VERSION
        || checkpoint.certified_prefix_bytes != frontier.certified_prefix_bytes()
        || checkpoint.certified_prefix_bytes != certificate.counts().certified_bytes
        || checkpoint.complete_records != certificate.counts().complete_records
        || frontier.certified_prefix_digest() != certificate.content_digest()
    {
        return Err(CodexPromptHistorySourceBackedErrorV0::InvalidCheckpoint);
    }
    Ok(checkpoint)
}

fn retained_document_bytes(document: &LexicalDocument) -> usize {
    document
        .body
        .len()
        .saturating_add(document.provider_session_id.as_ref().map_or(0, String::len))
        .saturating_add(document.source_path.as_ref().map_or(0, String::len))
        .saturating_add(512)
}

fn bounded_metadata(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= DOCUMENT_METADATA_MAX_BYTES).then(|| value.to_owned())
}

fn absolute_lexical_path(path: &Path) -> CodexPromptHistorySourceBackedResultV0<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

/// Invocation-local resolver for exact prompt-history JSONL ranges.
#[derive(Debug)]
pub(crate) struct CodexPromptHistorySourceBackedResolverV0 {
    routes: HashMap<SourceKey, CodexPromptHistorySourceBackedSourceV0>,
}

impl CodexPromptHistorySourceBackedResolverV0 {
    pub(crate) fn new(
        routes: impl IntoIterator<Item = CodexPromptHistorySourceBackedSourceV0>,
    ) -> CodexPromptHistorySourceBackedResultV0<Self> {
        let mut registered = HashMap::<SourceKey, CodexPromptHistorySourceBackedSourceV0>::new();
        for route in routes {
            if let Some(existing) = registered.get(&route.source) {
                if !existing.source.exact_descriptor_eq(&route.source)
                    || existing.input != route.input
                {
                    return Err(CodexPromptHistorySourceBackedErrorV0::DuplicateResolverSource);
                }
                continue;
            }
            registered.insert(route.source.clone(), route);
        }
        Ok(Self { routes: registered })
    }

    fn route_for(
        &self,
        request: &EventHydrationRequest,
    ) -> CodexPromptHistorySourceBackedResultV0<&CodexPromptHistorySourceBackedSourceV0> {
        request.locator().validate_contract()?;
        let route = self
            .routes
            .get(request.locator().source())
            .ok_or(CodexPromptHistorySourceBackedErrorV0::LocatorSourceNotFound)?;
        if !route.source.exact_descriptor_eq(request.locator().source()) {
            return Err(CodexPromptHistorySourceBackedErrorV0::InvalidLocator);
        }
        Ok(route)
    }

    fn hydrate_exact(
        &self,
        request: &EventHydrationRequest,
    ) -> CodexPromptHistorySourceBackedResultV0<HydratedProviderRecord> {
        let route = self.route_for(request)?;
        hydrate_from_source(route, request)
    }
}

impl ContentSourceResolver for CodexPromptHistorySourceBackedResolverV0 {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        self.hydrate_exact(request).map_err(hydration_failure)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        let Some(first) = request.events().first() else {
            return Ok(Vec::new());
        };
        let first_route = self.route_for(first).map_err(hydration_failure)?;
        request
            .events()
            .iter()
            .map(|event| {
                let route = self.route_for(event).map_err(hydration_failure)?;
                if route.input != first_route.input {
                    return Err(HydrationFailure {
                        kind: HydrationFailureKind::InvalidLocator,
                        detail: "Codex prompt-history session hydration crossed source routes"
                            .to_owned(),
                    });
                }
                let (_, _, _, native_session_id) =
                    validate_locator(event.locator()).map_err(hydration_failure)?;
                let session_id = stable_session_id(event.locator().source(), &native_session_id)
                    .map_err(hydration_failure)?;
                if session_id != request.session_id() {
                    return Err(HydrationFailure {
                        kind: HydrationFailureKind::InvalidLocator,
                        detail: "Codex prompt-history locator belongs to another session"
                            .to_owned(),
                    });
                }
                hydrate_from_source(route, event).map_err(hydration_failure)
            })
            .collect()
    }
}

fn hydrate_from_source(
    source: &CodexPromptHistorySourceBackedSourceV0,
    request: &EventHydrationRequest,
) -> CodexPromptHistorySourceBackedResultV0<HydratedProviderRecord> {
    let locator = request.locator();
    let (byte_offset, byte_length, physical_ordinal, native_session_id) =
        validate_locator(locator)?;
    let range_end = byte_offset
        .checked_add(byte_length)
        .ok_or(CodexPromptHistorySourceBackedErrorV0::LocatorRangeTooLarge)?;
    if range_end > source.opened.len() {
        return Err(CodexPromptHistorySourceBackedErrorV0::LocatorRangeMissing);
    }
    if byte_offset != 0 {
        let boundary = source
            .opened
            .read_exact_range(byte_offset.saturating_sub(1), 1, 1)?;
        if boundary != [b'\n'] {
            return Err(CodexPromptHistorySourceBackedErrorV0::InvalidLocator);
        }
    }
    let length = usize::try_from(byte_length)
        .map_err(|_| CodexPromptHistorySourceBackedErrorV0::LocatorRangeTooLarge)?;
    let provider_bytes = source.opened.read_exact_range(
        byte_offset,
        length,
        usize::try_from(MAX_HYDRATED_RECORD_BYTES)
            .map_err(|_| CodexPromptHistorySourceBackedErrorV0::LocatorRangeTooLarge)?,
    )?;
    if !provider_bytes.ends_with(b"\n") {
        return Err(CodexPromptHistorySourceBackedErrorV0::InvalidLocator);
    }
    if &Sha256::digest(&provider_bytes)[..] != locator.record_digest() {
        return Err(CodexPromptHistorySourceBackedErrorV0::LocatorDigestMismatch);
    }
    let body = provider_bytes
        .strip_suffix(b"\n")
        .unwrap_or(&provider_bytes);
    let body = body.strip_suffix(b"\r").unwrap_or(body);
    let line: PromptLine = serde_json::from_slice(body)
        .map_err(|_| CodexPromptHistorySourceBackedErrorV0::LocatorRecordMismatch)?;
    if line.session_id != native_session_id
        || line.session_id.trim().is_empty()
        || chrono::DateTime::from_timestamp(line.ts, 0).is_none()
    {
        return Err(CodexPromptHistorySourceBackedErrorV0::LocatorRecordMismatch);
    }
    let session_id = stable_session_id(locator.source(), &line.session_id)?;
    let native_item_key = NativeItemKey::certified_position(
        EVENT_POSITION_KIND,
        TypedKey::U64(physical_ordinal),
        PositionStability::AppendStable,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source: locator.source(),
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    if event_id != request.event_id() {
        return Err(CodexPromptHistorySourceBackedErrorV0::LocatorRecordMismatch);
    }
    Ok(HydratedProviderRecord {
        event_id,
        provider_bytes: prompt_lexical_body(&line.text).into_bytes(),
    })
}

fn validate_locator(
    locator: &SourceRecordLocator,
) -> CodexPromptHistorySourceBackedResultV0<(u64, u64, u64, String)> {
    if locator.source().provider() != CaptureProvider::Codex.as_str()
        || locator.source().source_format() != SOURCE_FORMAT
        || locator.source().schema_variant() != SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != SOURCE_IDENTITY_VERSION
        || !matches!(locator.source().anchor(), SourceAnchor::CatalogLineage(_))
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(CodexPromptHistorySourceBackedErrorV0::InvalidLocator);
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key: Some(TypedKey::Utf8(native_session_id)),
        native_event_key: Some(TypedKey::U64(native_event_ordinal)),
    } = locator.coordinate()
    else {
        return Err(CodexPromptHistorySourceBackedErrorV0::InvalidLocator);
    };
    if *byte_length == 0 || *byte_length > MAX_HYDRATED_RECORD_BYTES {
        return Err(CodexPromptHistorySourceBackedErrorV0::LocatorRangeTooLarge);
    }
    if native_session_id.is_empty() || native_event_ordinal != physical_ordinal {
        return Err(CodexPromptHistorySourceBackedErrorV0::InvalidLocator);
    }
    Ok((
        *byte_offset,
        *byte_length,
        *physical_ordinal,
        native_session_id.clone(),
    ))
}

fn hydration_failure(error: CodexPromptHistorySourceBackedErrorV0) -> HydrationFailure {
    let kind = match &error {
        CodexPromptHistorySourceBackedErrorV0::LocatorDigestMismatch
        | CodexPromptHistorySourceBackedErrorV0::LocatorRecordMismatch
        | CodexPromptHistorySourceBackedErrorV0::Capture(
            CaptureError::SourceChangedDuringCapture
            | CaptureError::InvalidProviderTranscriptPath { .. },
        ) => HydrationFailureKind::StaleRecordEvidence,
        CodexPromptHistorySourceBackedErrorV0::LocatorRangeMissing => {
            HydrationFailureKind::MissingRecord
        }
        CodexPromptHistorySourceBackedErrorV0::SourceChanged => {
            HydrationFailureKind::StaleSourceEvidence
        }
        CodexPromptHistorySourceBackedErrorV0::InvalidLocator
        | CodexPromptHistorySourceBackedErrorV0::Resolver(_)
        | CodexPromptHistorySourceBackedErrorV0::LocatorRangeTooLarge
        | CodexPromptHistorySourceBackedErrorV0::LocatorSourceNotFound
        | CodexPromptHistorySourceBackedErrorV0::DuplicateResolverSource => {
            HydrationFailureKind::InvalidLocator
        }
        _ => HydrationFailureKind::TemporarilyUnavailable,
    };
    HydrationFailure {
        kind,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests;
