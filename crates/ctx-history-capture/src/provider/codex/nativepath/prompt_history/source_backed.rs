//! Source-backed projection for Codex's ordinary `history.jsonl` prompt log.
//!
//! The catalog lineage supplied by discovery is durable identity. The path is
//! only a route used to acquire one retained ordinary-file capability. Parsing,
//! certification, direct Core publication, and final route checks all use that
//! capability rather than reopening a canonicalized pathname.

use std::{
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    CertifiedSourceAppend, CoreRecord, CoreRecordError, EventIdentityInput, NativeItemKey,
    NativeSessionKey, PositionStability, ProjectionContractError, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceFrontier, SourceKey, SourceObservation,
    StableEntityId, TypedKey,
};
use ctx_history_index::BaseEventIdentityLookup;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::PromptLine;
#[cfg(test)]
use crate::common::io::open_provider_source_file;
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::source_backed::{
        family::jsonl::{
            JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyBaseScope, JsonlFamilyInventory,
            JsonlFamilyInventoryMode, JsonlFamilyLeaf, JsonlFamilyOptimizedLeafOutcome,
            JsonlFamilyProjector, JsonlFamilyPublication, JsonlFamilyRootMissingMode,
            JsonlFamilyTerminalProof, JsonlFamilyWorkerContext,
        },
        SourceBackedRouteErrorKind,
    },
    CaptureError, MAX_PROVIDER_JSONL_LINE_BYTES,
};

