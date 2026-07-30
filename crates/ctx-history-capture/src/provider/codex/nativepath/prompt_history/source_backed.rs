//! Source-backed projection for Codex's ordinary `history.jsonl` prompt log.
//!
//! The catalog lineage supplied by discovery is durable identity. The path is
//! only a route used to acquire one retained ordinary-file capability. Parsing,
//! certification, exact hydration, and final route checks all use that
//! capability rather than reopening a canonicalized pathname.

use std::{
    fs::{File, Metadata},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    CertifiedSourceAppend, ContentSourceResolver, EventHydrationRequest, EventIdentityInput,
    HydratedProviderRecord, HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, PositionStability,
    ProjectionContractError, ScannedSourceCounts, SessionHydrationRequest, SessionIdentityInput,
    SourceAnchor, SourceFrontier, SourceKey, SourceObservation, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
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

mod hydration;
mod path;
pub(crate) use hydration::CodexPromptHistorySourceBackedResolverV0;
use path::absolute_lexical_path;

const SOURCE_FORMAT: &str = "codex_history_jsonl";
const SOURCE_SCHEMA_VARIANT: &str = "codex-prompt-history-jsonl-v1";
const SOURCE_IDENTITY_VERSION: u32 = 1;
const SOURCE_REVISION_KIND: &str = "codex-prompt-history-ordinary-file-v2";
const PARSER_REVISION: &str = "codex-prompt-history-source-backed-v4";
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

#[cfg(test)]
std::thread_local! {
    static FULL_SCAN_BYTES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static PREFIX_HASH_BYTES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static AFTER_PREFIX_HASH_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn reset_prompt_history_work_counters() {
    FULL_SCAN_BYTES.set(0);
    PREFIX_HASH_BYTES.set(0);
}

#[cfg(test)]
fn prompt_history_full_scan_bytes() -> u64 {
    FULL_SCAN_BYTES.get()
}

#[cfg(test)]
fn prompt_history_prefix_hash_bytes() -> u64 {
    PREFIX_HASH_BYTES.get()
}

#[cfg(test)]
fn set_after_prompt_history_prefix_hash_hook(hook: impl FnOnce() + 'static) {
    AFTER_PREFIX_HASH_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "prompt-history prefix-hash hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_after_prompt_history_prefix_hash_hook() {
    AFTER_PREFIX_HASH_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

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
    Append,
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
    #[cfg(test)]
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
#[serde(deny_unknown_fields)]
struct ObservationWireV0 {
    length: u64,
    modified_after_epoch: bool,
    modified_seconds: u64,
    modified_nanos: u32,
    readonly: bool,
    ordinary_file_token: [u8; 32],
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

    let (opening_metadata, opening_token) =
        stable_current_ordinary_file_observation(&source.opened)?;
    if let Some(prior) = prior {
        if exact_ordinary_file_observation_matches(&opening_metadata, opening_token, prior)? {
            let _checkpoint = decode_checkpoint(prior)?;
            return Ok(CodexPromptHistorySourceBackedScanV0 {
                source,
                certificate: prior.clone(),
                disposition: CodexPromptHistorySourceBackedDispositionV0::Unchanged,
                emitted_documents: 0,
                #[cfg(test)]
                terminal: _checkpoint.terminal,
            });
        }
    }

    let frozen_len = opening_metadata.len();
    let analysis = walk_complete_records(&source.opened, frozen_len, |_| Ok(()))?;
    verify_frozen_prefix(&source.opened, frozen_len, analysis.whole_source_digest)?;

    let observation_wire = observation_wire(
        &opening_metadata,
        opening_token,
        analysis.whole_source_digest,
    )?;
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
        let projection_analysis = walk_complete_records(&source.opened, frozen_len, |record| {
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
    verify_frozen_prefix(&source.opened, frozen_len, analysis.whole_source_digest)?;

    Ok(CodexPromptHistorySourceBackedScanV0 {
        source,
        certificate,
        disposition,
        emitted_documents,
        #[cfg(test)]
        terminal: analysis.terminal,
    })
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
                    if CertifiedSourceAppend::certify(
                        prior,
                        current.clone(),
                        checkpoint.certified_prefix_bytes,
                        prefix_digest,
                    )
                    .is_ok()
                    {
                        return Ok((
                            CodexPromptHistorySourceBackedDispositionV0::Append,
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
    frozen_len: u64,
    mut retained: impl FnMut(&RetainedPromptRecord) -> CodexPromptHistorySourceBackedResultV0<()>,
) -> CodexPromptHistorySourceBackedResultV0<ScanAnalysis> {
    let mut reader = BufReader::new(opened_file_from_start(source)?.take(frozen_len));
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
            #[cfg(test)]
            FULL_SCAN_BYTES.with(|bytes| {
                bytes.set(
                    bytes
                        .get()
                        .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX)),
                );
            });
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

    if offset != frozen_len {
        return Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged);
    }
    source.revalidate_same_object()?;
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
    for _ in 0..2 {
        source.revalidate_same_object()?;
        let before = source.current_ordinary_file_token()?;
        let observed_len = source.file().metadata()?.len();
        let before_hash = source.current_ordinary_file_token()?;
        if before != before_hash {
            continue;
        }

        let digest = read_opened_prefix(source, target, observed_len)?;
        if digest.is_some() {
            #[cfg(test)]
            run_after_prompt_history_prefix_hash_hook();
        }
        let confirmation = read_opened_prefix(source, target, observed_len)?;
        if digest != confirmation {
            return Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged);
        }

        let after_hash = source.current_ordinary_file_token()?;
        source.revalidate_same_object()?;
        let after = source.current_ordinary_file_token()?;
        if before != after_hash || after_hash != after {
            continue;
        }
        return Ok(digest);
    }
    Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged)
}

fn read_opened_prefix(
    source: &OpenedProviderSourceFile,
    target: u64,
    observed_len: u64,
) -> CodexPromptHistorySourceBackedResultV0<Option<[u8; 32]>> {
    if target > observed_len {
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
        #[cfg(test)]
        PREFIX_HASH_BYTES.with(|hashed| {
            hashed.set(
                hashed
                    .get()
                    .saturating_add(u64::try_from(count).unwrap_or(u64::MAX)),
            );
        });
        digest.update(&bytes[..count]);
        remaining = remaining.saturating_sub(
            u64::try_from(count)
                .map_err(|_| CodexPromptHistorySourceBackedErrorV0::CountMismatch)?,
        );
    }
    Ok(Some(digest.finalize().into()))
}

fn verify_frozen_prefix(
    source: &OpenedProviderSourceFile,
    frozen_len: u64,
    expected_digest: [u8; 32],
) -> CodexPromptHistorySourceBackedResultV0<()> {
    let actual = hash_opened_prefix(source, frozen_len)?
        .ok_or(CodexPromptHistorySourceBackedErrorV0::SourceChanged)?;
    if actual != expected_digest {
        return Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged);
    }
    Ok(())
}

fn observation_wire(
    metadata: &Metadata,
    ordinary_file_token: [u8; 32],
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
        ordinary_file_token,
        whole_source_digest,
    })
}

