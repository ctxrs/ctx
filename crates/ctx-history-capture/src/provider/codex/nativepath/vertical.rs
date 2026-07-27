//! Codex NativePath Store/Core+Pro production vertical.
//!
//! Sources are scanned into certified, bounded pages and revalidated before
//! publication. Root coordination batches sources while preserving those page
//! boundaries.

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Fidelity, FileTouched, ProviderSourceTrust, Session, SessionEdge, SessionEdgeType,
    SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, EventSearchBulkGuard, JournalCheckpoint,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use super::{
    classify_source_lifecycle, revalidate_codex_source_observation, CodexAppendProof,
    CodexCatalogSource, CodexCheckpointGeneration, CodexFileObservation, CodexFileTouch,
    CodexKnownSource, CodexNativeCheckpoint, CodexNativeFrontier, CodexNativeOwnedPage,
    CodexNativePage, CodexNativeProOutputPage, CodexNativeProfile, CodexNativeScanner,
    CodexSessionRow, CodexSourceIdentity, CodexSourceLifecycle,
};
use crate::{
    native_source::NativePosition,
    provider::{
        codex::events::codex_canonical_event,
        importer::{
            avoid_provider_source_event_seq_collision, certified_provider_sync_cursor,
            provider_file_touch_import_id, provider_source_cursor_stream_for_path,
            provider_source_event_import_identity, provider_source_identity, provider_source_root,
            provider_source_session_uuid, provider_sync_metadata, timestamps,
            BoundedParserCheckpoint, CertifiedProviderCursor,
        },
        native_ingestion::{
            process_pro_replay_only, NativeIngestionPageError, NativePageAccounting,
            NativeProReplayFailure, NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
    },
    stable_capture_uuid, CaptureError, OutputNativeCursor, OutputSourceIdentity, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, Result as CaptureResult,
    CODEX_SESSION_SOURCE_FORMAT,
};

const CODEX_NATIVE_CURSOR_VERSION: u32 = 2;
const CODEX_NATIVE_CURSOR_MIN_READ_VERSION: u32 = 1;
const CODEX_NATIVE_FRONTIER_VERSION: u32 = 1;
const CODEX_NATIVE_POSITION_KIND: &str = "codex-nativepath-store-frontier-v1";
const CODEX_NATIVE_PUBLICATION_PREFIX: &str = "codex-nativepath-v1:";
const CODEX_PROVIDER: &str = "codex";
const STORE_MUTATION_OVERHEAD_UNITS: usize = 10;
const NATIVE_INGESTION_GROUP_MAX_PAGES: usize = 512;
const NATIVE_INGESTION_GROUP_MAX_SOURCES: usize = 512;
const NATIVE_INGESTION_GROUP_MAX_UNITS: usize = 4_096;
const NATIVE_INGESTION_GROUP_MAX_BYTES: usize = 8 * 1024 * 1024;

fn capture_revision() -> u32 {
    super::super::CODEX_CAPTURE_REVISION
}

fn policy_revision() -> u32 {
    super::super::CODEX_POLICY_REVISION
}

fn output_parser_revision() -> String {
    format!(
        "capture:{};policy:{}",
        capture_revision(),
        policy_revision()
    )
}

#[derive(Debug, Clone)]
pub(crate) struct CodexNativeStoreOptions {
    pub(crate) machine_id: String,
    pub(crate) imported_at: DateTime<Utc>,
    pub(crate) history_record_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub(crate) enum CodexNativeSourceAdmission {
    Live(CodexCatalogSource),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum CodexNativeLifecycleGate {
    #[error("Codex NativePath source requires more than one bounded atomic publication group")]
    SourceExceedsSinglePublicationGroup,
    #[error(
        "Codex NativePath source mutation requires generation-aware reconciliation: {lifecycle}"
    )]
    SourceMutationRequiresReconciliation { lifecycle: &'static str },
    #[error("Codex NativePath output replay requires an exact committed Core source")]
    OutputReplayRequiresCommittedCore,
    #[error("Codex NativePath output replay source no longer matches committed Core authority")]
    OutputReplaySourceChanged,
}

#[derive(Debug, Error)]
pub(crate) enum CodexNativeVerticalError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Store(#[from] ctx_history_store::StoreError),
    #[error(transparent)]
    Page(#[from] NativeIngestionPageError),
    #[error(transparent)]
    Lifecycle(#[from] CodexNativeLifecycleGate),
    #[error(transparent)]
    Output(#[from] ProOutputSinkError),
    #[error("Codex NativePath source has no valid session owner")]
    MissingOwner,
    #[error("Codex NativePath cursor is corrupt: {0}")]
    CorruptCursor(&'static str),
    #[error("Codex NativePath Core page/frontier chain is corrupt: {0}")]
    CorruptFrontier(&'static str),
    #[error("Codex NativePath checkpoint generation is exhausted")]
    CheckpointGenerationExhausted,
    #[error("Codex NativePath canonical projection journal is inactive")]
    CanonicalJournalInactive,
    #[error("Codex NativePath output progress is corrupt: {0}")]
    CorruptOutputProgress(&'static str),
    #[error("Codex NativePath output source epoch is exhausted")]
    OutputSourceEpochExhausted,
    #[error("Codex NativePath Core commit failed after source certification: {0}")]
    CoreCommit(String),
}

type VerticalResult<T> = std::result::Result<T, CodexNativeVerticalError>;

pub(crate) fn retire_codex_native_source_route(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &CodexCatalogSource,
    options: &CodexNativeStoreOptions,
    reason: ProviderSourceRouteRetirementReason,
) -> VerticalResult<bool> {
    let identity = source_projection_identity(source)?;
    let Some(current) =
        store.get_sync_cursor(None, &options.machine_id, &identity.cursor_stream)?
    else {
        return Ok(false);
    };
    let committed = match decode_native_path_committed_cursor(&current.cursor) {
        Ok(committed) => committed,
        Err(_) => return Ok(false),
    };
    let certified = CertifiedProviderCursor::decode(committed.provider_cursor())
        .map_err(|_| CodexNativeVerticalError::CorruptCursor("provider cursor is malformed"))?;
    let wire: CodexNativeStoreCursorWire = certified
        .parser_checkpoint()
        .deserialize()
        .map_err(|_| CodexNativeVerticalError::CorruptCursor("checkpoint envelope is malformed"))?;
    let owner_namespace = canonical_source_namespace(
        &source.source_root,
        &wire.checkpoint.owner.native_session_id,
    )?;
    let next = certified_provider_sync_cursor(
        CaptureProvider::Codex,
        &options.machine_id,
        identity.cursor_stream.clone(),
        &certified,
        current
            .last_synced_at
            .unwrap_or(current.timestamps.updated_at),
    )?;
    let transition = NativePathCursorTransition::new(Some(current.cursor.clone()), next);
    let accounting = NativePathGroupAccounting::new(0, 1, 0)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut publication = store.begin_native_path_publication_group(admission, accounting)?;
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Codex,
        source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
        machine_id: options.machine_id.clone(),
        locator_identity: source_locator_identity(&identity.cursor_stream, &owner_namespace),
        cursor_stream: identity.cursor_stream,
        expected_canonical_source_identity: owner_namespace,
        expected_source_revision: certified.source_revision().to_owned(),
        retired_at_ms: options.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    let classification =
        publication.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?;
    let changed = match classification {
        NativePathCursorSetClassification::AllExpected => {
            let disposition = publication.retire_provider_source_route(&retirement)?;
            publication.prepare_journal_checkpoint()?;
            publication.publish_cursor_set()?;
            matches!(
                disposition,
                ctx_history_store::ProviderSourceRouteRetirementDisposition::Retired
            )
        }
        NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
    };
    publication.commit()?;
    Ok(changed)
}

pub(crate) fn retire_replaced_codex_native_source_route(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &CodexCatalogSource,
    options: &CodexNativeStoreOptions,
) -> VerticalResult<bool> {
    let identity = source_projection_identity(source)?;
    let Some(current) =
        store.get_sync_cursor(None, &options.machine_id, &identity.cursor_stream)?
    else {
        return Ok(false);
    };
    let committed = match decode_native_path_committed_cursor(&current.cursor) {
        Ok(committed) => committed,
        Err(_) => return Ok(false),
    };
    let certified = CertifiedProviderCursor::decode(committed.provider_cursor())
        .map_err(|_| CodexNativeVerticalError::CorruptCursor("provider cursor is malformed"))?;
    let wire: CodexNativeStoreCursorWire = certified
        .parser_checkpoint()
        .deserialize()
        .map_err(|_| CodexNativeVerticalError::CorruptCursor("checkpoint envelope is malformed"))?;
    if source.catalog_native_session_id.as_deref()
        == Some(wire.checkpoint.owner.native_session_id.as_str())
    {
        return Ok(false);
    }
    retire_codex_native_source_route(
        store,
        bulk_guard,
        source,
        options,
        ProviderSourceRouteRetirementReason::Replaced,
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexNativeStoreCursorWire {
    version: u32,
    canonical_source_key: String,
    generation: u64,
    checkpoint: CodexNativeCheckpoint,
    #[serde(default)]
    retained_events: u64,
    #[serde(default)]
    skipped_events: u64,
}

#[derive(Debug, Clone)]
struct CodexCommittedSource {
    expected_store_cursor: SyncCursor,
    proof: Option<CodexAppendProof>,
    generation: u64,
    frontier: CodexNativeFrontier,
    source_revision: String,
    rejected_records: u64,
    canonical_journal_frontier: Option<JournalCheckpoint>,
    retained_events: u64,
    skipped_events: u64,
}

#[derive(Debug, Clone)]
pub(super) struct CodexPublicationContext {
    source: CodexCatalogSource,
    certified_observation: CodexFileObservation,
    options: CodexNativeStoreOptions,
    canonical_source_key: String,
    source_namespace: String,
    source_id: Uuid,
    session_id: Uuid,
    parent_session_id: Option<Uuid>,
    root_session_id: Uuid,
    cursor_stream: String,
    expected_store_cursor: Option<SyncCursor>,
    next_store_cursor: SyncCursor,
    expected_native_frontier: CodexNativeFrontier,
    source_revision: String,
    owner: CodexSessionRow,
    generation: u64,
    checkpoint: CodexNativeCheckpoint,
    rejected_records: u64,
    retained_events: u64,
    skipped_events: u64,
}

#[derive(Debug)]
pub(crate) struct CodexNativeNoop {
    pub(crate) terminal: bool,
    pub(crate) skipped_events: usize,
    pub(crate) retained_events: u64,
}

#[derive(Debug)]
pub(crate) enum CodexNativePreparedSource {
    Noop(CodexNativeNoop),
    Publication(Box<CodexNativeCorePublication>),
}

#[derive(Debug)]
pub(crate) struct CodexNativeCorePublication {
    pub(super) context: CodexPublicationContext,
    pub(super) core_pages: Vec<CodexNativePage>,
    output_pages: Vec<NativeProReplayPage>,
    pub(crate) terminal: bool,
    pub(crate) imported_events: usize,
    pub(crate) imported_edges: usize,
    pub(crate) skipped_events: usize,
    pub(crate) rejected_records: usize,
    pub(crate) retained_events: u64,
}

impl CodexNativeCorePublication {
    pub(super) fn into_root_parts(
        self,
    ) -> VerticalResult<(Vec<CodexNativeRootChunk>, Option<CodexNativeOutputReplay>)> {
        let output_replay = (!self.output_pages.is_empty()).then(|| CodexNativeOutputReplay {
            source: self.context.source.clone(),
            certified_observation: self.context.certified_observation.clone(),
            pages: self.output_pages,
        });
        let mut chunks = Vec::new();
        let mut pages = Vec::new();
        let mut units = STORE_MUTATION_OVERHEAD_UNITS;
        let mut bytes = 0_usize;
        let mut expected_cursor = self.context.expected_store_cursor.clone();
        let mut expected_frontier = self.context.expected_native_frontier.clone();
        for page in self.core_pages {
            let page_units = page.mutation_units();
            let would_overflow = !pages.is_empty()
                && (pages.len().saturating_add(1) > NATIVE_INGESTION_GROUP_MAX_PAGES
                    || units.saturating_add(page_units) > NATIVE_INGESTION_GROUP_MAX_UNITS
                    || bytes.saturating_add(page.serialized_bytes)
                        > NATIVE_INGESTION_GROUP_MAX_BYTES);
            if would_overflow {
                let next_frontier = pages
                    .last()
                    .map(|page: &CodexNativePage| page.next_safe_frontier.clone())
                    .ok_or(CodexNativeVerticalError::CorruptFrontier(
                        "bounded Codex root chunk lost its terminal page",
                    ))?;
                let next_cursor = build_context_store_cursor(&self.context, &next_frontier)?;
                chunks.push(CodexNativeRootChunk::new(
                    self.context.clone(),
                    std::mem::take(&mut pages),
                    expected_cursor.take(),
                    next_cursor.clone(),
                    expected_frontier.clone(),
                    next_frontier.clone(),
                    false,
                )?);
                expected_cursor = Some(next_cursor);
                expected_frontier = next_frontier;
                units = STORE_MUTATION_OVERHEAD_UNITS;
                bytes = 0;
            }
            units = units.saturating_add(page_units);
            bytes = bytes.saturating_add(page.serialized_bytes);
            pages.push(page);
        }
        if !pages.is_empty() {
            let next_frontier = pages
                .last()
                .map(|page| page.next_safe_frontier.clone())
                .ok_or(CodexNativeVerticalError::CorruptFrontier(
                    "Codex root publication lost its terminal page",
                ))?;
            chunks.push(CodexNativeRootChunk::new(
                self.context.clone(),
                pages,
                expected_cursor,
                self.context.next_store_cursor.clone(),
                expected_frontier,
                next_frontier,
                self.terminal,
            )?);
        }
        Ok((chunks, output_replay))
    }
}

#[derive(Debug)]
pub(super) struct CodexNativeRootChunk {
    context: CodexPublicationContext,
    pages: Vec<CodexNativePage>,
    expected_store_cursor: Option<SyncCursor>,
    next_store_cursor: SyncCursor,
    expected_frontier: CodexNativeFrontier,
    next_frontier: CodexNativeFrontier,
    terminal: bool,
    mutation_units: usize,
    serialized_bytes: usize,
}

impl CodexNativeRootChunk {
    #[allow(clippy::too_many_arguments)]
    fn new(
        context: CodexPublicationContext,
        pages: Vec<CodexNativePage>,
        expected_store_cursor: Option<SyncCursor>,
        next_store_cursor: SyncCursor,
        expected_frontier: CodexNativeFrontier,
        next_frontier: CodexNativeFrontier,
        terminal: bool,
    ) -> VerticalResult<Self> {
        validate_native_core_chain(&pages, &expected_frontier, &next_frontier, terminal)?;
        let mutation_units = pages
            .iter()
            .map(CodexNativePage::mutation_units)
            .sum::<usize>()
            .saturating_add(STORE_MUTATION_OVERHEAD_UNITS);
        let serialized_bytes = pages
            .iter()
            .map(|page| page.serialized_bytes)
            .sum::<usize>();
        if pages.is_empty()
            || pages.len() > NATIVE_INGESTION_GROUP_MAX_PAGES
            || mutation_units > NATIVE_INGESTION_GROUP_MAX_UNITS
            || serialized_bytes > NATIVE_INGESTION_GROUP_MAX_BYTES
        {
            return Err(CodexNativeLifecycleGate::SourceExceedsSinglePublicationGroup.into());
        }
        Ok(Self {
            context,
            pages,
            expected_store_cursor,
            next_store_cursor,
            expected_frontier,
            next_frontier,
            terminal,
            mutation_units,
            serialized_bytes,
        })
    }

    pub(super) const fn terminal(&self) -> bool {
        self.terminal
    }
}

#[derive(Debug, Default)]
pub(crate) struct CodexNativeRootGroup {
    chunks: Vec<CodexNativeRootChunk>,
    pages: usize,
    mutation_units: usize,
    serialized_bytes: usize,
}

impl CodexNativeRootGroup {
    pub(super) fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    pub(super) fn try_push(
        &mut self,
        chunk: CodexNativeRootChunk,
    ) -> std::result::Result<(), CodexNativeRootChunk> {
        let duplicate_source = self.chunks.iter().any(|current| {
            current.context.canonical_source_key == chunk.context.canonical_source_key
        });
        let fits = !duplicate_source
            && self.chunks.len().saturating_add(1) <= NATIVE_INGESTION_GROUP_MAX_SOURCES
            && self.pages.saturating_add(chunk.pages.len()) <= NATIVE_INGESTION_GROUP_MAX_PAGES
            && self.mutation_units.saturating_add(chunk.mutation_units)
                <= NATIVE_INGESTION_GROUP_MAX_UNITS
            && self.serialized_bytes.saturating_add(chunk.serialized_bytes)
                <= NATIVE_INGESTION_GROUP_MAX_BYTES;
        if !fits {
            return Err(chunk);
        }
        self.pages = self.pages.saturating_add(chunk.pages.len());
        self.mutation_units = self.mutation_units.saturating_add(chunk.mutation_units);
        self.serialized_bytes = self.serialized_bytes.saturating_add(chunk.serialized_bytes);
        self.chunks.push(chunk);
        Ok(())
    }

    pub(super) fn publish(
        self,
        store: &Store,
        bulk_guard: &EventSearchBulkGuard,
    ) -> VerticalResult<JournalCheckpoint> {
        if self.chunks.is_empty() {
            return Err(CodexNativeVerticalError::CorruptFrontier(
                "Codex root publication group is empty",
            ));
        }
        ensure_active_journal(store)?;
        let accounting =
            NativePathGroupAccounting::new(self.pages, self.chunks.len(), self.serialized_bytes)?;
        let admission = store.admit_event_search_bulk_group(bulk_guard)?;
        let mut publication = store.begin_native_path_publication_group(admission, accounting)?;
        let transitions = self
            .chunks
            .iter()
            .map(|chunk| {
                NativePathCursorTransition::new(
                    chunk
                        .expected_store_cursor
                        .as_ref()
                        .map(|cursor| cursor.cursor.clone()),
                    chunk.next_store_cursor.clone(),
                )
            })
            .collect::<Vec<_>>();
        let publication_id = root_publication_id(&self.chunks);
        let classification = publication.classify_cursor_set(&publication_id, &transitions)?;
        match classification {
            NativePathCursorSetClassification::AllExpected => {
                for chunk in &self.chunks {
                    write_raw_core(store, &mut publication, &chunk.context, &chunk.pages)?;
                }
                publication.prepare_journal_checkpoint()?;
                for chunk in &self.chunks {
                    revalidate_codex_source_observation(
                        &chunk.context.source,
                        &chunk.context.certified_observation,
                    )?;
                }
                publication.publish_cursor_set()?;
            }
            NativePathCursorSetClassification::AllNextSameGroup { .. } => {
                for chunk in &self.chunks {
                    revalidate_codex_source_observation(
                        &chunk.context.source,
                        &chunk.context.certified_observation,
                    )?;
                }
            }
        }
        let receipt = publication.commit()?;
        receipt
            .checkpoint()
            .cloned()
            .ok_or(CodexNativeVerticalError::CanonicalJournalInactive)
    }
}

#[derive(Debug)]
pub(crate) struct CodexNativeOutputReplay {
    source: CodexCatalogSource,
    certified_observation: CodexFileObservation,
    pages: Vec<NativeProReplayPage>,
}

impl CodexNativeOutputReplay {
    pub(crate) fn replay(
        mut self,
        output_sink: &dyn ProOutputSink,
    ) -> VerticalResult<
        Vec<
            std::result::Result<
                crate::provider::native_ingestion::NativeOutputPageReceipt,
                Box<NativeProReplayFailure>,
            >,
        >,
    > {
        revalidate_codex_source_observation(&self.source, &self.certified_observation)?;
        Ok(self
            .pages
            .drain(..)
            .map(|page| process_pro_replay_only(page, output_sink))
            .collect())
    }
}

pub(crate) fn prepare_codex_native_source(
    store: &Store,
    admission: CodexNativeSourceAdmission,
    options: CodexNativeStoreOptions,
    output_sink: Option<&dyn ProOutputSink>,
) -> VerticalResult<CodexNativePreparedSource> {
    let CodexNativeSourceAdmission::Live(source) = admission;
    ensure_active_journal(store)?;
    let identity = source_projection_identity(&source)?;
    let committed = load_committed_source(store, &source, &options, &identity)?;
    let profile = if output_sink.is_some() {
        CodexNativeProfile::CoreAndPro
    } else {
        CodexNativeProfile::CoreOnly
    };
    let resume_proof = committed.as_ref().and_then(|state| state.proof.as_ref());
    let mut scanner = match CodexNativeScanner::new(source.clone(), resume_proof, profile) {
        Ok(scanner) => scanner,
        Err(CaptureError::InvalidPayload(message))
            if resume_proof.is_some() && message.starts_with("invalid Codex append proof:") =>
        {
            CodexNativeScanner::new(source.clone(), None, profile)?
        }
        Err(error) => return Err(map_scan_error(error, committed.is_some())),
    };
    let mut core_pages = Vec::new();
    let mut output_pages = Vec::new();
    while let Some(page) = scanner
        .next_page()
        .map_err(|error| map_scan_error(error, committed.is_some()))?
    {
        match page {
            CodexNativeOwnedPage::Core(page) => core_pages.push(*page),
            CodexNativeOwnedPage::Pro(page) => output_pages.push(*page),
        }
    }
    let scan = scanner
        .finish()
        .map_err(|error| map_scan_error(error, committed.is_some()))?;
    let owner = scan
        .owner
        .clone()
        .ok_or(CodexNativeVerticalError::MissingOwner)?;
    validate_lifecycle(&scan, committed.as_ref())?;

    if scan.is_observation_replay() {
        let committed = committed.ok_or(CodexNativeVerticalError::CorruptCursor(
            "observation replay has no committed cursor",
        ))?;
        if committed.canonical_journal_frontier.is_none() {
            return Err(CodexNativeVerticalError::CorruptCursor(
                "NativePath observation replay has no journal checkpoint",
            ));
        }
        return Ok(CodexNativePreparedSource::Noop(CodexNativeNoop {
            terminal: scan.terminal(),
            skipped_events: usize::try_from(
                committed
                    .retained_events
                    .saturating_add(committed.skipped_events),
            )
            .unwrap_or(usize::MAX),
            retained_events: committed.retained_events,
        }));
    }
    if core_pages.is_empty() {
        return Err(CodexNativeVerticalError::CorruptFrontier(
            "changed source produced no Core authority page",
        ));
    }

    let checkpoint = scan
        .checkpoint()
        .ok_or(CodexNativeVerticalError::MissingOwner)?;
    let generation = committed
        .as_ref()
        .map(|state| {
            state
                .generation
                .checked_add(1)
                .ok_or(CodexNativeVerticalError::CheckpointGenerationExhausted)
        })
        .transpose()?
        .unwrap_or(0);
    let scan_rejected_records = scan
        .counters
        .malformed_records
        .saturating_add(scan.counters.oversized_records);
    let rejected_records = committed
        .as_ref()
        .map(|state| state.rejected_records)
        .unwrap_or_default()
        .saturating_add(scan_rejected_records);
    let current_revision = source_revision(&scan.full_revision_sha256);
    let scanned_retained_events = core_pages
        .iter()
        .map(|page| u64::try_from(page.core_rows.len()).unwrap_or(u64::MAX))
        .fold(0_u64, u64::saturating_add);
    let scanned_sparse_results = core_pages
        .iter()
        .flat_map(|page| &page.core_rows)
        .filter(|row| {
            matches!(
                row.provider_event.event_type,
                ctx_history_core::EventType::ToolOutput
                    | ctx_history_core::EventType::CommandOutput
            )
        })
        .count();
    let scanned_skipped_events = scan
        .counters
        .native_result_records
        .saturating_sub(u64::try_from(scanned_sparse_results).unwrap_or(u64::MAX));
    let resumed_append = scan.resume_proof().is_some();
    let retained_events = scanned_retained_events.saturating_add(
        resumed_append
            .then(|| {
                committed
                    .as_ref()
                    .map(|state| state.retained_events)
                    .unwrap_or_default()
            })
            .unwrap_or_default(),
    );
    let authority_skipped_events = scanned_skipped_events.saturating_add(
        resumed_append
            .then(|| {
                committed
                    .as_ref()
                    .map(|state| state.skipped_events)
                    .unwrap_or_default()
            })
            .unwrap_or_default(),
    );
    let resumable_partial = committed.as_ref().is_some_and(|state| {
        state.proof.is_none()
            && state.source_revision == current_revision
            && state.frontier != initial_codex_frontier()
    });
    let expected_native_frontier = if resumable_partial {
        committed
            .as_ref()
            .map(|state| state.frontier.clone())
            .unwrap_or_else(initial_codex_frontier)
    } else {
        committed
            .as_ref()
            .and_then(|state| state.proof.as_ref().map(|_| state.frontier.clone()))
            .unwrap_or_else(initial_codex_frontier)
    };
    if resumable_partial {
        let committed_frontier = &expected_native_frontier;
        let committed_index = core_pages
            .iter()
            .position(|page| &page.next_safe_frontier == committed_frontier)
            .ok_or(CodexNativeVerticalError::CorruptCursor(
                "partial Core cursor is not a provider-certified page boundary",
            ))?;
        core_pages.drain(..=committed_index);
    }
    validate_native_core_chain(
        &core_pages,
        &expected_native_frontier,
        &frontier_from_checkpoint(&checkpoint),
        scan.terminal(),
    )?;
    let imported_events = core_pages
        .iter()
        .map(|page| page.core_rows.len())
        .sum::<usize>();
    let skipped_events = scanned_skipped_events;

    let source_revision = current_revision;
    let next_store_cursor = build_next_store_cursor(
        &options,
        &identity,
        generation,
        &checkpoint,
        rejected_records,
        retained_events,
        authority_skipped_events,
    )?;
    let generic_output_pages = match output_sink {
        Some(sink) => match adapt_output_pages(
            output_pages,
            &scan,
            &identity,
            &expected_native_frontier,
            sink,
            committed.as_ref(),
            false,
        ) {
            Ok(pages) => pages,
            Err(CodexNativeVerticalError::Output(error)) => {
                sink.mark_behind(error.clone());
                Vec::new()
            }
            Err(CodexNativeVerticalError::CorruptOutputProgress(reason)) => {
                let error = ProOutputSinkError::new("invalid_progress", reason);
                sink.mark_behind(error.clone());
                Vec::new()
            }
            Err(error) => return Err(error),
        },
        None => Vec::new(),
    };
    let context = CodexPublicationContext {
        source,
        certified_observation: scan.after_observation.clone(),
        options,
        canonical_source_key: identity.canonical_source_key,
        source_namespace: identity.source_namespace,
        source_id: identity.source_id,
        session_id: identity.session_id,
        parent_session_id: identity.parent_session_id,
        root_session_id: identity.root_session_id,
        cursor_stream: identity.cursor_stream,
        expected_store_cursor: committed.map(|state| state.expected_store_cursor),
        next_store_cursor,
        expected_native_frontier,
        source_revision,
        owner,
        generation,
        checkpoint,
        rejected_records,
        retained_events,
        skipped_events: authority_skipped_events,
    };
    let imported_edges = usize::from(context.parent_session_id.is_some());
    Ok(CodexNativePreparedSource::Publication(Box::new(
        CodexNativeCorePublication {
            context,
            core_pages,
            output_pages: generic_output_pages,
            terminal: scan.terminal(),
            imported_events,
            imported_edges,
            skipped_events: usize::try_from(skipped_events).unwrap_or(usize::MAX),
            rejected_records: usize::try_from(scan_rejected_records).unwrap_or(usize::MAX),
            retained_events,
        },
    )))
}

pub(crate) fn prepare_codex_native_output_replay(
    store: &Store,
    source: CodexCatalogSource,
    options: CodexNativeStoreOptions,
    output_sink: &dyn ProOutputSink,
) -> VerticalResult<CodexNativeOutputReplay> {
    let identity = source_projection_identity(&source)?;
    let committed = load_committed_source(store, &source, &options, &identity)?
        .ok_or(CodexNativeLifecycleGate::OutputReplayRequiresCommittedCore)?;
    let mut scanner = CodexNativeScanner::new(source, None, CodexNativeProfile::CoreAndPro)?;
    let mut output_pages = Vec::new();
    while let Some(page) = scanner.next_page()? {
        match page {
            CodexNativeOwnedPage::Core(_) => {}
            CodexNativeOwnedPage::Pro(page) => {
                output_pages.push(*page);
            }
        }
    }
    let scan = scanner.finish()?;
    let checkpoint = scan
        .checkpoint()
        .ok_or(CodexNativeVerticalError::MissingOwner)?;
    let committed_proof = committed
        .proof
        .as_ref()
        .ok_or(CodexNativeLifecycleGate::OutputReplayRequiresCommittedCore)?;
    if checkpoint != committed_proof.checkpoint {
        return Err(CodexNativeLifecycleGate::OutputReplaySourceChanged.into());
    }
    let pages = adapt_output_pages(
        output_pages,
        &scan,
        &identity,
        &initial_codex_frontier(),
        output_sink,
        Some(&committed),
        true,
    )?;
    Ok(CodexNativeOutputReplay {
        source: scan.source.clone(),
        certified_observation: scan.after_observation.clone(),
        pages,
    })
}

#[derive(Debug)]
struct SourceProjectionIdentity {
    canonical_source_key: String,
    source_namespace: String,
    source_id: Uuid,
    session_id: Uuid,
    parent_session_id: Option<Uuid>,
    root_session_id: Uuid,
    cursor_stream: String,
}

fn source_projection_identity(
    source: &CodexCatalogSource,
) -> VerticalResult<SourceProjectionIdentity> {
    let raw_source_path = source.source_path.display().to_string();
    let native_session_id = source
        .catalog_native_session_id
        .as_deref()
        .ok_or(CodexNativeVerticalError::MissingOwner)?;
    let root_namespace = provider_source_identity(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        Some(&source.source_root),
        None,
        None,
        &Value::Null,
    )
    .ok_or(CodexNativeVerticalError::CorruptCursor(
        "canonical root namespace is unavailable",
    ))?;
    let source_namespace = canonical_source_namespace(&source.source_root, native_session_id)?;
    let source_id = stable_capture_uuid(&source_namespace, "codex-nativepath-capture-source");
    let session_namespace = root_namespace;
    let session_id = provider_source_session_uuid(&session_namespace, native_session_id);
    let parent_session_id = source
        .catalog_parent_native_session_id
        .as_deref()
        .map(|parent| provider_source_session_uuid(&session_namespace, parent));
    let root_session_id = source
        .catalog_root_native_session_id
        .as_deref()
        .or(source.catalog_parent_native_session_id.as_deref())
        .map(|root| provider_source_session_uuid(&session_namespace, root))
        .unwrap_or(session_id);
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        &raw_source_path,
    );
    let canonical_source_key = source_namespace.clone();
    Ok(SourceProjectionIdentity {
        canonical_source_key,
        source_namespace,
        source_id,
        session_id,
        parent_session_id,
        root_session_id,
        cursor_stream,
    })
}

fn canonical_source_namespace(
    source_root: &str,
    native_session_id: &str,
) -> VerticalResult<String> {
    let root_namespace = provider_source_identity(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        Some(source_root),
        None,
        None,
        &Value::Null,
    )
    .ok_or(CodexNativeVerticalError::CorruptCursor(
        "canonical root namespace is unavailable",
    ))?;
    Ok(format!(
        "codex-nativepath:{}",
        stable_capture_uuid(
            &format!("{root_namespace}:{native_session_id}"),
            "canonical-source"
        )
    ))
}

fn source_locator_identity(cursor_stream: &str, canonical_source_identity: &str) -> String {
    format!("{cursor_stream}#{canonical_source_identity}")
}

fn load_committed_source(
    store: &Store,
    source: &CodexCatalogSource,
    options: &CodexNativeStoreOptions,
    identity: &SourceProjectionIdentity,
) -> VerticalResult<Option<CodexCommittedSource>> {
    let Some(cursor) = store.get_sync_cursor(None, &options.machine_id, &identity.cursor_stream)?
    else {
        return Ok(None);
    };
    let committed = match decode_native_path_committed_cursor(&cursor.cursor) {
        Ok(committed) => committed,
        Err(_) => return migration_committed_source(cursor, None, None),
    };
    let canonical_journal_frontier = committed.journal_checkpoint().cloned();
    let certified = CertifiedProviderCursor::decode(committed.provider_cursor())
        .map_err(|_| CodexNativeVerticalError::CorruptCursor("provider cursor is malformed"))?;
    if certified.parser_revision() != capture_revision()
        || certified.policy_revision() != policy_revision()
        || certified.native_position().kind() != CODEX_NATIVE_POSITION_KIND
    {
        return migration_committed_source(cursor, Some(&certified), canonical_journal_frontier);
    }
    let wire: CodexNativeStoreCursorWire = certified
        .parser_checkpoint()
        .deserialize()
        .map_err(|_| CodexNativeVerticalError::CorruptCursor("checkpoint envelope is malformed"))?;
    if !(CODEX_NATIVE_CURSOR_MIN_READ_VERSION..=CODEX_NATIVE_CURSOR_VERSION).contains(&wire.version)
    {
        return Err(CodexNativeVerticalError::CorruptCursor(
            "checkpoint identity/version mismatch",
        ));
    }
    let encoded_checkpoint = wire.checkpoint.encode().map_err(CaptureError::from)?;
    let checkpoint = CodexNativeCheckpoint::decode(&encoded_checkpoint)
        .map_err(|_| CodexNativeVerticalError::CorruptCursor("checkpoint authority is invalid"))?;
    let frontier = decode_frontier(certified.native_position().value())?;
    let terminal_frontier = frontier_from_checkpoint(&checkpoint);
    if certified.source_revision() != source_revision(&checkpoint.full_revision_sha256)
        || frontier.complete_prefix_end > terminal_frontier.complete_prefix_end
        || frontier.next_raw_ordinal > terminal_frontier.next_raw_ordinal
    {
        return Err(CodexNativeVerticalError::CorruptCursor(
            "certified source revision/frontier mismatch",
        ));
    }
    let source_identity = CodexSourceIdentity::new(
        identity.canonical_source_key.clone(),
        source.source_root.clone(),
        source.source_path.clone(),
    )?;
    Ok(Some(CodexCommittedSource {
        expected_store_cursor: cursor,
        proof: (wire.version == CODEX_NATIVE_CURSOR_VERSION
            && wire.canonical_source_key == identity.canonical_source_key
            && frontier == terminal_frontier)
            .then(|| {
                CodexAppendProof::new(
                    source_identity,
                    CodexCheckpointGeneration::new(wire.generation),
                    checkpoint.clone(),
                )
            }),
        generation: wire.generation,
        frontier: if wire.version == CODEX_NATIVE_CURSOR_VERSION {
            frontier
        } else {
            initial_codex_frontier()
        },
        source_revision: certified.source_revision().to_owned(),
        rejected_records: certified.rejected_records(),
        canonical_journal_frontier,
        retained_events: wire.retained_events,
        skipped_events: wire.skipped_events,
    }))
}

fn migration_committed_source(
    cursor: SyncCursor,
    certified: Option<&CertifiedProviderCursor>,
    canonical_journal_frontier: Option<JournalCheckpoint>,
) -> VerticalResult<Option<CodexCommittedSource>> {
    Ok(Some(CodexCommittedSource {
        expected_store_cursor: cursor,
        proof: None,
        generation: 0,
        frontier: initial_codex_frontier(),
        source_revision: certified
            .map(|cursor| cursor.source_revision().to_owned())
            .unwrap_or_default(),
        rejected_records: certified
            .map(CertifiedProviderCursor::rejected_records)
            .unwrap_or_default(),
        canonical_journal_frontier,
        retained_events: 0,
        skipped_events: 0,
    }))
}

fn build_next_store_cursor(
    options: &CodexNativeStoreOptions,
    identity: &SourceProjectionIdentity,
    generation: u64,
    checkpoint: &CodexNativeCheckpoint,
    rejected_records: u64,
    retained_events: u64,
    skipped_events: u64,
) -> VerticalResult<SyncCursor> {
    let frontier = frontier_from_checkpoint(checkpoint);
    let parser_checkpoint =
        BoundedParserCheckpoint::from_serializable(&CodexNativeStoreCursorWire {
            version: CODEX_NATIVE_CURSOR_VERSION,
            canonical_source_key: identity.canonical_source_key.clone(),
            generation,
            checkpoint: checkpoint.clone(),
            retained_events,
            skipped_events,
        })?;
    let certified = CertifiedProviderCursor::new(
        source_revision(&checkpoint.full_revision_sha256),
        capture_revision(),
        policy_revision(),
        NativePosition::new(CODEX_NATIVE_POSITION_KIND, encode_frontier(&frontier)?)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        parser_checkpoint,
    )?
    .with_rejected_records(rejected_records);
    Ok(certified_provider_sync_cursor(
        CaptureProvider::Codex,
        &options.machine_id,
        identity.cursor_stream.clone(),
        &certified,
        options.imported_at,
    )?)
}

fn build_context_store_cursor(
    context: &CodexPublicationContext,
    frontier: &CodexNativeFrontier,
) -> VerticalResult<SyncCursor> {
    let parser_checkpoint =
        BoundedParserCheckpoint::from_serializable(&CodexNativeStoreCursorWire {
            version: CODEX_NATIVE_CURSOR_VERSION,
            canonical_source_key: context.canonical_source_key.clone(),
            generation: context.generation,
            checkpoint: context.checkpoint.clone(),
            retained_events: context.retained_events,
            skipped_events: context.skipped_events,
        })?;
    let certified = CertifiedProviderCursor::new(
        source_revision(&context.checkpoint.full_revision_sha256),
        capture_revision(),
        policy_revision(),
        NativePosition::new(CODEX_NATIVE_POSITION_KIND, encode_frontier(frontier)?)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        parser_checkpoint,
    )?
    .with_rejected_records(context.rejected_records);
    Ok(certified_provider_sync_cursor(
        CaptureProvider::Codex,
        &context.options.machine_id,
        context.cursor_stream.clone(),
        &certified,
        context.options.imported_at,
    )?)
}

fn adapt_output_pages(
    pages: Vec<CodexNativeProOutputPage>,
    scan: &super::CodexSourceScan,
    identity: &SourceProjectionIdentity,
    acquisition_frontier: &CodexNativeFrontier,
    sink: &dyn ProOutputSink,
    committed: Option<&CodexCommittedSource>,
    full_replay: bool,
) -> VerticalResult<Vec<NativeProReplayPage>> {
    let output_source = OutputSourceIdentity {
        provider: CODEX_PROVIDER.to_owned(),
        namespace_id: identity.cursor_stream.clone(),
        source_id: identity.canonical_source_key.clone(),
    };
    let progress = sink.observe_source(&output_source)?;
    let current_revision = source_revision(&scan.full_revision_sha256);
    let parser_revision = output_parser_revision();
    let materializer_revision = sink.materializer_revision().to_owned();
    let plan = OutputPlan::new(
        progress,
        acquisition_frontier,
        &frontier_from_scan(scan),
        &current_revision,
        &parser_revision,
        &materializer_revision,
        committed,
        full_replay,
    )?;
    if plan.noop {
        return Ok(Vec::new());
    }

    let mut raw_pages = pages;
    let final_frontier = frontier_from_scan(scan);
    let needs_terminal_progress = raw_pages
        .last()
        .is_none_or(|page| page.next_safe_frontier != final_frontier);
    if needs_terminal_progress {
        let expected_frontier = raw_pages
            .last()
            .map(|page| page.next_safe_frontier.clone())
            .unwrap_or_else(|| acquisition_frontier.clone());
        raw_pages.push(CodexNativeProOutputPage {
            identity: Default::default(),
            expected_frontier: expected_frontier.clone(),
            next_safe_frontier: final_frontier.clone(),
            outputs: Vec::new(),
            serialized_bytes: 4 * 1024,
        });
    }

    let start_index = plan.start_index(&raw_pages)?;
    let mut expected_sink_frontier = plan.expected_sink_frontier.clone();
    let mut expected_source_epoch = plan.expected_source_epoch;
    let mut disposition = plan.disposition;
    let native_source_identity =
        NativeSourceIdentity::new(CODEX_PROVIDER, identity.canonical_source_key.clone());
    let mut adapted = Vec::new();
    for page in raw_pages.into_iter().skip(start_index) {
        let expected_frontier = safe_frontier(&page.expected_frontier)?;
        let next_safe_frontier = safe_frontier(&page.next_safe_frontier)?;
        let terminal = scan.terminal() && page.next_safe_frontier == final_frontier;
        let output = crate::provider::native_ingestion::NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_source.clone(),
            source_epoch: plan.source_epoch,
            observed_revision: current_revision.clone(),
            parser_revision: parser_revision.clone(),
            materializer_revision: materializer_revision.clone(),
            disposition,
            expected_prior_source_epoch: expected_source_epoch,
            expected_prior_frontier: expected_sink_frontier.clone(),
            observations: page.outputs,
        };
        let accounting = NativePageAccounting {
            logical_units: output.observations.len(),
            conservative_serialized_bytes: page.serialized_bytes,
        };
        let adapted_page = NativeProReplayPage::new_with_source_identity(
            native_source_identity.clone(),
            expected_frontier,
            next_safe_frontier.clone(),
            terminal,
            accounting,
            output,
        )?;
        expected_source_epoch = Some(plan.source_epoch);
        expected_sink_frontier = Some(next_safe_frontier);
        disposition = ProOutputSourceDisposition::AppendOrResume;
        adapted.push(adapted_page);
    }
    Ok(adapted)
}

struct OutputPlan {
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    progress_frontier: Option<CodexNativeFrontier>,
    noop: bool,
    full_replay: bool,
}

impl OutputPlan {
    #[allow(clippy::too_many_arguments)]
    fn new(
        progress: Option<ProOutputProgress>,
        acquisition_frontier: &CodexNativeFrontier,
        final_frontier: &CodexNativeFrontier,
        current_revision: &str,
        parser_revision: &str,
        materializer_revision: &str,
        committed: Option<&CodexCommittedSource>,
        full_replay: bool,
    ) -> VerticalResult<Self> {
        let Some(progress) = progress else {
            if committed.is_some() && *acquisition_frontier != initial_codex_frontier() {
                return Err(CodexNativeVerticalError::CorruptOutputProgress(
                    "output source is absent behind a nonzero Core frontier",
                ));
            }
            return Ok(Self {
                source_epoch: 0,
                expected_source_epoch: None,
                expected_sink_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
                progress_frontier: None,
                noop: false,
                full_replay,
            });
        };
        let progress_cursor = progress
            .cursor
            .as_ref()
            .map(output_cursor_frontier)
            .transpose()?;
        let revision_rewrite = progress.parser_revision != parser_revision
            || progress.materializer_revision != materializer_revision;
        if revision_rewrite {
            if !full_replay && *acquisition_frontier != initial_codex_frontier() {
                return Err(CodexNativeVerticalError::CorruptOutputProgress(
                    "output revision rewrite requires a full output-only replay",
                ));
            }
            let source_epoch = progress
                .source_epoch
                .checked_add(1)
                .ok_or(CodexNativeVerticalError::OutputSourceEpochExhausted)?;
            return Ok(Self {
                source_epoch,
                expected_source_epoch: Some(progress.source_epoch),
                expected_sink_frontier: progress
                    .cursor
                    .as_ref()
                    .map(output_cursor_safe_frontier)
                    .transpose()?,
                disposition: ProOutputSourceDisposition::Rewrite,
                progress_frontier: None,
                noop: false,
                full_replay,
            });
        }
        let Some(progress_frontier) = progress_cursor else {
            return Err(CodexNativeVerticalError::CorruptOutputProgress(
                "existing output source has no cursor",
            ));
        };
        if progress.observed_revision == current_revision
            && progress_frontier == *final_frontier
            && progress.terminal
        {
            return Ok(Self {
                source_epoch: progress.source_epoch,
                expected_source_epoch: Some(progress.source_epoch),
                expected_sink_frontier: progress
                    .cursor
                    .as_ref()
                    .map(output_cursor_safe_frontier)
                    .transpose()?,
                disposition: ProOutputSourceDisposition::AppendOrResume,
                progress_frontier: Some(progress_frontier),
                noop: true,
                full_replay,
            });
        }
        if !full_replay && progress_frontier != *acquisition_frontier {
            return Err(CodexNativeVerticalError::CorruptOutputProgress(
                "output cursor does not match the Core acquisition frontier",
            ));
        }
        Ok(Self {
            source_epoch: progress.source_epoch,
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier: progress
                .cursor
                .as_ref()
                .map(output_cursor_safe_frontier)
                .transpose()?,
            disposition: ProOutputSourceDisposition::AppendOrResume,
            progress_frontier: Some(progress_frontier),
            noop: false,
            full_replay,
        })
    }

    fn start_index(&self, pages: &[CodexNativeProOutputPage]) -> VerticalResult<usize> {
        if !self.full_replay || self.disposition == ProOutputSourceDisposition::Rewrite {
            return Ok(0);
        }
        let Some(progress) = self.progress_frontier.as_ref() else {
            return Ok(0);
        };
        if *progress == initial_codex_frontier() {
            return Ok(0);
        }
        pages
            .iter()
            .position(|page| page.next_safe_frontier == *progress)
            .map(|index| index + 1)
            .ok_or(CodexNativeVerticalError::CorruptOutputProgress(
                "output cursor is not a certified source page boundary",
            ))
    }
}

fn write_raw_core(
    store: &Store,
    publication: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &CodexPublicationContext,
    pages: &[CodexNativePage],
) -> VerticalResult<()> {
    let raw_source_path = context.source.source_path.display().to_string();
    let locator = ProviderSourceLocatorObservation {
        provider: CaptureProvider::Codex,
        source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
        machine_id: context.options.machine_id.clone(),
        locator_identity: source_locator_identity(
            &context.cursor_stream,
            &context.source_namespace,
        ),
        cursor_stream: context.cursor_stream.clone(),
        proposed_source_identity: context.source_namespace.clone(),
        raw_source_path: Some(raw_source_path.clone()),
        source_revision: context.source_revision.clone(),
        observed_at_ms: context.options.imported_at.timestamp_millis(),
    };
    let resolution = publication.reconcile_provider_source_locator(&locator)?;
    publication.upsert_capture_source(&capture_source(
        context,
        &raw_source_path,
        &resolution.canonical_source_identity,
    ))?;
    publication
        .bind_capture_source_provider_route(context.source_id, &resolution.route_binding())?;
    let session = session(context, &raw_source_path);
    publication.upsert_session(&session)?;
    if let Some(parent_session_id) = session.parent_session_id {
        publication.upsert_projection_neutral_session_edge(
            &canonical_actor(&session),
            &parent_edge(context, &session, parent_session_id),
        )?;
    }
    for page in pages {
        for row in &page.core_rows {
            let mut identity = provider_source_event_import_identity(
                context.source_id,
                row.raw_ordinal,
                &row.normalized_body_hash,
            );
            identity = avoid_provider_source_event_seq_collision(
                store,
                identity,
                context.source_id,
                row.raw_ordinal,
                row.raw_ordinal,
            )?;
            let line_number = usize::try_from(row.raw_ordinal)
                .ok()
                .and_then(|ordinal| ordinal.checked_add(1))
                .ok_or(CodexNativeVerticalError::CorruptFrontier(
                    "provider event ordinal exceeds platform limits",
                ))?;
            let (event, command_run) = codex_canonical_event(
                &context.owner.native_session_id,
                CODEX_SESSION_SOURCE_FORMAT,
                ProviderSourceTrust::ProviderExport,
                context.options.imported_at,
                context.options.history_record_id,
                context.source_id,
                context.session_id,
                line_number,
                &row.provider_event,
                &row.normalized_body_hash,
                ProviderEventHashAuthority::NormalizedPayloadFallback,
                &identity,
            )?;
            if let Some(run) = &command_run {
                publication.upsert_run(run)?;
            }
            if !publication.reconcile_provider_event(
                &event,
                ProviderEventHashAuthority::NormalizedPayloadFallback,
            )? {
                return Err(CodexNativeVerticalError::CoreCommit(
                    "Store rejected a retained Codex Core event".to_owned(),
                ));
            }
            for file in &row.file_touches {
                if file.provider_event_index != Some(row.raw_ordinal) {
                    return Err(CodexNativeVerticalError::CorruptFrontier(
                        "file touch does not belong to its provider event",
                    ));
                }
                let touch_id = provider_file_touch_import_id(
                    store,
                    file.provider,
                    &file.provider_session_id,
                    context.source_id,
                    file.provider_event_index,
                    file.provider_touch_index,
                    false,
                )?;
                publication.upsert_file_touched(&codex_file_touched(
                    context,
                    file,
                    Some(event.id),
                    touch_id,
                ))?;
            }
        }
    }
    Ok(())
}

fn codex_file_touched(
    context: &CodexPublicationContext,
    file: &CodexFileTouch,
    event_id: Option<Uuid>,
    touch_id: Uuid,
) -> FileTouched {
    let source_root =
        provider_source_root(file.source_root.as_deref(), file.raw_source_path.as_deref());
    FileTouched {
        id: touch_id,
        history_record_id: context.options.history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: file.path.clone(),
        change_kind: file.change_kind,
        old_path: file.old_path.clone(),
        line_count_delta: file.line_count_delta,
        confidence: file.confidence,
        timestamps: timestamps(file.occurred_at),
        source_id: Some(context.source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": file.provider.as_str(),
                "provider_session_id": file.provider_session_id,
                "provider_touch_index": file.provider_touch_index,
                "provider_event_index": file.provider_event_index,
                "raw_source_path": file.raw_source_path,
                "source_id": context.source_id,
                "source_format": file.source_format,
                "source_root": source_root,
                "metadata": file.metadata,
                "session_id": context.session_id,
            }),
        ),
    }
}

fn capture_source(
    context: &CodexPublicationContext,
    raw_source_path: &str,
    canonical_source_identity: &str,
) -> CaptureSource {
    let owner = &context.owner;
    CaptureSource {
        id: context.source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Codex,
            machine_id: context.options.machine_id.clone(),
            process_id: None,
            cwd: owner.cwd.clone(),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(CODEX_SESSION_SOURCE_FORMAT.to_owned()),
            source_root: Some(context.source.source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(owner.native_session_id.clone()),
        },
        started_at: owner.started_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": owner.native_session_id,
                "source_format": CODEX_SESSION_SOURCE_FORMAT,
                "source_trust": "provider_export",
                "imported_at": context.options.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": context.source.source_root,
                "cataloged_at_ms": context.source.cataloged_at_ms,
                "catalog_observation": context.source.catalog_observation,
                "nativepath_publication": "codex-v1",
            }),
        ),
    }
}

fn session(context: &CodexPublicationContext, raw_source_path: &str) -> Session {
    let owner = &context.owner;
    let is_subagent = context.parent_session_id.is_some();
    Session {
        id: context.session_id,
        history_record_id: context.options.history_record_id,
        parent_session_id: context.parent_session_id,
        root_session_id: Some(context.root_session_id),
        capture_source_id: Some(context.source_id),
        provider: CaptureProvider::Codex,
        external_session_id: Some(owner.native_session_id.clone()),
        external_agent_id: owner.external_agent_id.clone(),
        agent_type: if is_subagent {
            AgentType::Subagent
        } else {
            AgentType::Primary
        },
        role_hint: owner
            .role_hint
            .clone()
            .or_else(|| Some(if is_subagent { "worker" } else { "primary" }.to_owned())),
        is_primary: !is_subagent,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: owner.started_at,
        ended_at: None,
        timestamps: timestamps(context.options.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": owner.native_session_id,
                "source_format": CODEX_SESSION_SOURCE_FORMAT,
                "imported_at": context.options.imported_at,
                "session_idempotency_key":
                    format!("provider-session:codex:{}", owner.native_session_id),
                "metadata": {
                    "source_format": CODEX_SESSION_SOURCE_FORMAT,
                    "source_fidelity": "codex_rollout_jsonl",
                    "raw_source_path": raw_source_path,
                    "cwd": owner.cwd,
                    "originator": owner.originator,
                    "cli_version": owner.cli_version,
                    "source": owner.source_kind,
                    "agent_nickname": owner.external_agent_id,
                    "agent_role": owner.role_hint,
                    "model_provider": owner.model_provider,
                    "import_profile": "core",
                    "lineage_resolution": "codex-nativepath-root-inventory-v1",
                },
            }),
        ),
    }
}

fn canonical_actor(session: &Session) -> CanonicalActor {
    CanonicalActor {
        direct_session_id: session.id,
        root_session_id: session.root_session_id.unwrap_or(session.id),
        parent_session_id: session.parent_session_id,
        external_session_id: session.external_session_id.clone(),
        external_agent_id: session.external_agent_id.clone(),
        agent_type: session.agent_type.as_str().to_owned(),
        role_hint: session.role_hint.clone(),
        is_primary: session.is_primary,
    }
}

fn parent_edge(
    context: &CodexPublicationContext,
    session: &Session,
    parent_session_id: Uuid,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "codex-nativepath-edge:{}:{}:{}",
                context.source_namespace, context.owner.native_session_id, session.id
            ),
            "parent_child",
        ),
        from_session_id: session.id,
        to_session_id: parent_session_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id: Some(context.source_id),
        timestamps: timestamps(context.options.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": context.owner.native_session_id,
                "parent_provider_session_id": context.owner.parent_native_session_id,
                "source_format": CODEX_SESSION_SOURCE_FORMAT,
                "imported_at": context.options.imported_at,
                "nativepath_publication": "codex-v1",
            }),
        ),
    }
}