mod path;
mod projection;
mod snapshot;
use path::absolute_lexical_path;
use projection::{core_record, retained_record_bytes};
use snapshot::{
    decode_checkpoint, exact_ordinary_file_observation_matches, observation_wire,
    opened_file_from_start, stable_current_ordinary_file_observation, terminal_prefix,
    verify_frozen_prefix, CheckpointV0, CodexPromptHistoryFrozenSnapshotV0,
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub(crate) certificate: CertifiedSource,
    pub(crate) disposition: CodexPromptHistorySourceBackedDispositionV0,
    pub(crate) emitted_documents: u64,
    terminal_prefix_bytes: u64,
    terminal_prefix_sha256: [u8; 32],
    #[cfg(test)]
    pub(crate) terminal: bool,
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
#[cfg(test)]
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

#[derive(Default)]
struct CodexPromptHistoryJsonlFamilyStateV0 {
    discovered_source: Option<CodexPromptHistorySourceBackedSourceV0>,
    #[cfg(test)]
    after_scan_hook: Option<Box<dyn FnOnce() + Send>>,
    #[cfg(test)]
    after_family_source_open_hook: Option<Box<dyn FnOnce() + Send>>,
}

/// Shared-family adapter for Codex's single prompt-history JSONL file. The
/// family owns inventory, publication, deletion, and commit scheduling while
/// the optimized leaf callback retains the native bounded prompt scanner.
#[derive(Clone)]
pub(crate) struct CodexPromptHistoryJsonlFamilyAdapterV0 {
    input: CodexPromptHistorySourceBackedInputV0,
    route_path: PathBuf,
    state: Arc<Mutex<CodexPromptHistoryJsonlFamilyStateV0>>,
}

impl CodexPromptHistoryJsonlFamilyAdapterV0 {
    pub(crate) fn new(
        mut input: CodexPromptHistorySourceBackedInputV0,
    ) -> CodexPromptHistorySourceBackedResultV0<Self> {
        let route_path = absolute_lexical_path(input.path())?;
        input.path = route_path.clone();
        Ok(Self {
            input,
            route_path,
            state: Arc::new(Mutex::new(CodexPromptHistoryJsonlFamilyStateV0::default())),
        })
    }

    pub(crate) fn route_path(&self) -> &Path {
        &self.route_path
    }

    #[cfg(test)]
    fn set_after_scan_hook(&self, hook: impl FnOnce() + Send + 'static) {
        let mut state = self.state.lock().expect("prompt-history state lock");
        assert!(
            state.after_scan_hook.is_none(),
            "prompt-history after-scan hook is already installed"
        );
        state.after_scan_hook = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn set_after_family_source_open_hook(&self, hook: impl FnOnce() + Send + 'static) {
        let mut state = self.state.lock().expect("prompt-history state lock");
        assert!(
            state.after_family_source_open_hook.is_none(),
            "prompt-history after-source-open hook is already installed"
        );
        state.after_family_source_open_hook = Some(Box::new(hook));
    }

    fn discover_family(&self, route_path: &Path) -> crate::Result<JsonlFamilyInventory> {
        if route_path != self.route_path {
            return Err(CaptureError::InvalidPayload(
                "Codex prompt-history JSONL route path changed".to_owned(),
            ));
        }
        let parent = route_path.parent().ok_or_else(|| {
            CaptureError::InvalidPayload("Codex prompt-history JSONL path has no parent".to_owned())
        })?;
        let authority_path = route_path.file_name().map(PathBuf::from).ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Codex prompt-history JSONL path has no filename".to_owned(),
            )
        })?;
        let retained = (|| -> crate::Result<_> {
            let authority = Arc::new(ProviderSourceRoot::open(parent)?);
            let opened = Arc::new(authority.open_file(&authority_path)?);
            Ok((authority, opened))
        })();
        let (authority, opened) = match retained {
            Ok(retained) => retained,
            Err(error) if capture_error_is_not_found(&error) => {
                self.state
                    .lock()
                    .map_err(|_| prompt_family_state_error())?
                    .discovered_source = None;
                return JsonlFamilyInventory::missing(CaptureProvider::Codex, route_path);
            }
            Err(error) => return Err(error),
        };
        #[cfg(test)]
        if let Some(hook) = self
            .state
            .lock()
            .map_err(|_| prompt_family_state_error())?
            .after_family_source_open_hook
            .take()
        {
            hook();
        }
        let source_key = self
            .input
            .source_key()
            .map_err(prompt_family_capture_error)?;
        let binding = TypedKey::bytes(source_key.exact_descriptor_digest().to_vec())
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let leaf = JsonlFamilyLeaf::bind_opened(
            source_key.clone(),
            route_path.to_path_buf(),
            Arc::clone(&authority),
            authority_path,
            binding,
            &opened,
        )?;
        let source = CodexPromptHistorySourceBackedSourceV0 {
            source: source_key,
            opened,
        };
        self.state
            .lock()
            .map_err(|_| prompt_family_state_error())?
            .discovered_source = Some(source);
        JsonlFamilyInventory::present(CaptureProvider::Codex, route_path, authority, vec![leaf])
    }

    fn scan_leaf(
        &self,
        leaf: &JsonlFamilyLeaf,
        base: Option<&CertifiedSource>,
        emit_page: &mut dyn FnMut(JsonlFamilyPublication, Vec<CoreRecord>) -> crate::Result<()>,
    ) -> crate::Result<JsonlFamilyOptimizedLeafOutcome> {
        let source = self
            .state
            .lock()
            .map_err(|_| prompt_family_state_error())?
            .discovered_source
            .clone()
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Codex prompt-history JSONL leaf has no retained source".to_owned(),
                )
            })?;
        if !source.source().exact_descriptor_eq(leaf.source()) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let scan =
            scan_codex_prompt_history_jsonl_family_leaf_v0(source, base, |disposition, page| {
                if !page.source.exact_descriptor_eq(leaf.source()) {
                    return Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged);
                }
                let _retained_page_bytes = page.retained_bytes;
                let publication = match disposition {
                    CodexPromptHistorySourceBackedDispositionV0::Append => {
                        JsonlFamilyPublication::Append
                    }
                    CodexPromptHistorySourceBackedDispositionV0::Cold
                    | CodexPromptHistorySourceBackedDispositionV0::Replacement => {
                        JsonlFamilyPublication::Replace
                    }
                    CodexPromptHistorySourceBackedDispositionV0::Unchanged => {
                        return Err(CodexPromptHistorySourceBackedErrorV0::CountMismatch);
                    }
                };
                emit_page(publication, page.records)
                    .map_err(CodexPromptHistorySourceBackedErrorV0::Capture)
            })
            .map_err(prompt_family_capture_error)?;
        validate_prompt_family_scan_counts(&scan, base)?;
        let terminal_proof = JsonlFamilyTerminalProof::frozen_prefix(
            self,
            leaf,
            &scan.certificate,
            scan.terminal_prefix_bytes,
            scan.terminal_prefix_sha256,
        )?;
        let outcome = match scan.disposition {
            CodexPromptHistorySourceBackedDispositionV0::Cold
            | CodexPromptHistorySourceBackedDispositionV0::Replacement => {
                JsonlFamilyOptimizedLeafOutcome::replacement(
                    scan.certificate.clone(),
                    terminal_proof,
                )
            }
            CodexPromptHistorySourceBackedDispositionV0::Unchanged
            | CodexPromptHistorySourceBackedDispositionV0::Append => {
                let base = base.ok_or_else(|| {
                    CaptureError::InvalidPayload(
                        "Codex prompt-history append has no base".to_owned(),
                    )
                })?;
                let frontier = base.frontier().ok_or_else(|| {
                    CaptureError::InvalidPayload(
                        "Codex prompt-history append base has no frontier".to_owned(),
                    )
                })?;
                let append = CertifiedSourceAppend::certify(
                    base,
                    scan.certificate.clone(),
                    frontier.certified_prefix_bytes(),
                    *frontier.certified_prefix_digest(),
                )
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
                JsonlFamilyOptimizedLeafOutcome::append(append, terminal_proof)
            }
        };
        #[cfg(test)]
        if let Some(hook) = self
            .state
            .lock()
            .map_err(|_| prompt_family_state_error())?
            .after_scan_hook
            .take()
        {
            hook();
        }
        Ok(outcome)
    }
}