fn stable_current_ordinary_file_observation(
    source: &OpenedProviderSourceFile,
) -> CodexPromptHistorySourceBackedResultV0<(Metadata, [u8; 32])> {
    let opened_token = source.ordinary_file_token();
    if source.current_ordinary_file_token()? == opened_token
        && source.revalidate().is_ok()
        && source.current_ordinary_file_token()? == opened_token
    {
        return Ok((source.metadata().clone(), opened_token));
    }

    for _ in 0..2 {
        source.revalidate_same_object()?;
        let before = source.current_ordinary_file_token()?;
        let metadata = source.file().metadata()?;
        let after_metadata = source.current_ordinary_file_token()?;
        source.revalidate_same_object()?;
        let after = source.current_ordinary_file_token()?;
        if before == after_metadata && after_metadata == after {
            return Ok((metadata, after));
        }
    }
    Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged)
}

fn exact_ordinary_file_observation_matches(
    metadata: &Metadata,
    ordinary_file_token: [u8; 32],
    expected: &CertifiedSource,
) -> CodexPromptHistorySourceBackedResultV0<bool> {
    if expected.parser_revision() != PARSER_REVISION
        || expected.observation().revision_kind() != SOURCE_REVISION_KIND
    {
        return Ok(false);
    }
    let expected_observation = decode_observation(expected)?;
    Ok(observation_wire(
        metadata,
        ordinary_file_token,
        expected_observation.whole_source_digest,
    )? == expected_observation)
}

fn decode_observation(
    certificate: &CertifiedSource,
) -> CodexPromptHistorySourceBackedResultV0<ObservationWireV0> {
    serde_json::from_slice(certificate.observation().revision())
        .map_err(|_| CodexPromptHistorySourceBackedErrorV0::InvalidCheckpoint)
}

/// Revalidates one previously staged prompt-history snapshot while allowing
/// the provider to append bytes beyond its frozen observation boundary.
pub(crate) fn revalidate_codex_prompt_history_source_backed_v0(
    source: &CodexPromptHistorySourceBackedSourceV0,
    expected: &CertifiedSource,
) -> CodexPromptHistorySourceBackedResultV0<()> {
    expected.validate_contract()?;
    if expected.parser_revision() != PARSER_REVISION
        || !source
            .source
            .exact_descriptor_eq(expected.observation().source())
        || expected.observation().revision_kind() != SOURCE_REVISION_KIND
    {
        return Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged);
    }
    let observation = decode_observation(expected)?;
    let checkpoint = decode_checkpoint(expected)?;
    if checkpoint.certified_prefix_bytes > observation.length {
        return Err(CodexPromptHistorySourceBackedErrorV0::InvalidCheckpoint);
    }
    let (current_metadata, current_token) =
        stable_current_ordinary_file_observation(&source.opened)?;
    if observation_wire(
        &current_metadata,
        current_token,
        observation.whole_source_digest,
    )? == observation
    {
        return Ok(());
    }
    verify_frozen_prefix(
        &source.opened,
        observation.length,
        observation.whole_source_digest,
    )
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
        agent_type: AgentType::Primary.as_str().to_owned(),
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

#[cfg(test)]
mod tests;
