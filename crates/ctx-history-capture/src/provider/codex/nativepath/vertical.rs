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
    NativePathRetainedSourceEntities, NativePathSourceEntityFrontier, NativePathSourceEntityKind,
    NativePathSourceGenerationKey, ProviderEventHashAuthority, ProviderSourceLocatorObservation,
    ProviderSourceRouteRetirement, ProviderSourceRouteRetirementReason, Store,
    NATIVE_PATH_MAX_GROUP_PAGES, NATIVE_PATH_MAX_GROUP_SOURCES, NATIVE_PATH_MAX_MUTATION_UNITS,
    NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use super::reader::CodexRecordRejection;
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

const CODEX_NATIVE_CURSOR_VERSION: u32 = 3;
const CODEX_NATIVE_CURSOR_MIN_READ_VERSION: u32 = 1;
const CODEX_NATIVE_FRONTIER_VERSION: u32 = 1;
const CODEX_NATIVE_POSITION_KIND: &str = "codex-nativepath-store-frontier-v1";
const CODEX_NATIVE_PUBLICATION_PREFIX: &str = "codex-nativepath-v1:";
const CODEX_PROVIDER: &str = "codex";
const STORE_MUTATION_OVERHEAD_UNITS: usize = 10;
const CODEX_RETIREMENT_PAGE_UNITS: usize = 256;
const CODEX_RETIREMENT_PAGE_BYTES: usize = 4 * 1024;

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
}

impl CodexNativeVerticalError {
    pub(crate) fn requires_immediate_propagation(&self) -> bool {
        matches!(
            self,
            Self::Store(_)
                | Self::Capture(
                    CaptureError::Store(_)
                        | CaptureError::SystemIo { .. }
                        | CaptureError::SystemInvariant(_)
                        | CaptureError::WorkerPanicked(_)
                )
                | Self::Page(_)
                | Self::Lifecycle(
                    CodexNativeLifecycleGate::SourceMutationRequiresReconciliation { .. }
                )
                | Self::CorruptCursor(_)
                | Self::CorruptFrontier(_)
                | Self::CheckpointGenerationExhausted
                | Self::CanonicalJournalInactive
                | Self::CorruptOutputProgress(_)
                | Self::OutputSourceEpochExhausted
        )
    }