fn capture_error_is_not_found(error: &CaptureError) -> bool {
    match error {
        CaptureError::Io(error) => error.kind() == std::io::ErrorKind::NotFound,
        CaptureError::SystemIo { source, .. } => source.kind() == std::io::ErrorKind::NotFound,
        _ => false,
    }
}

impl JsonlFamilyAdapter for CodexPromptHistoryJsonlFamilyAdapterV0 {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Codex
    }

    fn source_format(&self) -> &'static str {
        SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn root_missing_mode(&self) -> JsonlFamilyRootMissingMode {
        JsonlFamilyRootMissingMode::AuthoritativeEmpty
    }

    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions
    }

    fn base_scope(&self) -> JsonlFamilyBaseScope {
        JsonlFamilyBaseScope::Route
    }

    fn discover(&self, root: &Path) -> crate::Result<JsonlFamilyInventory> {
        self.discover_family(root)
    }

    fn discovery_error_kind(&self, error: &CaptureError) -> SourceBackedRouteErrorKind {
        prompt_family_error_kind(error, false)
    }

    fn scan_error_kind(&self, error: &CaptureError) -> SourceBackedRouteErrorKind {
        prompt_family_error_kind(error, true)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> crate::Result<Box<dyn JsonlFamilyProjector>> {
        Err(CaptureError::SystemInvariant(
            "Codex prompt-history JSONL requires the native optimized executor",
        ))
    }

    fn scan_optimized_leaf(
        &self,
        leaf: &JsonlFamilyLeaf,
        base: Option<&CertifiedSource>,
        _base_event_lookup: &BaseEventIdentityLookup,
        _worker: &mut JsonlFamilyWorkerContext,
        emit_page: &mut dyn FnMut(JsonlFamilyPublication, Vec<CoreRecord>) -> crate::Result<()>,
    ) -> crate::Result<Option<JsonlFamilyOptimizedLeafOutcome>> {
        self.scan_leaf(leaf, base, emit_page).map(Some)
    }

    fn base_source_path(&self, _certificate: &CertifiedSource) -> crate::Result<PathBuf> {
        Ok(self.route_path.clone())
    }
}

