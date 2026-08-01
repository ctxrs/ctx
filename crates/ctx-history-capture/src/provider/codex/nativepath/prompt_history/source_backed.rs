//! Source-backed projection for Codex's ordinary `history.jsonl` prompt log.
//!
//! The catalog lineage supplied by discovery is durable identity. The path is
//! only a route used to acquire one retained ordinary-file capability. Parsing,
//! certification, direct Core publication, and final route checks all use that
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
    CertifiedSourceAppend, CoreRecord, CoreRecordError, EventIdentityInput, NativeItemKey,
    NativeSessionKey, PositionStability, ProjectionContractError, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceFrontier, SourceKey, SourceObservation,
    StableEntityId, TypedKey,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::PromptLine;
use crate::{
    common::io::{open_provider_source_file, OpenedProviderSourceFile},
    CaptureError, MAX_PROVIDER_JSONL_LINE_BYTES,
};

mod path;
mod projection;
use path::absolute_lexical_path;
use projection::{core_record, retained_record_bytes};

const SOURCE_FORMAT: &str = "codex_history_jsonl";
const SOURCE_SCHEMA_VARIANT: &str = "codex-prompt-history-jsonl-v1";
const SOURCE_IDENTITY_VERSION: u32 = 1;
const SOURCE_REVISION_KIND: &str = "codex-prompt-history-ordinary-file-v2";
const PARSER_REVISION: &str = "codex-prompt-history-core-record-v3";
const FRONTIER_KIND: &str = "codex-prompt-history-jsonl-frontier-v1";
const SESSION_KEY_NAMESPACE: &str = "codex.prompt-history.session";
const EVENT_POSITION_KIND: &str = "codex.prompt-history.raw-ordinal";
const LOGICAL_SESSION_KIND: &str = "codex-prompt-history-session";
const LOGICAL_EVENT_KIND: &str = "codex-prompt-history-event";
const CHECKPOINT_VERSION: u32 = 1;
const PAGE_MAX_DOCUMENTS: usize = 64;
const PAGE_MAX_RETAINED_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub(crate) enum CodexPromptHistorySourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
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
    #[error("Codex prompt-history Core record exceeds its page bound")]
    RecordTooLarge,
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
    source: SourceKey,
    opened: Arc<OpenedProviderSourceFile>,
}