    pub(crate) fn into_capture_error(self) -> CaptureError {
        match self {
            Self::Capture(error) => error,
            Self::Store(error) => CaptureError::Store(error),
            _ => CaptureError::InvalidPayload(format!("Codex NativePath import failed: {self}")),
        }
    }
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
    let _receipt = publication.commit()?;
    #[cfg(codex_nativepath_qualification)]
    super::qualification::observe_store_receipt(&_receipt);
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
    certified_observation: Option<CodexFileObservation>,
    #[serde(default)]
    phase: CodexNativeCursorPhase,
    #[serde(default)]
    retained_events: u64,
    #[serde(default)]
    skipped_events: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CodexNativeCursorPhase {
    Core,
    Rebuilding,
    Retiring {
        after: Option<CodexRetirementFrontierWire>,
    },
    #[default]
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexRetirementFrontierWire {
    kind: String,
    id: Uuid,
}

impl CodexRetirementFrontierWire {
    fn from_store(frontier: NativePathSourceEntityFrontier) -> Self {
        Self {
            kind: frontier.kind.as_str().to_owned(),
            id: frontier.id,
        }
    }

    fn to_store(&self) -> VerticalResult<NativePathSourceEntityFrontier> {
        let kind = match self.kind.as_str() {
            "session" => NativePathSourceEntityKind::Session,
            "session_edge" => NativePathSourceEntityKind::SessionEdge,
            "run" => NativePathSourceEntityKind::Run,
            "event" => NativePathSourceEntityKind::Event,
            "file_touch" => NativePathSourceEntityKind::FileTouch,
            _ => {
                return Err(CodexNativeVerticalError::CorruptCursor(
                    "retirement frontier kind is invalid",
                ));
            }
        };
        Ok(NativePathSourceEntityFrontier { kind, id: self.id })
    }
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
    certified_observation: Option<CodexFileObservation>,
    phase: CodexNativeCursorPhase,
}

#[derive(Debug)]
pub(crate) struct CodexNativeProducerTask {
    source: CodexCatalogSource,
    options: CodexNativeStoreOptions,
    identity: SourceProjectionIdentity,
    committed: Option<CodexCommittedSource>,
}

#[derive(Debug)]
pub(crate) struct CodexNativeWindowProducer {
    source: CodexCatalogSource,
    options: CodexNativeStoreOptions,
    identity: SourceProjectionIdentity,
    committed: Option<CodexCommittedSource>,
    scanner: Option<CodexNativeScanner>,
    generation: u64,
    source_revision: String,
    expected_store_cursor: Option<SyncCursor>,
    expected_frontier: CodexNativeFrontier,
    base_retained_events: u64,
    base_skipped_events: u64,
    base_rejected_records: u64,
    imported_events: usize,
    scanned_sparse_results: u64,
    published_window: bool,
    published_core: bool,
    stage_generation: bool,
    pending_step: Option<CodexNativeProducerStep>,
    prevalidated_noop: Option<CodexNativeNoop>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CodexNativeCommittedDelta {
    pub(crate) imported_sessions: usize,
    pub(crate) imported_events: usize,
    pub(crate) imported_edges: usize,
}

#[derive(Debug)]
pub(crate) struct CodexNativeTerminalReport {
    pub(crate) skipped_events: usize,
    pub(crate) rejected_records: usize,
    pub(crate) rejections: Vec<CodexRecordRejection>,
    pub(crate) retained_events: u64,
    pub(crate) terminal: bool,
}

#[derive(Debug)]
// A window is already bounded and transferred by ownership through the hot
// producer path. Boxing it would add an allocation to every published page.
#[allow(clippy::large_enum_variant)]
pub(crate) enum CodexNativeProducerStep {
    Window {
        chunk: CodexNativeRootChunk,
        source_done: bool,
        delta: CodexNativeCommittedDelta,
        report: Option<CodexNativeTerminalReport>,
    },
    Noop(CodexNativeNoop),
}

#[derive(Debug, Clone)]
pub(super) struct CodexPublicationContext {
    source: CodexCatalogSource,
    certified_observation: CodexFileObservation,
    options: CodexNativeStoreOptions,
    canonical_source_key: String,
    proposed_source_namespace: String,
    root_namespace: String,
    parent_native_session_id: Option<String>,
    root_native_session_id: Option<String>,
    cursor_stream: String,
    source_revision: String,
    owner: CodexSessionRow,
    generation: u64,
    checkpoint: CodexNativeCheckpoint,
    rejected_records: u64,
    retained_events: u64,
    skipped_events: u64,
    stage_generation: bool,
}

#[derive(Debug)]
pub(crate) struct CodexNativeNoop {
    pub(crate) terminal: bool,
    pub(crate) skipped_events: usize,
    pub(crate) rejected_records: usize,
    pub(crate) rejections: Vec<CodexRecordRejection>,
    pub(crate) retained_events: u64,
    pub(crate) committed_authority: bool,
}

#[derive(Debug)]
pub(crate) struct CodexNativeRootChunk {
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
        let page_mutation_units = pages
            .iter()
            .map(CodexNativePage::mutation_units)
            .sum::<usize>();
        // A rebuilding generation writes every retained entity once to its
        // canonical table and once to the durable generation staging ledger.
        // The fixed allowance covers source/session/route/generation/cursor
        // controls for both data-bearing and terminal cursor-only chunks.
        let mutation_units = if context.stage_generation {
            page_mutation_units
                .saturating_mul(2)
                .saturating_add(STORE_MUTATION_OVERHEAD_UNITS)
        } else {
            page_mutation_units.saturating_add(STORE_MUTATION_OVERHEAD_UNITS)
        };
        let serialized_bytes = pages
            .iter()
            .map(|page| page.serialized_bytes)
            .sum::<usize>();
        if pages.is_empty()
            || pages.len() > NATIVE_PATH_MAX_GROUP_PAGES
            || mutation_units > NATIVE_PATH_MAX_MUTATION_UNITS
            || serialized_bytes > NATIVE_PATH_MAX_RETAINED_PAGE_BYTES
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

    pub(super) fn detach_parent_lineage(&mut self) {
        self.context.parent_native_session_id = None;
        self.context.root_native_session_id = None;
    }

    pub(super) fn bind_exact_expected_cursor(
        &mut self,
        publication: &CodexNativeRootPublication,
    ) -> VerticalResult<()> {
        let expected = self.expected_store_cursor.as_mut().ok_or(
            CodexNativeVerticalError::CorruptFrontier(
                "bounded Codex chunk lost its expected cursor",
            ),
        )?;
        let Some(current) = exact_cursor_for_key(&publication.published_cursors, expected) else {
            return Err(CodexNativeVerticalError::CorruptFrontier(
                "prior Codex publication did not return one exact cursor",
            ));
        };
        let committed = decode_native_path_committed_cursor(&current.cursor).map_err(|_| {
            CodexNativeVerticalError::CorruptCursor(
                "prior Codex publication returned a malformed cursor envelope",
            )
        })?;
        let expected_provider_cursor = decode_native_path_committed_cursor(&expected.cursor)
            .map_or_else(
                |_| expected.cursor.clone(),
                |cursor| cursor.provider_cursor().to_owned(),
            );
        if committed.provider_cursor() != expected_provider_cursor {
            return Err(CodexNativeVerticalError::CorruptFrontier(
                "prior Codex publication does not match the next certified frontier",
            ));
        }
        expected.cursor.clone_from(&current.cursor);
        Ok(())
    }

    pub(super) fn split_at_page_boundary(mut self) -> VerticalResult<Option<(Self, Self)>> {
        if self.pages.len() < 2 {
            return Ok(None);
        }
        let right_pages = self.pages.split_off(self.pages.len() / 2);
        let split_frontier = self
            .pages
            .last()
            .map(|page| page.next_safe_frontier.clone())
            .ok_or(CodexNativeVerticalError::CorruptFrontier(
                "Codex journal split lost its certified page boundary",
            ))?;
        let split_cursor = build_context_store_cursor(
            &self.context,
            &split_frontier,
            CodexNativeCursorPhase::Core,
        )?;
        let left = Self::new(
            self.context.clone(),
            self.pages,
            self.expected_store_cursor,
            split_cursor.clone(),
            self.expected_frontier,
            split_frontier.clone(),
            false,
        )?;
        let right = Self::new(
            self.context,
            right_pages,
            Some(split_cursor),
            self.next_store_cursor,
            split_frontier,
            self.next_frontier,
            self.terminal,
        )?;
        Ok(Some((left, right)))
    }
}

#[derive(Debug, Clone, Copy)]
struct CodexNativeRootChunkAccounting {
    pages: usize,
    mutation_units: usize,
    serialized_bytes: usize,
}

impl From<&CodexNativeRootChunk> for CodexNativeRootChunkAccounting {
    fn from(chunk: &CodexNativeRootChunk) -> Self {
        Self {
            pages: chunk.pages.len(),
            mutation_units: chunk.mutation_units,
            serialized_bytes: chunk.serialized_bytes,
        }
    }
}

#[derive(Debug, Default)]
struct CodexNativeRootGroupAccounting {
    chunks: usize,
    pages: usize,
    mutation_units: usize,
    serialized_bytes: usize,
}

impl CodexNativeRootGroupAccounting {
    fn try_push(&mut self, chunk: CodexNativeRootChunkAccounting) -> bool {
        let fits = self.chunks.saturating_add(1) <= NATIVE_PATH_MAX_GROUP_SOURCES
            && self.pages.saturating_add(chunk.pages) <= NATIVE_PATH_MAX_GROUP_PAGES
            && self.mutation_units.saturating_add(chunk.mutation_units)
                <= NATIVE_PATH_MAX_MUTATION_UNITS
            && self.serialized_bytes.saturating_add(chunk.serialized_bytes)
                <= NATIVE_PATH_MAX_RETAINED_PAGE_BYTES;
        if !fits {
            return false;
        }
        self.chunks = self.chunks.saturating_add(1);
        self.pages = self.pages.saturating_add(chunk.pages);
        self.mutation_units = self.mutation_units.saturating_add(chunk.mutation_units);
        self.serialized_bytes = self.serialized_bytes.saturating_add(chunk.serialized_bytes);
        true
    }
}

#[derive(Debug, Default)]
pub(crate) struct CodexNativeRootGroup {
    chunks: Vec<CodexNativeRootChunk>,
    accounting: CodexNativeRootGroupAccounting,
}

impl CodexNativeRootGroup {
    pub(super) fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    // A rejected chunk is intentionally returned intact for the caller's next group.
    #[allow(clippy::result_large_err)]
    pub(super) fn try_push(
        &mut self,
        chunk: CodexNativeRootChunk,
    ) -> std::result::Result<(), CodexNativeRootChunk> {
        let duplicate_source = self.chunks.iter().any(|current| {
            current.context.canonical_source_key == chunk.context.canonical_source_key
        });
        if duplicate_source
            || !self
                .accounting
                .try_push(CodexNativeRootChunkAccounting::from(&chunk))
        {
            return Err(chunk);
        }
        self.chunks.push(chunk);
        Ok(())
    }

    pub(super) fn publish(
        self,
        store: &Store,
        bulk_guard: &EventSearchBulkGuard,
    ) -> VerticalResult<CodexNativeRootPublication> {
        if self.chunks.is_empty() {
            return Err(CodexNativeVerticalError::CorruptFrontier(
                "Codex root publication group is empty",
            ));
        }
        ensure_active_journal(store)?;
        publish_root_group_bounded(store, bulk_guard, self.chunks)
    }
}

mod cursor;
mod grouping;
mod producer;
mod projection;
pub(super) mod publication;
mod replay;

use cursor::*;
pub(crate) use producer::{
    finish_pending_codex_native_retirement, prepare_codex_native_producer_task,
};
use projection::*;
use publication::*;
pub(crate) use replay::{prepare_codex_native_output_replay, CodexNativeOutputReplay};

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
    if store.native_cold_load_active() {
        return Ok(());
    }
    match store.projection_journal_checkpoint() {
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

fn source_observation_revision(observation: &CodexFileObservation) -> String {
    format!(
        "codex-file-observation-v1:{}:{}:{}",
        observation.len,
        observation.modified_at_ms,
        hex(&observation.change_token),
    )
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

fn generation_retirement_publication_id(
    key: &NativePathSourceGenerationKey,
    after: Option<&CodexRetirementFrontierWire>,
    next_after: Option<&CodexRetirementFrontierWire>,
    done: bool,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx/codex-nativepath/generation-retirement/v1\0");
    digest.update(key.provider.as_str().as_bytes());
    digest.update(key.source_format.as_bytes());
    digest.update(key.machine_id.as_bytes());
    digest.update(key.canonical_source_identity.as_bytes());
    digest.update(key.locator_identity.as_bytes());
    digest.update(key.cursor_stream.as_bytes());
    digest.update(key.source_revision.as_bytes());
    digest.update(key.generation_id.as_bytes());
    for frontier in [after, next_after] {
        match frontier {
            Some(frontier) => {
                digest.update([1]);
                digest.update(frontier.kind.as_bytes());
                digest.update(frontier.id.as_bytes());
            }
            None => digest.update([0]),
        }
    }
    digest.update([u8::from(done)]);
    format!(
        "{CODEX_NATIVE_PUBLICATION_PREFIX}retire:{}",
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