fn validate_native_core_chain(
    pages: &[CodexNativePage],
    expected: &CodexNativeFrontier,
    next: &CodexNativeFrontier,
    terminal: bool,
) -> VerticalResult<()> {
    let mut frontier = expected;
    for (index, page) in pages.iter().enumerate() {
        let receipt = page.receipt();
        if &receipt.expected_frontier != frontier
            || receipt.committed_frontier != page.next_safe_frontier
            || receipt.accepted_core_rows != page.core_rows.len()
            || receipt.accepted_physical_records != page.physical_records
            || page.terminal != (terminal && index + 1 == pages.len())
        {
            return Err(CodexNativeVerticalError::CorruptFrontier(
                "provider-owned page receipt chain mismatch",
            ));
        }
        frontier = &page.next_safe_frontier;
    }
    if frontier != next {
        return Err(CodexNativeVerticalError::CorruptFrontier(
            "provider-owned page chain does not reach certified scan frontier",
        ));
    }
    Ok(())
}

fn validate_lifecycle(
    scan: &super::CodexSourceScan,
    committed: Option<&CodexCommittedSource>,
) -> VerticalResult<()> {
    let candidates = committed
        .and_then(|state| {
            state.proof.as_ref().map(|proof| {
                vec![CodexKnownSource {
                    proof: proof.clone(),
                    route_live: true,
                }]
            })
        })
        .unwrap_or_default();
    match classify_source_lifecycle(scan, &candidates) {
        CodexSourceLifecycle::Fresh
        | CodexSourceLifecycle::Replay { .. }
        | CodexSourceLifecycle::Append { .. }
        | CodexSourceLifecycle::Rewrite { .. }
        | CodexSourceLifecycle::Truncation { .. }
        | CodexSourceLifecycle::Replacement { .. }
        | CodexSourceLifecycle::Relocation { .. }
        | CodexSourceLifecycle::Copy { .. } => Ok(()),
        CodexSourceLifecycle::AmbiguousRelocation { .. } => Err(
            CodexNativeLifecycleGate::SourceMutationRequiresReconciliation {
                lifecycle: "ambiguous_relocation",
            }
            .into(),
        ),
    }
}