impl CodexPromptHistorySourceBackedSourceV0 {
    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
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
    pub(crate) records: Vec<CoreRecord>,
    pub(crate) retained_bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct CodexPromptHistorySourceBackedScanV0 {
    pub(crate) source: CodexPromptHistorySourceBackedSourceV0,
    pub(crate) certificate: CertifiedSource,
    pub(crate) disposition: CodexPromptHistorySourceBackedDispositionV0,
    pub(crate) emitted_documents: u64,
    frozen: CodexPromptHistoryFrozenSnapshotV0,
    #[cfg(test)]
    pub(crate) terminal: bool,
}

#[derive(Debug, Clone)]
struct CodexPromptHistoryFrozenSnapshotV0 {
    metadata: Metadata,
    ordinary_file_token: [u8; 32],
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
    prior_prefix_digest: Option<[u8; 32]>,
    terminal: bool,
}

#[derive(Debug)]
struct RetainedPromptRecord {
    line: PromptLine,
    byte_offset: u64,
    physical_ordinal: u64,
}

#[derive(Debug)]
struct PageEmitter<'a, Emit>
where
    Emit: FnMut(CodexPromptHistorySourceBackedPageV0) -> CodexPromptHistorySourceBackedResultV0<()>,
{
    source: &'a CodexPromptHistorySourceBackedSourceV0,
    emit: Emit,
    records: Vec<CoreRecord>,
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
            records: Vec::new(),
            retained_bytes: 0,
            emitted_documents: 0,
        }
    }

    fn push(&mut self, record: CoreRecord) -> CodexPromptHistorySourceBackedResultV0<()> {
        let retained = retained_record_bytes(&record);
        if retained > PAGE_MAX_RETAINED_BYTES {
            return Err(CodexPromptHistorySourceBackedErrorV0::RecordTooLarge);
        }
        if !self.records.is_empty()
            && (self.records.len() == PAGE_MAX_DOCUMENTS
                || self.retained_bytes.saturating_add(retained) > PAGE_MAX_RETAINED_BYTES)
        {
            self.flush()?;
        }
        self.retained_bytes = self.retained_bytes.saturating_add(retained);
        self.records.push(record);
        Ok(())
    }

    fn flush(&mut self) -> CodexPromptHistorySourceBackedResultV0<()> {
        if self.records.is_empty() {
            return Ok(());
        }
        let records = std::mem::take(&mut self.records);
        let retained_bytes = std::mem::take(&mut self.retained_bytes);
        self.emitted_documents = self
            .emitted_documents
            .checked_add(
                u64::try_from(records.len())
                    .map_err(|_| CodexPromptHistorySourceBackedErrorV0::CountMismatch)?,
            )
            .ok_or(CodexPromptHistorySourceBackedErrorV0::CountMismatch)?;
        (self.emit)(CodexPromptHistorySourceBackedPageV0 {
            source: self.source.source.clone(),
            records,
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
    let (metadata, ordinary_file_token) = stable_current_ordinary_file_observation(&source.opened)?;
    scan_codex_prompt_history_source_backed_inner_v0(
        source,
        prior,
        true,
        CodexPromptHistoryFrozenSnapshotV0 {
            metadata,
            ordinary_file_token,
        },
        emit,
    )
}

pub(crate) fn plan_codex_prompt_history_source_backed_v0(
    source: CodexPromptHistorySourceBackedSourceV0,
    prior: Option<&CertifiedSource>,
) -> CodexPromptHistorySourceBackedResultV0<CodexPromptHistorySourceBackedScanV0> {
    let (metadata, ordinary_file_token) = stable_current_ordinary_file_observation(&source.opened)?;
    scan_codex_prompt_history_source_backed_inner_v0(
        source,
        prior,
        false,
        CodexPromptHistoryFrozenSnapshotV0 {
            metadata,
            ordinary_file_token,
        },
        |_| Ok(()),
    )
}

pub(crate) fn stage_planned_codex_prompt_history_source_backed_v0(
    source: CodexPromptHistorySourceBackedSourceV0,
    prior: Option<&CertifiedSource>,
    planned: &CodexPromptHistorySourceBackedScanV0,
    emit: impl FnMut(CodexPromptHistorySourceBackedPageV0) -> CodexPromptHistorySourceBackedResultV0<()>,
) -> CodexPromptHistorySourceBackedResultV0<CodexPromptHistorySourceBackedScanV0> {
    scan_codex_prompt_history_source_backed_inner_v0(
        source,
        prior,
        true,
        planned.frozen.clone(),
        emit,
    )
}

fn scan_codex_prompt_history_source_backed_inner_v0(
    source: CodexPromptHistorySourceBackedSourceV0,
    prior: Option<&CertifiedSource>,
    project_records: bool,
    frozen: CodexPromptHistoryFrozenSnapshotV0,
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

    source.opened.revalidate_same_object()?;
    if source.opened.file().metadata()?.len() < frozen.metadata.len() {
        return Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged);
    }
    let opening_metadata = &frozen.metadata;
    let opening_token = frozen.ordinary_file_token;
    if let Some(prior) = prior {
        if exact_ordinary_file_observation_matches(opening_metadata, opening_token, prior)? {
            let _checkpoint = decode_checkpoint(prior)?;
            return Ok(CodexPromptHistorySourceBackedScanV0 {
                source,
                certificate: prior.clone(),
                disposition: CodexPromptHistorySourceBackedDispositionV0::Unchanged,
                emitted_documents: 0,
                frozen,
                #[cfg(test)]
                terminal: _checkpoint.terminal,
            });
        }
    }

    let frozen_len = opening_metadata.len();
    let prior_prefix_boundary = prior
        .filter(|prior| prior.parser_revision() == PARSER_REVISION)
        .and_then(|prior| decode_checkpoint(prior).ok())
        .map(|checkpoint| checkpoint.certified_prefix_bytes);
    let analysis = walk_complete_records(
        &source.opened,
        frozen_len,
        prior_prefix_boundary,
        |_| Ok(()),
    )?;

    let observation_wire = observation_wire(
        opening_metadata,
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

    let (disposition, emit_from_byte) =
        classify_disposition(prior, &certificate, analysis.prior_prefix_digest)?;
    let emitted_documents = if !project_records
        || matches!(
            disposition,
            CodexPromptHistorySourceBackedDispositionV0::Unchanged
        ) {
        0
    } else {
        let mut pages = PageEmitter::new(&source, emit);
        let projection_analysis = walk_complete_records(
            &source.opened,
            frozen_len,
            prior_prefix_boundary,
            |record| {
                if record.byte_offset >= emit_from_byte {
                    pages.push(core_record(&source, record)?)
                } else {
                    Ok(())
                }
            },
        )?;
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
        frozen,
        #[cfg(test)]
        terminal: analysis.terminal,
    })
}

fn classify_disposition(
    prior: Option<&CertifiedSource>,
    current: &CertifiedSource,
    prior_prefix_digest: Option<[u8; 32]>,
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
                if let Some(prefix_digest) = prior_prefix_digest {
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
    prefix_boundary: Option<u64>,
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
    let mut prior_prefix_digest = prefix_boundary.filter(|boundary| *boundary == 0).map(|_| {
        let digest: [u8; 32] = Sha256::new().finalize().into();
        digest
    });
    let mut terminal = true;

    loop {
        let record_offset = offset;
        let complete_before = complete.clone();
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
                retained(&RetainedPromptRecord {
                    line,
                    byte_offset: record_offset,
                    physical_ordinal: ordinal,
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
        if prefix_boundary == Some(certified_bytes) {
            prior_prefix_digest = Some(complete.clone().finalize().into());
        }
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
        prior_prefix_digest,
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

#[cfg(test)]
mod tests;
