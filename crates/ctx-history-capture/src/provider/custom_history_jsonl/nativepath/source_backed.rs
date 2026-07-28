//! Source-backed adapter for one explicitly registered Custom History JSONL file.
//!
//! The durable catalog lineage supplied by the caller is the source identity.
//! The current path is only a resolvable route. This module parses and projects
//! provider facts, but it does not publish lifecycle state or retain event
//! bodies.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceAppend,
    CertifiedSourceDeletion, CertifiedSourceInventory, ContentSourceResolver,
    CtxHistoryJsonlEventRecord, CtxHistoryJsonlFileTouchRecord, CtxHistoryJsonlRecord,
    CtxHistoryJsonlSessionRecord, CtxHistoryJsonlSourceRecord, EventHydrationRequest,
    EventIdentityInput, HydratedProviderRecord, HydrationFailure, HydrationFailureKind,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    ProjectionContractError, ScannedSourceCounts, SessionHydrationRequest, SessionIdentityInput,
    SourceAnchor, SourceFrontier, SourceInventoryObservation, SourceKey, SourceObservation,
    SourceRecordLocator, SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::{LexicalDocument, MAX_BODY_PREVIEW_CHARS};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    model::{ParsedCustomHistory, CUSTOM_ROUTE_SOURCE_FORMAT},
    projection::ordered_sessions,
    reader::parse_custom_history,
};
use crate::{
    common::io::{open_provider_source_file, OpenedProviderSourceFile},
    provider::{
        custom_history_jsonl::custom_history_internal_session_id,
        normalization::{provider_local_preview, provider_policy_event_text, provider_value_text},
    },
    CaptureError, ProviderImportSummary, MAX_PROVIDER_JSONL_LINE_BYTES,
};

const CUSTOM_SOURCE_IDENTITY_VERSION: u32 = 1;
const CUSTOM_SOURCE_SCHEMA_VARIANT: &str = "ctx-history-jsonl-v1-source-backed-v1";
const CUSTOM_SOURCE_REVISION_KIND: &str = "custom-history-ordinary-file-observation-v1";
const CUSTOM_SOURCE_BACKED_PARSER_REVISION: &str = "custom-history-jsonl-source-backed-v1";
const CUSTOM_SOURCE_FRONTIER_KIND: &str = "custom-history-jsonl-frontier-v1";
const CUSTOM_INVENTORY_AUTHORITY_NAMESPACE: &str = "custom-history.explicit-registration";
const CUSTOM_INVENTORY_REVISION_KIND: &str = "custom-history-explicit-inventory-v1";
const CUSTOM_DISCOVERY_REVISION: &str = "custom-history-explicit-only-v1";
const CUSTOM_SESSION_KEY_NAMESPACE: &str = "custom-history.session";
const CUSTOM_EVENT_KEY_NAMESPACE: &str = "custom-history.event";
const CUSTOM_LOGICAL_SESSION_KIND: &str = "custom-history-session";
const CUSTOM_LOGICAL_EVENT_KIND: &str = "custom-history-event";
const CUSTOM_CHECKPOINT_VERSION: u32 = 1;
const CUSTOM_PAGE_MAX_DOCUMENTS: usize = 64;
const CUSTOM_PAGE_MAX_RETAINED_BYTES: usize = 1024 * 1024;
const CUSTOM_DOCUMENT_METADATA_MAX_BYTES: usize = 64 * 1024;
const CUSTOM_DOCUMENT_MAX_TOUCHED_FILES: usize = 256;
const CUSTOM_MAX_HYDRATED_RECORD_BYTES: u64 = MAX_PROVIDER_JSONL_LINE_BYTES as u64 + 2;

const SOURCE_DIGEST_DOMAIN: &[u8] = b"ctx.custom-history.source-prefix.v1\0";
const INVENTORY_DIGEST_DOMAIN: &[u8] = b"ctx.custom-history.explicit-inventory.v1\0";

#[derive(Debug, Error)]
pub(crate) enum CustomHistorySourceBackedError {
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
    #[error("Custom History explicit inventory changed while it was being certified")]
    InventoryChanged,
    #[error("Custom History prior certificate belongs to another explicit registration")]
    PriorSourceMismatch,
    #[error("Custom History source-backed checkpoint is malformed or incompatible")]
    InvalidCheckpoint,
    #[error("Custom History source-backed counters overflowed or did not reconcile")]
    CountMismatch,
    #[error("Custom History source-backed locator is malformed")]
    InvalidLocator,
    #[error("Custom History resolver received conflicting routes for one source")]
    DuplicateResolverSource,
    #[error("Custom History locator source is not registered with this resolver")]
    LocatorSourceNotFound,
    #[error("Custom History locator range exceeds the bounded JSONL record size")]
    LocatorRangeTooLarge,
    #[error("Custom History locator range is no longer present")]
    LocatorRangeMissing,
    #[error("Custom History locator record digest no longer matches provider bytes")]
    LocatorDigestMismatch,
    #[error("Custom History locator no longer decodes to the indexed provider event")]
    LocatorRecordMismatch,
}

pub(crate) type CustomHistorySourceBackedResult<T> = Result<T, CustomHistorySourceBackedError>;

/// One caller-selected Custom History file plus its durable catalog lineage.
///
/// There is intentionally no default-path or directory-discovery constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CustomHistorySourceBackedInput {
    path: PathBuf,
    catalog_lineage: [u8; 32],
}