fn map_scan_error(error: CaptureError, had_committed: bool) -> CodexNativeVerticalError {
    if had_committed
        && matches!(
            &error,
            CaptureError::InvalidPayload(message)
                if message.starts_with("invalid Codex append proof:")
        )
    {
        CodexNativeLifecycleGate::SourceMutationRequiresReconciliation {
            lifecycle: "append_proof_rejected",
        }
        .into()
    } else {
        error.into()
    }
}

fn ensure_active_journal(store: &Store) -> VerticalResult<()> {
    match store.projection_journal_snapshot(None) {
        Ok(_) => Ok(()),
        Err(ctx_history_store::StoreError::ProjectionJournalInactive) => {
            store.activate_projection_journal(ctx_pro_host_protocol::PROTOCOL_FINGERPRINT)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn initial_codex_frontier() -> CodexNativeFrontier {
    CodexNativeFrontier {
        complete_prefix_end: 0,
        next_raw_ordinal: 0,
        complete_prefix_sha256: Sha256::digest([]).into(),
    }
}

fn frontier_from_checkpoint(checkpoint: &CodexNativeCheckpoint) -> CodexNativeFrontier {
    CodexNativeFrontier {
        complete_prefix_end: checkpoint.complete_prefix_end(),
        next_raw_ordinal: checkpoint.next_raw_ordinal(),
        complete_prefix_sha256: checkpoint.complete_prefix_sha256,
    }
}

fn frontier_from_scan(scan: &super::CodexSourceScan) -> CodexNativeFrontier {
    CodexNativeFrontier {
        complete_prefix_end: scan.complete_prefix_end,
        next_raw_ordinal: scan.next_raw_ordinal,
        complete_prefix_sha256: scan.complete_prefix_sha256,
    }
}

fn encode_frontier(frontier: &CodexNativeFrontier) -> CaptureResult<Vec<u8>> {
    serde_json::to_vec(frontier).map_err(CaptureError::from)
}

fn decode_frontier(bytes: &[u8]) -> VerticalResult<CodexNativeFrontier> {
    serde_json::from_slice(bytes)
        .map_err(|_| CodexNativeVerticalError::CorruptCursor("frontier payload is malformed"))
}

fn safe_frontier(frontier: &CodexNativeFrontier) -> VerticalResult<NativeSafeFrontier> {
    Ok(NativeSafeFrontier::new(
        CODEX_NATIVE_FRONTIER_VERSION,
        encode_frontier(frontier)?,
    )?)
}

fn output_cursor_frontier(cursor: &OutputNativeCursor) -> VerticalResult<CodexNativeFrontier> {
    if cursor.version != CODEX_NATIVE_FRONTIER_VERSION {
        return Err(CodexNativeVerticalError::CorruptOutputProgress(
            "output cursor version is unsupported",
        ));
    }
    decode_frontier(&cursor.payload)
        .map_err(|_| CodexNativeVerticalError::CorruptOutputProgress("output cursor is malformed"))
}

fn output_cursor_safe_frontier(cursor: &OutputNativeCursor) -> VerticalResult<NativeSafeFrontier> {
    let frontier = output_cursor_frontier(cursor)?;
    safe_frontier(&frontier)
}

fn source_revision(hash: &[u8; 32]) -> String {
    format!("sha256:{}", hex(hash))
}

fn root_publication_id(chunks: &[CodexNativeRootChunk]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx/codex-nativepath/root-group/v1\0");
    digest.update((chunks.len() as u64).to_le_bytes());
    for chunk in chunks {
        digest.update(chunk.context.canonical_source_key.as_bytes());
        digest.update(chunk.context.source_revision.as_bytes());
        digest.update(chunk.context.generation.to_le_bytes());
        digest_frontier(&mut digest, &chunk.expected_frontier);
        digest_frontier(&mut digest, &chunk.next_frontier);
        digest.update([u8::from(chunk.terminal)]);
        for page in &chunk.pages {
            digest.update(page.identity.as_bytes());
        }
    }
    format!(
        "{CODEX_NATIVE_PUBLICATION_PREFIX}root:{}",
        hex(&digest.finalize())
    )
}

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx/codex-nativepath/route-retirement/v1\0");
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(format!("{:?}", retirement.reason).as_bytes());
    format!(
        "{CODEX_NATIVE_PUBLICATION_PREFIX}retire:{}",
        hex(&digest.finalize())
    )
}

fn digest_frontier(digest: &mut Sha256, frontier: &CodexNativeFrontier) {
    digest.update(frontier.complete_prefix_end.to_le_bytes());
    digest.update(frontier.next_raw_ordinal.to_le_bytes());
    digest.update(frontier.complete_prefix_sha256);
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}