fn prompt_family_capture_error(error: CodexPromptHistorySourceBackedErrorV0) -> CaptureError {
    match error {
        CodexPromptHistorySourceBackedErrorV0::Capture(error) => error,
        CodexPromptHistorySourceBackedErrorV0::Io(error) => CaptureError::Io(error),
        CodexPromptHistorySourceBackedErrorV0::Json(error) => CaptureError::Json(error),
        CodexPromptHistorySourceBackedErrorV0::PriorSourceMismatch
        | CodexPromptHistorySourceBackedErrorV0::SourceChanged => {
            CaptureError::SourceChangedDuringCapture
        }
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn validate_prompt_family_scan_counts(
    scan: &CodexPromptHistorySourceBackedScanV0,
    base: Option<&CertifiedSource>,
) -> crate::Result<()> {
    let expected = match scan.disposition {
        CodexPromptHistorySourceBackedDispositionV0::Cold
        | CodexPromptHistorySourceBackedDispositionV0::Replacement => {
            scan.certificate.counts().indexed_documents
        }
        CodexPromptHistorySourceBackedDispositionV0::Unchanged => {
            let Some(base) = base else {
                return Err(CaptureError::InvalidPayload(
                    "Codex prompt-history no-op has no base".to_owned(),
                ));
            };
            if scan.certificate != *base {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            0
        }
        CodexPromptHistorySourceBackedDispositionV0::Append => {
            let base = base.ok_or_else(|| {
                CaptureError::InvalidPayload("Codex prompt-history append has no base".to_owned())
            })?;
            scan.certificate
                .counts()
                .indexed_documents
                .checked_sub(base.counts().indexed_documents)
                .ok_or(CaptureError::SourceChangedDuringCapture)?
        }
    };
    if scan.emitted_documents != expected {
        return Err(CaptureError::InvalidPayload(
            "Codex prompt-history emitted-document count did not reconcile".to_owned(),
        ));
    }
    Ok(())
}

fn prompt_family_state_error() -> CaptureError {
    CaptureError::InvalidPayload(
        "Codex prompt-history JSONL family state lock was poisoned".to_owned(),
    )
}

fn prompt_family_error_kind(error: &CaptureError, scanning: bool) -> SourceBackedRouteErrorKind {
    match error {
        CaptureError::SourceChangedDuringCapture => SourceBackedRouteErrorKind::SourceChanged,
        CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound && scanning => {
            SourceBackedRouteErrorKind::SourceChanged
        }
        CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            SourceBackedRouteErrorKind::Unavailable
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    }
}

/// Scans through the retained source capability and emits bounded lexical pages.
#[cfg(test)]
pub(crate) fn scan_codex_prompt_history_source_backed_v0(
    source: CodexPromptHistorySourceBackedSourceV0,
    prior: Option<&CertifiedSource>,
    mut emit: impl FnMut(
        CodexPromptHistorySourceBackedPageV0,
    ) -> CodexPromptHistorySourceBackedResultV0<()>,
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
        move |_, page| emit(page),
    )
}

fn scan_codex_prompt_history_jsonl_family_leaf_v0(
    source: CodexPromptHistorySourceBackedSourceV0,
    prior: Option<&CertifiedSource>,
    emit: impl FnMut(
        CodexPromptHistorySourceBackedDispositionV0,
        CodexPromptHistorySourceBackedPageV0,
    ) -> CodexPromptHistorySourceBackedResultV0<()>,
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

fn scan_codex_prompt_history_source_backed_inner_v0(
    source: CodexPromptHistorySourceBackedSourceV0,
    prior: Option<&CertifiedSource>,
    project_records: bool,
    frozen: CodexPromptHistoryFrozenSnapshotV0,
    mut emit: impl FnMut(
        CodexPromptHistorySourceBackedDispositionV0,
        CodexPromptHistorySourceBackedPageV0,
    ) -> CodexPromptHistorySourceBackedResultV0<()>,
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
            let (terminal_prefix_bytes, terminal_prefix_sha256) = terminal_prefix(prior)?;
            return Ok(CodexPromptHistorySourceBackedScanV0 {
                certificate: prior.clone(),
                disposition: CodexPromptHistorySourceBackedDispositionV0::Unchanged,
                emitted_documents: 0,
                terminal_prefix_bytes,
                terminal_prefix_sha256,
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
        let mut pages = PageEmitter::new(&source, |page| emit(disposition, page));
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
        certificate,
        disposition,
        emitted_documents,
        terminal_prefix_bytes: frozen_len,
        terminal_prefix_sha256: analysis.whole_source_digest,
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

#[cfg(test)]
mod tests;