impl CustomHistorySourceBackedInput {
    pub(crate) fn explicit(path: impl Into<PathBuf>, catalog_lineage: [u8; 32]) -> Self {
        Self {
            path: path.into(),
            catalog_lineage,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn catalog_lineage(&self) -> &[u8; 32] {
        &self.catalog_lineage
    }

    pub(crate) fn source_key(&self) -> CustomHistorySourceBackedResult<SourceKey> {
        Ok(SourceKey::derive(
            CaptureProvider::Custom.as_str(),
            CUSTOM_ROUTE_SOURCE_FORMAT,
            CUSTOM_SOURCE_SCHEMA_VARIANT,
            CUSTOM_SOURCE_IDENTITY_VERSION,
            SourceAnchor::CatalogLineage(self.catalog_lineage),
        )?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CustomHistoryFileObservationWire {
    length: u64,
    modified_after_epoch: bool,
    modified_seconds: u64,
    modified_nanos: u32,
    strong_token: [u8; 32],
}

impl CustomHistoryFileObservationWire {
    fn from_opened(opened: &OpenedProviderSourceFile) -> Result<Self, CaptureError> {
        let metadata = opened.file().metadata()?;
        let (modified_after_epoch, duration) = match metadata.modified()?.duration_since(UNIX_EPOCH)
        {
            Ok(duration) => (true, duration),
            Err(error) => (false, error.duration()),
        };
        let mut token = Sha256::new();
        token.update(b"ctx.custom-history-opened-file-observation-v1\0");
        token.update(metadata.len().to_be_bytes());
        token.update([u8::from(metadata.permissions().readonly())]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            token.update(metadata.dev().to_be_bytes());
            token.update(metadata.ino().to_be_bytes());
            token.update(metadata.ctime().to_be_bytes());
            token.update(metadata.ctime_nsec().to_be_bytes());
        }
        #[cfg(not(unix))]
        {
            token.update([u8::from(modified_after_epoch)]);
            token.update(duration.as_secs().to_be_bytes());
            token.update(duration.subsec_nanos().to_be_bytes());
        }
        Ok(Self {
            length: metadata.len(),
            modified_after_epoch,
            modified_seconds: duration.as_secs(),
            modified_nanos: duration.subsec_nanos(),
            strong_token: token.finalize().into(),
        })
    }
}

#[derive(Debug, Clone)]
enum CustomHistoryInventoryState {
    Present {
        observation: CustomHistoryFileObservationWire,
        opened: Arc<OpenedProviderSourceFile>,
    },
    Missing,
}

impl PartialEq for CustomHistoryInventoryState {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Present {
                    observation: left, ..
                },
                Self::Present {
                    observation: right, ..
                },
            ) => left == right,
            (Self::Missing, Self::Missing) => true,
            _ => false,
        }
    }
}

impl Eq for CustomHistoryInventoryState {}

/// One finite observation of exactly the explicitly registered file.
#[derive(Debug, Clone)]
pub(crate) struct CustomHistorySourceBackedInventory {
    input: CustomHistorySourceBackedInput,
    source: SourceKey,
    observation: SourceInventoryObservation,
    state: CustomHistoryInventoryState,
}

impl CustomHistorySourceBackedInventory {
    pub(crate) fn input(&self) -> &CustomHistorySourceBackedInput {
        &self.input
    }

    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn observation(&self) -> &SourceInventoryObservation {
        &self.observation
    }

    pub(crate) fn is_missing(&self) -> bool {
        self.state == CustomHistoryInventoryState::Missing
    }

    pub(crate) fn certify_against(
        &self,
        closing: &Self,
    ) -> CustomHistorySourceBackedResult<CertifiedSourceInventory> {
        if self.input != closing.input
            || !self.source.exact_descriptor_eq(&closing.source)
            || self.state != closing.state
        {
            return Err(CustomHistorySourceBackedError::InventoryChanged);
        }
        let sources = match &self.state {
            CustomHistoryInventoryState::Present { .. } => vec![self.source.clone()],
            CustomHistoryInventoryState::Missing => Vec::new(),
        };
        Ok(CertifiedSourceInventory::certify(
            self.observation.clone(),
            closing.observation.clone(),
            CUSTOM_DISCOVERY_REVISION,
            sources,
        )?)
    }

    fn ordinary(&self) -> Option<&CustomHistoryFileObservationWire> {
        match &self.state {
            CustomHistoryInventoryState::Present { observation, .. } => Some(observation),
            CustomHistoryInventoryState::Missing => None,
        }
    }

    fn opened(&self) -> Option<&Arc<OpenedProviderSourceFile>> {
        match &self.state {
            CustomHistoryInventoryState::Present { opened, .. } => Some(opened),
            CustomHistoryInventoryState::Missing => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CustomHistoryReplacementReason {
    ParserRevisionChanged,
    InvalidPriorFrontier,
    Truncated,
    PrefixChanged,
    AppendInvalidatedPriorProjection,
    PriorEventMetadataChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CustomHistoryReplacementEvidence {
    pub(crate) reason: CustomHistoryReplacementReason,
    pub(crate) prior_content_digest: [u8; 32],
    pub(crate) replacement_content_digest: [u8; 32],
}

#[derive(Debug, Clone)]
pub(crate) enum CustomHistorySourceBackedDisposition {
    Cold,
    Unchanged,
    Append {
        proof: CertifiedSourceAppend,
    },
    Replacement {
        evidence: CustomHistoryReplacementEvidence,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct CustomHistorySourceBackedRoute {
    source: SourceKey,
    input: CustomHistorySourceBackedInput,
    opened: Arc<OpenedProviderSourceFile>,
}

impl CustomHistorySourceBackedRoute {
    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn path(&self) -> &Path {
        self.input.path()
    }
}

#[derive(Debug)]
pub(crate) struct CustomHistorySourceBackedPage {
    pub(crate) source: SourceKey,
    pub(crate) documents: Vec<LexicalDocument>,
    pub(crate) retained_bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct CustomHistorySourceBackedReceipt {
    pub(crate) route: CustomHistorySourceBackedRoute,
    pub(crate) inventory: CertifiedSourceInventory,
    pub(crate) certificate: CertifiedSource,
    pub(crate) disposition: CustomHistorySourceBackedDisposition,
    pub(crate) summary: ProviderImportSummary,
    pub(crate) emitted_documents: u64,
    pub(crate) terminal: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum CustomHistorySourceBackedOutcome {
    Present(CustomHistorySourceBackedReceipt),
    Missing {
        inventory: CertifiedSourceInventory,
        deletion: Option<CertifiedSourceDeletion>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CustomHistoryCheckpoint {
    version: u32,
    certified_prefix_bytes: u64,
    complete_records: u64,
    terminal: bool,
}

#[derive(Debug, Clone)]
struct CompleteLine {
    line_number: usize,
    byte_offset: u64,
    byte_length: u64,
    physical_ordinal: u64,
    record_digest: [u8; 32],
    oversized: bool,
}

#[derive(Debug)]
struct ParsedProjection {
    parsed: ParsedCustomHistory,
    lines: Vec<CompleteLine>,
    valid_sessions: BTreeSet<(String, String)>,
    event_lines: BTreeMap<(String, String, u64), usize>,
    counts: ScannedSourceCounts,
    checkpoint: CustomHistoryCheckpoint,
    content_digest: [u8; 32],
}

/// Observes exactly one explicitly supplied regular file.
pub(crate) fn observe_custom_history_source_backed_explicit(
    input: &CustomHistorySourceBackedInput,
) -> CustomHistorySourceBackedResult<CustomHistorySourceBackedInventory> {
    let source = input.source_key()?;
    let state = match open_explicit_source(input.path()) {
        Ok(opened) => {
            let observation = CustomHistoryFileObservationWire::from_opened(&opened)?;
            opened.revalidate()?;
            CustomHistoryInventoryState::Present {
                observation,
                opened,
            }
        }
        Err(CustomHistorySourceBackedError::Capture(CaptureError::Io(error)))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            CustomHistoryInventoryState::Missing
        }
        Err(error) => return Err(error),
    };
    let mut digest = Sha256::new();
    digest.update(INVENTORY_DIGEST_DOMAIN);
    match &state {
        CustomHistoryInventoryState::Present { observation, .. } => {
            digest.update(b"present\0");
            digest.update(serde_json::to_vec(observation)?);
        }
        CustomHistoryInventoryState::Missing => digest.update(b"missing\0"),
    }
    let observation = SourceInventoryObservation::new(
        CaptureProvider::Custom.as_str(),
        CUSTOM_INVENTORY_AUTHORITY_NAMESPACE,
        TypedKey::bytes(input.catalog_lineage.to_vec())?,
        CUSTOM_INVENTORY_REVISION_KIND,
        digest.finalize().to_vec(),
    )?;
    Ok(CustomHistorySourceBackedInventory {
        input: input.clone(),
        source,
        observation,
        state,
    })
}

/// Scans and projects one explicit source, returning evidence only.
///
/// The shared coordinator owns staging, append/replacement choice,
/// deletion application, and publication.
pub(crate) fn scan_custom_history_source_backed_explicit(
    input: &CustomHistorySourceBackedInput,
    prior: Option<&CertifiedSource>,
    mut emit: impl FnMut(CustomHistorySourceBackedPage) -> CustomHistorySourceBackedResult<()>,
) -> CustomHistorySourceBackedResult<CustomHistorySourceBackedOutcome> {
    let opening_inventory = observe_custom_history_source_backed_explicit(input)?;
    let source = opening_inventory.source().clone();
    if let Some(prior) = prior {
        prior.validate_contract()?;
        if !source.exact_descriptor_eq(prior.observation().source()) {
            return Err(CustomHistorySourceBackedError::PriorSourceMismatch);
        }
    }

    if opening_inventory.is_missing() {
        let closing = observe_custom_history_source_backed_explicit(input)?;
        let inventory = opening_inventory.certify_against(&closing)?;
        let deletion = prior
            .map(|prior| {
                CertifiedSourceDeletion::from_inventory(
                    prior.observation().source().clone(),
                    &inventory,
                )
            })
            .transpose()?;
        return Ok(CustomHistorySourceBackedOutcome::Missing {
            inventory,
            deletion,
        });
    }

    let opening_observation = source_observation(
        source.clone(),
        opening_inventory
            .ordinary()
            .ok_or(CustomHistorySourceBackedError::InventoryChanged)?,
    )?;
    if let Some(prior) = prior {
        if prior.parser_revision() == CUSTOM_SOURCE_BACKED_PARSER_REVISION
            && prior.observation() == &opening_observation
        {
            if let Ok(checkpoint) = decode_checkpoint(prior) {
                let closing = observe_custom_history_source_backed_explicit(input)?;
                let inventory = opening_inventory.certify_against(&closing)?;
                return Ok(CustomHistorySourceBackedOutcome::Present(
                    CustomHistorySourceBackedReceipt {
                        route: route(
                            input,
                            source,
                            Arc::clone(
                                opening_inventory
                                    .opened()
                                    .ok_or(CustomHistorySourceBackedError::InventoryChanged)?,
                            ),
                        ),
                        inventory,
                        certificate: prior.clone(),
                        disposition: CustomHistorySourceBackedDisposition::Unchanged,
                        summary: ProviderImportSummary::default(),
                        emitted_documents: 0,
                        terminal: checkpoint.terminal,
                    },
                ));
            }
        }
    }

    let opened = Arc::clone(
        opening_inventory
            .opened()
            .ok_or(CustomHistorySourceBackedError::InventoryChanged)?,
    );
    let bytes = read_explicit_source(&opened)?;
    let closing_inventory = observe_custom_history_source_backed_explicit(input)?;
    let closing_ordinary = closing_inventory
        .ordinary()
        .ok_or(CustomHistorySourceBackedError::InventoryChanged)?;
    if opening_inventory.ordinary() != Some(closing_ordinary) {
        return Err(CustomHistorySourceBackedError::InventoryChanged);
    }
    let inventory = opening_inventory.certify_against(&closing_inventory)?;
    let closing_observation = source_observation(source.clone(), closing_ordinary)?;
    let mut projection = parse_projection(&bytes)?;
    let certificate = CertifiedSource::certify_with_frontier(
        opening_observation,
        closing_observation,
        CUSTOM_SOURCE_BACKED_PARSER_REVISION,
        projection.content_digest,
        projection.counts,
        Some(SourceFrontier::new(
            CUSTOM_SOURCE_FRONTIER_KIND,
            TypedKey::bytes(serde_json::to_vec(&projection.checkpoint)?)?,
            projection.checkpoint.certified_prefix_bytes,
            projection.content_digest,
        )?),
    )?;

    let (disposition, emit_from) = classify_projection(prior, &bytes, &projection, &certificate)?;
    let emitted_documents =
        emit_projection_pages(&source, input, &projection, emit_from, &mut emit)?;
    let terminal = projection.checkpoint.terminal;
    let summary = std::mem::take(&mut projection.parsed.summary);
    Ok(CustomHistorySourceBackedOutcome::Present(
        CustomHistorySourceBackedReceipt {
            route: route(input, source, opened),
            inventory,
            certificate,
            disposition,
            summary,
            emitted_documents,
            terminal,
        },
    ))
}

pub(crate) fn revalidate_custom_history_source_backed(
    input: &CustomHistorySourceBackedInput,
    certificate: &CertifiedSource,
) -> CustomHistorySourceBackedResult<bool> {
    if certificate.parser_revision() != CUSTOM_SOURCE_BACKED_PARSER_REVISION {
        return Ok(false);
    }
    let source = input.source_key()?;
    if !source.exact_descriptor_eq(certificate.observation().source()) {
        return Ok(false);
    }
    let inventory = observe_custom_history_source_backed_explicit(input)?;
    let Some(ordinary) = inventory.ordinary() else {
        return Ok(false);
    };
    Ok(source_observation(source, ordinary)? == *certificate.observation())
}

fn route(
    input: &CustomHistorySourceBackedInput,
    source: SourceKey,
    opened: Arc<OpenedProviderSourceFile>,
) -> CustomHistorySourceBackedRoute {
    CustomHistorySourceBackedRoute {
        source,
        input: input.clone(),
        opened,
    }
}

fn open_explicit_source(
    path: &Path,
) -> CustomHistorySourceBackedResult<Arc<OpenedProviderSourceFile>> {
    let path = std::path::absolute(path)?;
    Ok(Arc::new(open_provider_source_file(&path)?))
}

fn read_explicit_source(
    source: &OpenedProviderSourceFile,
) -> CustomHistorySourceBackedResult<Vec<u8>> {
    Ok(source.read_all_bounded(usize::MAX)?)
}

fn source_observation(
    source: SourceKey,
    observation: &CustomHistoryFileObservationWire,
) -> CustomHistorySourceBackedResult<SourceObservation> {
    Ok(SourceObservation::new(
        source,
        CUSTOM_SOURCE_REVISION_KIND,
        serde_json::to_vec(observation)?,
    )?)
}

fn parse_projection(bytes: &[u8]) -> CustomHistorySourceBackedResult<ParsedProjection> {
    let (prefix_len, terminal) = complete_prefix(bytes);
    let prefix = &bytes[..prefix_len];
    let content_digest = prefix_digest(prefix);
    let source_revision = format!("custom-source-backed-sha256-v1:{content_digest:x?}");
    let mut parsed = parse_custom_history(std::io::Cursor::new(prefix), source_revision)?;
    let ordered = ordered_sessions(&parsed.sessions, &mut parsed.summary);
    let valid_sessions = ordered.into_iter().collect::<BTreeSet<_>>();
    let lines = complete_lines(prefix)?;
    let event_lines = parsed
        .events
        .iter()
        .filter(|(_, event)| {
            valid_sessions.contains(&(event.source_id.clone(), event.session_id.clone()))
        })
        .map(|(line, event)| {
            (
                (
                    event.source_id.clone(),
                    event.session_id.clone(),
                    event.event_index,
                ),
                *line,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut rejected_lines = parsed
        .summary
        .failures
        .iter()
        .filter_map(|failure| (failure.line != 0).then_some(failure.line))
        .collect::<BTreeSet<_>>();
    rejected_lines.extend(
        lines
            .iter()
            .filter_map(|line| line.oversized.then_some(line.line_number)),
    );
    for (line, event) in &parsed.events {
        if !valid_sessions.contains(&(event.source_id.clone(), event.session_id.clone())) {
            rejected_lines.insert(*line);
        }
    }

    let complete_records =
        u64::try_from(lines.len()).map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let retained_records = u64::try_from(event_lines.len())
        .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let retained_lines = event_lines.values().copied().collect::<BTreeSet<_>>();
    let rejected_records = u64::try_from(
        rejected_lines
            .iter()
            .filter(|line| **line <= lines.len() && !retained_lines.contains(*line))
            .count(),
    )
    .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let ignored_records = complete_records
        .checked_sub(retained_records)
        .and_then(|value| value.checked_sub(rejected_records))
        .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
    let certified_prefix_bytes =
        u64::try_from(prefix_len).map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let counts = ScannedSourceCounts {
        complete_records,
        retained_records,
        rejected_records,
        ignored_records,
        indexed_documents: retained_records,
        certified_bytes: certified_prefix_bytes,
    };
    Ok(ParsedProjection {
        parsed,
        lines,
        valid_sessions,
        event_lines,
        counts,
        checkpoint: CustomHistoryCheckpoint {
            version: CUSTOM_CHECKPOINT_VERSION,
            certified_prefix_bytes,
            complete_records,
            terminal,
        },
        content_digest,
    })
}

fn complete_prefix(bytes: &[u8]) -> (usize, bool) {
    if bytes.is_empty() || bytes.last() == Some(&b'\n') {
        return (bytes.len(), true);
    }
    (
        bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index.saturating_add(1)),
        false,
    )
}

fn complete_lines(prefix: &[u8]) -> CustomHistorySourceBackedResult<Vec<CompleteLine>> {
    let mut lines = Vec::new();
    let mut start = 0_usize;
    for (end, _) in prefix
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'\n')
    {
        let end = end.saturating_add(1);
        let bytes = &prefix[start..end];
        let line_number = lines.len().saturating_add(1);
        lines.push(CompleteLine {
            line_number,
            byte_offset: u64::try_from(start)
                .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?,
            byte_length: u64::try_from(bytes.len())
                .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?,
            physical_ordinal: u64::try_from(line_number.saturating_sub(1))
                .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?,
            record_digest: Sha256::digest(bytes).into(),
            oversized: bytes.len() > MAX_PROVIDER_JSONL_LINE_BYTES,
        });
        start = end;
    }
    if start != prefix.len() {
        return Err(CustomHistorySourceBackedError::CountMismatch);
    }
    Ok(lines)
}

fn prefix_digest(prefix: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(SOURCE_DIGEST_DOMAIN);
    digest.update((prefix.len() as u64).to_be_bytes());
    digest.update(prefix);
    digest.finalize().into()
}

fn classify_projection(
    prior: Option<&CertifiedSource>,
    bytes: &[u8],
    projection: &ParsedProjection,
    certificate: &CertifiedSource,
) -> CustomHistorySourceBackedResult<(CustomHistorySourceBackedDisposition, u64)> {
    let Some(prior) = prior else {
        return Ok((CustomHistorySourceBackedDisposition::Cold, 0));
    };
    let replacement = |reason| {
        (
            CustomHistorySourceBackedDisposition::Replacement {
                evidence: CustomHistoryReplacementEvidence {
                    reason,
                    prior_content_digest: *prior.content_digest(),
                    replacement_content_digest: projection.content_digest,
                },
            },
            0,
        )
    };
    if prior.parser_revision() != CUSTOM_SOURCE_BACKED_PARSER_REVISION {
        return Ok(replacement(
            CustomHistoryReplacementReason::ParserRevisionChanged,
        ));
    }
    let checkpoint = match decode_checkpoint(prior) {
        Ok(checkpoint) => checkpoint,
        Err(CustomHistorySourceBackedError::InvalidCheckpoint) => {
            return Ok(replacement(
                CustomHistoryReplacementReason::InvalidPriorFrontier,
            ))
        }
        Err(error) => return Err(error),
    };
    let prefix_len = usize::try_from(checkpoint.certified_prefix_bytes)
        .map_err(|_| CustomHistorySourceBackedError::InvalidCheckpoint)?;
    if projection.checkpoint.certified_prefix_bytes < checkpoint.certified_prefix_bytes
        || bytes.len() < prefix_len
    {
        return Ok(replacement(CustomHistoryReplacementReason::Truncated));
    }
    if prefix_digest(&bytes[..prefix_len]) != *prior.content_digest() {
        return Ok(replacement(CustomHistoryReplacementReason::PrefixChanged));
    }
    if appended_touch_changes_prior_document(projection, checkpoint.certified_prefix_bytes)? {
        return Ok(replacement(
            CustomHistoryReplacementReason::PriorEventMetadataChanged,
        ));
    }
    match CertifiedSourceAppend::certify(
        prior,
        certificate.clone(),
        checkpoint.certified_prefix_bytes,
        *prior.content_digest(),
    ) {
        Ok(proof) => Ok((
            CustomHistorySourceBackedDisposition::Append { proof },
            checkpoint.certified_prefix_bytes,
        )),
        Err(ProjectionContractError::AppendCountRegression) => Ok(replacement(
            CustomHistoryReplacementReason::AppendInvalidatedPriorProjection,
        )),
        Err(error) => Err(error.into()),
    }
}

fn appended_touch_changes_prior_document(
    projection: &ParsedProjection,
    prior_prefix_bytes: u64,
) -> CustomHistorySourceBackedResult<bool> {
    for (line_number, touch) in &projection.parsed.file_touches {
        let Some(event_index) = touch.event_index else {
            continue;
        };
        let touch_line = line_for_number(&projection.lines, *line_number)?;
        if touch_line.byte_offset < prior_prefix_bytes {
            continue;
        }
        let Some(event_line_number) = projection.event_lines.get(&(
            touch.source_id.clone(),
            touch.session_id.clone(),
            event_index,
        )) else {
            continue;
        };
        if line_for_number(&projection.lines, *event_line_number)?.byte_offset < prior_prefix_bytes
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn emit_projection_pages(
    source: &SourceKey,
    input: &CustomHistorySourceBackedInput,
    projection: &ParsedProjection,
    emit_from: u64,
    emit: &mut impl FnMut(CustomHistorySourceBackedPage) -> CustomHistorySourceBackedResult<()>,
) -> CustomHistorySourceBackedResult<u64> {
    let touches = event_touches(&projection.parsed.file_touches);
    let mut documents = Vec::new();
    let mut retained_bytes = 0_usize;
    let mut emitted = 0_u64;
    for (line_number, event) in &projection.parsed.events {
        if !projection
            .valid_sessions
            .contains(&(event.source_id.clone(), event.session_id.clone()))
        {
            continue;
        }
        let line = line_for_number(&projection.lines, *line_number)?;
        if line.byte_offset < emit_from {
            continue;
        }
        let source_record = &projection
            .parsed
            .sources
            .get(&event.source_id)
            .ok_or(CustomHistorySourceBackedError::CountMismatch)?
            .1;
        let session = &projection
            .parsed
            .sessions
            .get(&(event.source_id.clone(), event.session_id.clone()))
            .ok_or(CustomHistorySourceBackedError::CountMismatch)?
            .1;
        let document = lexical_document(
            source,
            input,
            projection,
            source_record,
            session,
            event,
            line,
            touches.get(&(
                event.source_id.clone(),
                event.session_id.clone(),
                event.event_index,
            )),
        )?;
        let document_bytes = retained_document_bytes(&document);
        if !documents.is_empty()
            && (documents.len() == CUSTOM_PAGE_MAX_DOCUMENTS
                || retained_bytes.saturating_add(document_bytes) > CUSTOM_PAGE_MAX_RETAINED_BYTES)
        {
            emit(CustomHistorySourceBackedPage {
                source: source.clone(),
                documents: std::mem::take(&mut documents),
                retained_bytes,
            })?;
            retained_bytes = 0;
        }
        retained_bytes = retained_bytes.saturating_add(document_bytes);
        documents.push(document);
        emitted = emitted
            .checked_add(1)
            .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
    }
    if !documents.is_empty() {
        emit(CustomHistorySourceBackedPage {
            source: source.clone(),
            documents,
            retained_bytes,
        })?;
    }
    Ok(emitted)
}

fn event_touches(
    touches: &[(usize, CtxHistoryJsonlFileTouchRecord)],
) -> BTreeMap<(String, String, u64), Vec<String>> {
    let mut by_event = BTreeMap::<(String, String, u64), Vec<String>>::new();
    let mut retained_bytes = BTreeMap::<(String, String, u64), usize>::new();
    for (_, touch) in touches {
        let Some(event_index) = touch.event_index else {
            continue;
        };
        let key = (
            touch.source_id.clone(),
            touch.session_id.clone(),
            event_index,
        );
        let paths = by_event.entry(key.clone()).or_default();
        let bytes = retained_bytes.entry(key).or_default();
        if paths.len() == CUSTOM_DOCUMENT_MAX_TOUCHED_FILES
            || touch.path.is_empty()
            || touch.path.len() > CUSTOM_DOCUMENT_METADATA_MAX_BYTES
            || bytes.saturating_add(touch.path.len()) > CUSTOM_DOCUMENT_METADATA_MAX_BYTES
        {
            continue;
        }
        *bytes = bytes.saturating_add(touch.path.len());
        paths.push(touch.path.clone());
    }
    by_event
}

#[allow(clippy::too_many_arguments)]
fn lexical_document(
    source: &SourceKey,
    input: &CustomHistorySourceBackedInput,
    projection: &ParsedProjection,
    source_record: &CtxHistoryJsonlSourceRecord,
    session: &CtxHistoryJsonlSessionRecord,
    event: &CtxHistoryJsonlEventRecord,
    line: &CompleteLine,
    touched_files: Option<&Vec<String>>,
) -> CustomHistorySourceBackedResult<LexicalDocument> {
    let session_id = custom_session_identity(
        source,
        &source_record.provider_key,
        &source_record.source_id,
        &session.session_id,
    )?;
    let parent_session_id = session
        .parent_session_id
        .as_deref()
        .map(|parent| {
            custom_session_identity(
                source,
                &source_record.provider_key,
                &source_record.source_id,
                parent,
            )
        })
        .transpose()?;
    let root_session_id =
        custom_root_session_identity(source, projection, source_record, session, session_id)?;
    let event_key = custom_event_typed_key(event)?;
    let native_item_key = NativeItemKey::native_id(CUSTOM_EVENT_KEY_NAMESPACE, event_key.clone())?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: CUSTOM_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: line.byte_offset,
            byte_length: line.byte_length,
            physical_ordinal: line.physical_ordinal,
            native_session_key: Some(custom_session_typed_key(
                &source_record.provider_key,
                &source_record.source_id,
                &session.session_id,
            )?),
            native_event_key: Some(event_key),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        line.record_digest,
    )?;
    let source_path = source_record
        .raw_source_path
        .as_deref()
        .or_else(|| input.path().to_str())
        .and_then(bounded_metadata);
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id,
        root_session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(custom_history_internal_session_id(
            &source_record.provider_key,
            &source_record.source_id,
            &session.session_id,
        )),
        // The v1 interchange schema has no branch or workspace field.
        branch: None,
        source_path,
        agent_type: session.agent_type.as_str().to_owned(),
        is_primary: session.is_primary,
        event_sequence: event.event_index,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body: lexical_preview(event),
        workspace: None,
        cwd: session.cwd.as_deref().and_then(bounded_metadata),
        touched_files: touched_files.cloned().unwrap_or_default(),
    })
}

fn custom_root_session_identity(
    source: &SourceKey,
    projection: &ParsedProjection,
    source_record: &CtxHistoryJsonlSourceRecord,
    session: &CtxHistoryJsonlSessionRecord,
    session_id: StableEntityId,
) -> CustomHistorySourceBackedResult<StableEntityId> {
    if let Some(root) = session.root_session_id.as_deref() {
        return custom_session_identity(
            source,
            &source_record.provider_key,
            &source_record.source_id,
            root,
        );
    }
    let mut current = session;
    for _ in 0..=projection.parsed.sessions.len() {
        let Some(parent) = current.parent_session_id.as_deref() else {
            return if current.session_id == session.session_id {
                Ok(session_id)
            } else {
                custom_session_identity(
                    source,
                    &source_record.provider_key,
                    &source_record.source_id,
                    &current.session_id,
                )
            };
        };
        current = &projection
            .parsed
            .sessions
            .get(&(source_record.source_id.clone(), parent.to_owned()))
            .ok_or(CustomHistorySourceBackedError::CountMismatch)?
            .1;
    }
    Err(CustomHistorySourceBackedError::CountMismatch)
}

fn custom_session_typed_key(
    provider_key: &str,
    source_id: &str,
    session_id: &str,
) -> CustomHistorySourceBackedResult<TypedKey> {
    Ok(TypedKey::composite(vec![
        TypedKey::utf8(provider_key)?,
        TypedKey::utf8(source_id)?,
        TypedKey::utf8(session_id)?,
    ])?)
}

fn custom_session_identity(
    source: &SourceKey,
    provider_key: &str,
    source_id: &str,
    session_id: &str,
) -> CustomHistorySourceBackedResult<StableEntityId> {
    let native_session_key = NativeSessionKey::composite(
        CUSTOM_SESSION_KEY_NAMESPACE,
        vec![
            TypedKey::utf8(provider_key)?,
            TypedKey::utf8(source_id)?,
            TypedKey::utf8(session_id)?,
        ],
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: CUSTOM_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn custom_event_typed_key(
    event: &CtxHistoryJsonlEventRecord,
) -> CustomHistorySourceBackedResult<TypedKey> {
    Ok(
        match event.event_id.as_deref().filter(|id| !id.is_empty()) {
            Some(event_id) => {
                TypedKey::composite(vec![TypedKey::utf8("event_id")?, TypedKey::utf8(event_id)?])?
            }
            None => TypedKey::composite(vec![
                TypedKey::utf8("event_index")?,
                TypedKey::U64(event.event_index),
            ])?,
        },
    )
}

fn lexical_preview(event: &CtxHistoryJsonlEventRecord) -> String {
    let candidate = event
        .preview
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            ["text", "content", "message", "summary", "command"]
                .into_iter()
                .find_map(|key| event.payload.get(key).and_then(provider_value_text))
        })
        .or_else(|| provider_value_text(&event.payload))
        .unwrap_or_default();
    let retained = provider_policy_event_text(event.event_type, &candidate, &event.payload);
    let (preview, _) = provider_local_preview(&retained.text, MAX_BODY_PREVIEW_CHARS);
    if preview.is_empty() {
        event.event_type.as_str().to_owned()
    } else {
        preview
    }
}

fn bounded_metadata(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= CUSTOM_DOCUMENT_METADATA_MAX_BYTES)
        .then(|| value.to_owned())
}

fn retained_document_bytes(document: &LexicalDocument) -> usize {
    document
        .body
        .len()
        .saturating_add(document.provider_session_id.as_ref().map_or(0, String::len))
        .saturating_add(document.source_path.as_ref().map_or(0, String::len))
        .saturating_add(document.cwd.as_ref().map_or(0, String::len))
        .saturating_add(
            document
                .touched_files
                .iter()
                .map(String::len)
                .sum::<usize>(),
        )
        .saturating_add(512)
}

fn line_for_number(
    lines: &[CompleteLine],
    line_number: usize,
) -> CustomHistorySourceBackedResult<&CompleteLine> {
    line_number
        .checked_sub(1)
        .and_then(|index| lines.get(index))
        .ok_or(CustomHistorySourceBackedError::CountMismatch)
}

fn decode_checkpoint(
    certificate: &CertifiedSource,
) -> CustomHistorySourceBackedResult<CustomHistoryCheckpoint> {
    if certificate.parser_revision() != CUSTOM_SOURCE_BACKED_PARSER_REVISION {
        return Err(CustomHistorySourceBackedError::InvalidCheckpoint);
    }
    let frontier = certificate
        .frontier()
        .ok_or(CustomHistorySourceBackedError::InvalidCheckpoint)?;
    if frontier.checkpoint_kind() != CUSTOM_SOURCE_FRONTIER_KIND {
        return Err(CustomHistorySourceBackedError::InvalidCheckpoint);
    }
    let TypedKey::Bytes(bytes) = frontier.checkpoint() else {
        return Err(CustomHistorySourceBackedError::InvalidCheckpoint);
    };
    let checkpoint: CustomHistoryCheckpoint = serde_json::from_slice(bytes)
        .map_err(|_| CustomHistorySourceBackedError::InvalidCheckpoint)?;
    if checkpoint.version != CUSTOM_CHECKPOINT_VERSION
        || checkpoint.certified_prefix_bytes != frontier.certified_prefix_bytes()
        || checkpoint.certified_prefix_bytes != certificate.counts().certified_bytes
        || checkpoint.complete_records != certificate.counts().complete_records
        || frontier.certified_prefix_digest() != certificate.content_digest()
    {
        return Err(CustomHistorySourceBackedError::InvalidCheckpoint);
    }
    Ok(checkpoint)
}

/// Invocation-local resolver for exact Custom History JSONL ranges.
#[derive(Debug)]
pub(crate) struct CustomHistorySourceBackedResolver {
    routes: HashMap<SourceKey, CustomHistorySourceBackedRoute>,
}

impl CustomHistorySourceBackedResolver {
    pub(crate) fn new(
        routes: impl IntoIterator<Item = CustomHistorySourceBackedRoute>,
    ) -> CustomHistorySourceBackedResult<Self> {
        let mut registered = HashMap::<SourceKey, CustomHistorySourceBackedRoute>::new();
        for route in routes {
            if let Some(existing) = registered.get(&route.source) {
                if !existing.source.exact_descriptor_eq(&route.source)
                    || existing.input != route.input
                {
                    return Err(CustomHistorySourceBackedError::DuplicateResolverSource);
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
    ) -> CustomHistorySourceBackedResult<&CustomHistorySourceBackedRoute> {
        request.locator().validate_contract()?;
        let route = self
            .routes
            .get(request.locator().source())
            .ok_or(CustomHistorySourceBackedError::LocatorSourceNotFound)?;
        if !route.source.exact_descriptor_eq(request.locator().source()) {
            return Err(CustomHistorySourceBackedError::InvalidLocator);
        }
        Ok(route)
    }

    fn hydrate_exact(
        &self,
        request: &EventHydrationRequest,
    ) -> CustomHistorySourceBackedResult<HydratedProviderRecord> {
        let route = self.route_for(request)?;
        hydrate_from_file(&route.opened, request)
    }
}

impl ContentSourceResolver for CustomHistorySourceBackedResolver {
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
        let route = self.route_for(first).map_err(hydration_failure)?;
        request
            .events()
            .iter()
            .map(|event| {
                let event_route = self.route_for(event).map_err(hydration_failure)?;
                if event_route.input != route.input {
                    return Err(HydrationFailure {
                        kind: HydrationFailureKind::InvalidLocator,
                        detail: "Custom History session hydration crossed explicit routes"
                            .to_owned(),
                    });
                }
                validate_session_membership(request.session_id(), event)
                    .map_err(hydration_failure)?;
                hydrate_from_file(&route.opened, event).map_err(hydration_failure)
            })
            .collect()
    }
}

fn validate_session_membership(
    requested_session_id: StableEntityId,
    event: &EventHydrationRequest,
) -> CustomHistorySourceBackedResult<()> {
    let (_, _, provider_key, source_id, session_id, _) = validate_locator(event.locator())?;
    let locator_session_id = custom_session_identity(
        event.locator().source(),
        &provider_key,
        &source_id,
        &session_id,
    )?;
    if locator_session_id != requested_session_id {
        return Err(CustomHistorySourceBackedError::InvalidLocator);
    }
    Ok(())
}

fn hydrate_from_file(
    file: &OpenedProviderSourceFile,
    request: &EventHydrationRequest,
) -> CustomHistorySourceBackedResult<HydratedProviderRecord> {
    let locator = request.locator();
    let (byte_offset, byte_length, provider_key, source_id, session_id, locator_event_key) =
        validate_locator(locator)?;
    if byte_length > CUSTOM_MAX_HYDRATED_RECORD_BYTES {
        return Err(CustomHistorySourceBackedError::LocatorRangeTooLarge);
    }
    let range_end = byte_offset
        .checked_add(byte_length)
        .ok_or(CustomHistorySourceBackedError::LocatorRangeTooLarge)?;
    if file.len() < range_end {
        return Err(CustomHistorySourceBackedError::LocatorRangeMissing);
    }
    if byte_offset != 0 {
        let boundary = file.read_exact_range(byte_offset.saturating_sub(1), 1, 1)?;
        if boundary != [b'\n'] {
            return Err(CustomHistorySourceBackedError::InvalidLocator);
        }
    }
    let length = usize::try_from(byte_length)
        .map_err(|_| CustomHistorySourceBackedError::LocatorRangeTooLarge)?;
    let provider_bytes = file.read_exact_range(
        byte_offset,
        length,
        usize::try_from(CUSTOM_MAX_HYDRATED_RECORD_BYTES)
            .map_err(|_| CustomHistorySourceBackedError::LocatorRangeTooLarge)?,
    )?;
    if !provider_bytes.ends_with(b"\n") {
        return Err(CustomHistorySourceBackedError::InvalidLocator);
    }
    if &Sha256::digest(&provider_bytes)[..] != locator.record_digest() {
        return Err(CustomHistorySourceBackedError::LocatorDigestMismatch);
    }
    let record_bytes = provider_bytes
        .strip_suffix(b"\n")
        .unwrap_or(&provider_bytes);
    let record_bytes = record_bytes.strip_suffix(b"\r").unwrap_or(record_bytes);
    let CtxHistoryJsonlRecord::Event(event) = serde_json::from_slice(record_bytes)
        .map_err(|_| CustomHistorySourceBackedError::LocatorRecordMismatch)?
    else {
        return Err(CustomHistorySourceBackedError::LocatorRecordMismatch);
    };
    if event.source_id != source_id || event.session_id != session_id {
        return Err(CustomHistorySourceBackedError::LocatorRecordMismatch);
    }
    let actual_event_key = custom_event_typed_key(&event)?;
    if actual_event_key != locator_event_key {
        return Err(CustomHistorySourceBackedError::LocatorRecordMismatch);
    }
    let stable_session_id =
        custom_session_identity(locator.source(), &provider_key, &source_id, &session_id)?;
    let native_item_key = NativeItemKey::native_id(CUSTOM_EVENT_KEY_NAMESPACE, actual_event_key)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: locator.source(),
        session_id: stable_session_id,
        logical_item_kind: CUSTOM_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    if event_id != request.event_id() {
        return Err(CustomHistorySourceBackedError::LocatorRecordMismatch);
    }
    Ok(HydratedProviderRecord {
        event_id,
        provider_bytes,
    })
}

fn validate_locator(
    locator: &SourceRecordLocator,
) -> CustomHistorySourceBackedResult<(u64, u64, String, String, String, TypedKey)> {
    if locator.source().provider() != CaptureProvider::Custom.as_str()
        || locator.source().source_format() != CUSTOM_ROUTE_SOURCE_FORMAT
        || locator.source().schema_variant() != CUSTOM_SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != CUSTOM_SOURCE_IDENTITY_VERSION
        || !matches!(locator.source().anchor(), SourceAnchor::CatalogLineage(_))
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(CustomHistorySourceBackedError::InvalidLocator);
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        native_session_key: Some(TypedKey::Composite(session_key)),
        native_event_key: Some(event_key),
        ..
    } = locator.coordinate()
    else {
        return Err(CustomHistorySourceBackedError::InvalidLocator);
    };
    let [TypedKey::Utf8(provider_key), TypedKey::Utf8(source_id), TypedKey::Utf8(session_id)] =
        session_key.as_slice()
    else {
        return Err(CustomHistorySourceBackedError::InvalidLocator);
    };
    if *byte_length == 0 || *byte_length > CUSTOM_MAX_HYDRATED_RECORD_BYTES {
        return Err(CustomHistorySourceBackedError::LocatorRangeTooLarge);
    }
    Ok((
        *byte_offset,
        *byte_length,
        provider_key.clone(),
        source_id.clone(),
        session_id.clone(),
        event_key.clone(),
    ))
}

fn hydration_failure(error: CustomHistorySourceBackedError) -> HydrationFailure {
    let kind = match &error {
        CustomHistorySourceBackedError::LocatorDigestMismatch
        | CustomHistorySourceBackedError::LocatorRecordMismatch
        | CustomHistorySourceBackedError::Capture(
            CaptureError::SourceChangedDuringCapture
            | CaptureError::InvalidProviderTranscriptPath { .. },
        ) => HydrationFailureKind::StaleRecordEvidence,
        CustomHistorySourceBackedError::LocatorRangeMissing => HydrationFailureKind::MissingRecord,
        CustomHistorySourceBackedError::InvalidLocator
        | CustomHistorySourceBackedError::Resolver(_)
        | CustomHistorySourceBackedError::LocatorRangeTooLarge
        | CustomHistorySourceBackedError::LocatorSourceNotFound
        | CustomHistorySourceBackedError::DuplicateResolverSource => {
            HydrationFailureKind::InvalidLocator
        }
        CustomHistorySourceBackedError::InventoryChanged => {
            HydrationFailureKind::StaleSourceEvidence
        }
        _ => HydrationFailureKind::TemporarilyUnavailable,
    };
    HydrationFailure {
        kind,
        detail: error.to_string(),
    }
}
