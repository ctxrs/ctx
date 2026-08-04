//! Source-backed adapter for one explicitly registered Custom History JSONL file.
//!
//! The durable catalog lineage supplied by the caller is the source identity.
//! The current path is only a resolvable route. This module parses and projects
//! provider facts, but it does not publish lifecycle state or retain event
//! bodies.

#[cfg(test)]
use std::cell::Cell;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::File,
    io::{BufRead, BufReader, BufWriter, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceAppend,
    CertifiedSourceDeletion, CertifiedSourceInventory, CoreRecord, CtxHistoryJsonlEventRecord,
    CtxHistoryJsonlRecord, NativeSessionKey, ProjectionContractError, ScannedSourceCounts,
    SessionEdgeType, SessionIdentityInput, SourceAnchor, SourceFrontier, SourceKey, StableEntityId,
    TypedKey, CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
};
use ctx_history_index::BaseEventIdentityLookup;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::reader::{
    emit_projection_pages, write_spooled_event, CatalogBudget, CustomHistoryCatalogLimits,
    CUSTOM_HISTORY_CATALOG_ENTRY_OVERHEAD_BYTES,
};
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::{
    common::io::OpenedProviderSourceFile,
    provider::{
        custom_history_jsonl::{validate_custom_history_identifier, validate_custom_source_record},
        normalization::{provider_policy_event_text, provider_value_text},
        source_backed::family::jsonl::{
            JsonlCheckpoint, JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyBaseScope,
            JsonlFamilyInventory, JsonlFamilyLeaf, JsonlFamilyOptimizedLeafOutcome,
            JsonlFamilyProjector, JsonlFamilyPublication, JsonlFamilyRootMissingMode,
            JsonlFamilyWorkerContext,
        },
    },
    CaptureError, ProviderImportSummary, ProviderSourceFailureKind, MAX_PROVIDER_JSONL_LINE_BYTES,
};

mod inventory;
mod parser;
#[cfg(test)]
use inventory::open_explicit_source;
pub(crate) use inventory::CustomHistorySourceBackedInventory;
use inventory::{custom_history_jsonl_family_inventory, source_observation};
use parser::parse_projection;

const CUSTOM_SOURCE_IDENTITY_VERSION: u32 = 1;
const CUSTOM_ROUTE_SOURCE_FORMAT: &str = "ctx_history_jsonl_v1";
const CUSTOM_SOURCE_SCHEMA_VARIANT: &str = "ctx-history-jsonl-v1-source-backed-v1";
pub(super) const CUSTOM_SOURCE_BACKED_PARSER_REVISION: &str =
    "custom-history-jsonl-source-backed-v2";
const CUSTOM_SOURCE_FRONTIER_KIND: &str = "custom-history-jsonl-frontier-v2";
pub(super) const CUSTOM_SESSION_KEY_NAMESPACE: &str = "custom-history.session";
pub(super) const CUSTOM_EVENT_KEY_NAMESPACE: &str = "custom-history.event";
pub(super) const CUSTOM_LOGICAL_SESSION_KIND: &str = "custom-history-session";
pub(super) const CUSTOM_LOGICAL_EVENT_KIND: &str = "custom-history-event";
const CUSTOM_CHECKPOINT_VERSION: u32 = 2;
pub(super) const CUSTOM_PAGE_MAX_DOCUMENTS: usize = 64;
pub(super) const CUSTOM_PAGE_MAX_RETAINED_BYTES: usize = 1024 * 1024;
pub(super) const CUSTOM_DOCUMENT_METADATA_MAX_BYTES: usize = 64 * 1024;
pub(super) const CUSTOM_HISTORY_CATALOG_MAX_RECORDS: usize = 1_000_000;
pub(super) const CUSTOM_HISTORY_CATALOG_MAX_METADATA_BYTES: usize = 256 * 1024 * 1024;

const SOURCE_DIGEST_DOMAIN: &[u8] = b"ctx.custom-history.source-prefix.v2\0";

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CustomHistorySourceBackedWork {
    pub(crate) projection_parses: usize,
    pub(crate) source_read_passes: usize,
    pub(crate) provider_records_parsed: usize,
    pub(crate) session_nodes: usize,
    pub(crate) session_dependencies: usize,
    pub(crate) session_root_nodes: usize,
    pub(crate) event_root_lookups: usize,
    pub(crate) spooled_event_body_bytes: usize,
    pub(crate) resident_event_body_bytes: usize,
    pub(crate) peak_resident_event_body_bytes: usize,
    pub(crate) peak_provider_record_bytes: usize,
    pub(crate) retained_events_before_prior_prefix: usize,
    pub(crate) catalog_records: usize,
    pub(crate) catalog_metadata_bytes: usize,
}

#[cfg(test)]
thread_local! {
    static CUSTOM_HISTORY_WORK: Cell<CustomHistorySourceBackedWork> =
        const { Cell::new(CustomHistorySourceBackedWork {
            projection_parses: 0,
            source_read_passes: 0,
            provider_records_parsed: 0,
            session_nodes: 0,
            session_dependencies: 0,
            session_root_nodes: 0,
            event_root_lookups: 0,
            spooled_event_body_bytes: 0,
            resident_event_body_bytes: 0,
            peak_resident_event_body_bytes: 0,
            peak_provider_record_bytes: 0,
            retained_events_before_prior_prefix: 0,
            catalog_records: 0,
            catalog_metadata_bytes: 0,
        }) };
}

#[cfg(test)]
pub(crate) fn reset_custom_history_source_backed_work() {
    CUSTOM_HISTORY_WORK.set(CustomHistorySourceBackedWork::default());
}

#[cfg(test)]
pub(crate) fn custom_history_source_backed_work() -> CustomHistorySourceBackedWork {
    CUSTOM_HISTORY_WORK.get()
}

#[cfg(test)]
pub(super) fn record_custom_history_work(update: impl FnOnce(&mut CustomHistorySourceBackedWork)) {
    let mut work = CUSTOM_HISTORY_WORK.get();
    update(&mut work);
    CUSTOM_HISTORY_WORK.set(work);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CustomHistorySourceBackedBound {
    CatalogRecords,
    CatalogMetadataBytes,
    ParentSessionIdBytes,
    RootSessionIdBytes,
    EdgeIdBytes,
}

#[derive(Debug, Error)]
pub(crate) enum CustomHistorySourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Custom History manifest failed ({kind}): {detail}")]
    StructuralManifest {
        kind: ProviderSourceFailureKind,
        detail: String,
    },
    #[error(
        "Custom History source-backed {limit:?} bound exceeded: maximum {maximum}, observed {observed}"
    )]
    Bounds {
        limit: CustomHistorySourceBackedBound,
        maximum: usize,
        observed: usize,
    },
    #[error("Custom History explicit inventory changed while it was being certified")]
    InventoryChanged,
    #[error("Custom History prior certificate belongs to another explicit registration")]
    PriorSourceMismatch,
    #[error("Custom History source-backed checkpoint is malformed or incompatible")]
    InvalidCheckpoint,
    #[error("Custom History source-backed counters overflowed or did not reconcile")]
    CountMismatch,
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

#[derive(Debug)]
struct CustomHistoryJsonlFamilyAdapter {
    input: CustomHistorySourceBackedInput,
    source: SourceKey,
}

pub(crate) fn custom_history_jsonl_family_adapter(
    input: CustomHistorySourceBackedInput,
) -> CustomHistorySourceBackedResult<Arc<dyn JsonlFamilyAdapter>> {
    let source = input.source_key()?;
    Ok(Arc::new(CustomHistoryJsonlFamilyAdapter { input, source }))
}

impl JsonlFamilyAdapter for CustomHistoryJsonlFamilyAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Custom
    }

    fn source_format(&self) -> &'static str {
        CUSTOM_ROUTE_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        CUSTOM_SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        CUSTOM_SOURCE_BACKED_PARSER_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::Replacement
    }

    fn root_missing_mode(&self) -> JsonlFamilyRootMissingMode {
        JsonlFamilyRootMissingMode::AuthoritativeEmpty
    }

    fn base_scope(&self) -> JsonlFamilyBaseScope {
        JsonlFamilyBaseScope::Route
    }

    fn discover(&self, root: &Path) -> crate::Result<JsonlFamilyInventory> {
        custom_history_jsonl_family_inventory(&self.input, &self.source, root)
            .map_err(custom_history_family_capture_error)
    }

    fn scan_error_kind(
        &self,
        error: &CaptureError,
    ) -> crate::provider::source_backed::SourceBackedRouteErrorKind {
        match error {
            CaptureError::ProviderSource {
                kind: ProviderSourceFailureKind::SchemaIncompatible,
                ..
            } => crate::provider::source_backed::SourceBackedRouteErrorKind::Unsupported,
            _ => crate::provider::source_backed::SourceBackedRouteErrorKind::InvalidSource,
        }
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> crate::Result<Box<dyn JsonlFamilyProjector>> {
        Err(CaptureError::SystemInvariant(
            "Custom History must use optimized JSONL leaf execution",
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
        self.source
            .validate_exact_descriptor(leaf.source())
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let outcome =
            scan_custom_history_source_backed_explicit(&self.input, base, |disposition, page| {
                let publication = match disposition {
                    CustomHistorySourceBackedDisposition::Unchanged
                    | CustomHistorySourceBackedDisposition::Append => {
                        JsonlFamilyPublication::Append
                    }
                    CustomHistorySourceBackedDisposition::Cold
                    | CustomHistorySourceBackedDisposition::Replacement => {
                        JsonlFamilyPublication::Replace
                    }
                };
                emit_page(publication, page.records).map_err(CustomHistorySourceBackedError::from)
            })
            .map_err(custom_history_family_capture_error)?;
        let receipt = match outcome {
            CustomHistorySourceBackedOutcome::Present(receipt) => receipt,
            CustomHistorySourceBackedOutcome::Missing {
                inventory,
                deletion,
            } => {
                if deletion
                    .as_ref()
                    .is_some_and(|deletion| !deletion.verifies(&inventory))
                {
                    return Err(CaptureError::SystemInvariant(
                        "Custom History missing-source evidence does not reconcile",
                    ));
                }
                return Err(CaptureError::SourceChangedDuringCapture);
            }
        };
        let receipt = *receipt;
        let optimized = match (receipt.disposition, receipt.append) {
            (
                CustomHistorySourceBackedDisposition::Unchanged
                | CustomHistorySourceBackedDisposition::Append,
                Some(append),
            ) => JsonlFamilyOptimizedLeafOutcome::append(append),
            (
                CustomHistorySourceBackedDisposition::Cold
                | CustomHistorySourceBackedDisposition::Replacement,
                None,
            ) => JsonlFamilyOptimizedLeafOutcome::replacement(receipt.certificate),
            _ => {
                return Err(CaptureError::SystemInvariant(
                    "Custom History disposition and append evidence disagree",
                ));
            }
        };
        Ok(Some(optimized))
    }

    fn base_source_path(&self, certificate: &CertifiedSource) -> crate::Result<PathBuf> {
        certificate
            .validate_contract()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        self.source
            .validate_exact_descriptor(certificate.observation().source())
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        decode_checkpoint(certificate).map_err(custom_history_family_capture_error)?;
        std::path::absolute(self.input.path()).map_err(CaptureError::from)
    }

    fn revalidate_leaf(
        &self,
        leaf: &JsonlFamilyLeaf,
        certificate: &CertifiedSource,
        checkpoint: Option<&JsonlCheckpoint>,
    ) -> crate::Result<bool> {
        if checkpoint.is_some() || !self.source.exact_descriptor_eq(leaf.source()) {
            return Ok(false);
        }
        revalidate_custom_history_source_backed(&self.input, certificate)
            .map_err(custom_history_family_capture_error)
    }

    fn owns(&self, source: &SourceKey) -> bool {
        self.source.exact_descriptor_eq(source)
    }
}

fn custom_history_family_capture_error(error: CustomHistorySourceBackedError) -> CaptureError {
    match error {
        CustomHistorySourceBackedError::Capture(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

#[derive(Debug, Clone)]
pub(crate) enum CustomHistorySourceBackedDisposition {
    Cold,
    Unchanged,
    Append,
    Replacement,
}

#[derive(Debug)]
pub(crate) struct CustomHistorySourceBackedPage {
    pub(crate) records: Vec<CoreRecord>,
}

#[derive(Debug, Clone)]
pub(crate) struct CustomHistorySourceBackedReceipt {
    pub(crate) certificate: CertifiedSource,
    pub(crate) disposition: CustomHistorySourceBackedDisposition,
    pub(crate) append: Option<CertifiedSourceAppend>,
}

#[derive(Debug, Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "boxing deletion evidence would complicate the coordinator-owned missing-source contract"
)]
pub(crate) enum CustomHistorySourceBackedOutcome {
    Present(Box<CustomHistorySourceBackedReceipt>),
    Missing {
        inventory: CertifiedSourceInventory,
        deletion: Option<CertifiedSourceDeletion>,
    },
}

#[derive(Debug)]
struct CustomHistorySourceBackedStage {
    source: SourceKey,
    receipt: CustomHistorySourceBackedReceipt,
    projection: Option<ParsedProjection>,
    emit_from: u64,
}

impl CustomHistorySourceBackedStage {
    fn disposition(&self) -> &CustomHistorySourceBackedDisposition {
        &self.receipt.disposition
    }

    fn emit_pages(
        &mut self,
        mut emit: impl FnMut(CustomHistorySourceBackedPage) -> CustomHistorySourceBackedResult<()>,
    ) -> CustomHistorySourceBackedResult<()> {
        if let Some(projection) = &mut self.projection {
            emit_projection_pages(&self.source, projection, self.emit_from, &mut emit)?;
        }
        Ok(())
    }

    fn into_receipt(self) -> CustomHistorySourceBackedReceipt {
        self.receipt
    }
}

#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "the staged missing variant preserves the public outcome's evidence shape"
)]
enum CustomHistorySourceBackedStagedOutcome {
    Present(Box<CustomHistorySourceBackedStage>),
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
pub(super) struct CompleteLine {
    pub(super) line_number: usize,
    pub(super) byte_offset: u64,
}

pub(super) type CustomSessionKey = (String, String);
pub(super) type CustomEventKey = (String, String, u64);

#[derive(Debug)]
pub(super) struct CustomSourceCatalogEntry {
    pub(super) provider_key: String,
}

#[derive(Debug)]
pub(super) struct CustomSessionCatalogEntry {
    pub(super) line_number: usize,
    pub(super) source_id: String,
    pub(super) session_id: String,
    pub(super) parent_session_id: Option<String>,
    pub(super) root_session_id: Option<String>,
    pub(super) agent_type: String,
    pub(super) is_primary: bool,
    pub(super) cwd: Option<String>,
}

#[derive(Debug)]
pub(super) struct CustomEventCatalogEntry {
    pub(super) line_number: usize,
    pub(super) line: CompleteLine,
}

#[derive(Debug)]
pub(super) struct SpooledCustomEvent {
    pub(super) source_id: String,
    pub(super) session_id: String,
    pub(super) event_index: u64,
    pub(super) event_id: Option<String>,
    pub(super) event_type: String,
    pub(super) role: Option<String>,
    pub(super) occurred_at_unix_ms: i64,
    pub(super) body: String,
}

impl SpooledCustomEvent {
    pub(super) fn key(&self) -> CustomEventKey {
        (
            self.source_id.clone(),
            self.session_id.clone(),
            self.event_index,
        )
    }
}

#[derive(Debug)]
pub(super) struct ParsedProjection {
    pub(super) sources: BTreeMap<String, CustomSourceCatalogEntry>,
    pub(super) sessions: BTreeMap<CustomSessionKey, CustomSessionCatalogEntry>,
    pub(super) session_roots: BTreeMap<CustomSessionKey, String>,
    pub(super) events: BTreeMap<CustomEventKey, CustomEventCatalogEntry>,
    pub(super) event_spool: File,
    observed_prior_prefix_digest: Option<[u8; 32]>,
    retained_records_before_prior_prefix: Option<u64>,
    counts: ScannedSourceCounts,
    checkpoint: CustomHistoryCheckpoint,
    content_digest: [u8; 32],
}

/// Observes exactly one explicitly supplied regular file.
pub(crate) fn observe_custom_history_source_backed_explicit(
    input: &CustomHistorySourceBackedInput,
) -> CustomHistorySourceBackedResult<CustomHistorySourceBackedInventory> {
    inventory::observe_custom_history_source_backed_explicit(input)
}

/// Scans and projects one explicit source, returning evidence only.
///
/// The shared coordinator owns staging, append/replacement choice,
/// deletion application, and publication.
pub(crate) fn scan_custom_history_source_backed_explicit(
    input: &CustomHistorySourceBackedInput,
    prior: Option<&CertifiedSource>,
    mut emit: impl FnMut(
        &CustomHistorySourceBackedDisposition,
        CustomHistorySourceBackedPage,
    ) -> CustomHistorySourceBackedResult<()>,
) -> CustomHistorySourceBackedResult<CustomHistorySourceBackedOutcome> {
    match stage_custom_history_source_backed_explicit(input, prior)? {
        CustomHistorySourceBackedStagedOutcome::Present(mut stage) => {
            let disposition = stage.disposition().clone();
            stage.emit_pages(|page| emit(&disposition, page))?;
            Ok(CustomHistorySourceBackedOutcome::Present(Box::new(
                (*stage).into_receipt(),
            )))
        }
        CustomHistorySourceBackedStagedOutcome::Missing {
            inventory,
            deletion,
        } => Ok(CustomHistorySourceBackedOutcome::Missing {
            inventory,
            deletion,
        }),
    }
}

/// Builds one source-authoritative projection before the writer chooses its
/// append or replacement staging mode. The staged projection is invocation
/// local and is dropped after its bounded pages are emitted.
fn stage_custom_history_source_backed_explicit(
    input: &CustomHistorySourceBackedInput,
    prior: Option<&CertifiedSource>,
) -> CustomHistorySourceBackedResult<CustomHistorySourceBackedStagedOutcome> {
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
        return Ok(CustomHistorySourceBackedStagedOutcome::Missing {
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
        let checkpoint = decode_checkpoint(prior);
        if prior.parser_revision() == CUSTOM_SOURCE_BACKED_PARSER_REVISION
            && prior.observation() == &opening_observation
            && checkpoint.is_ok()
        {
            let closing = observe_custom_history_source_backed_explicit(input)?;
            opening_inventory.certify_against(&closing)?;
            let checkpoint = checkpoint?;
            let append = CertifiedSourceAppend::certify(
                prior,
                prior.clone(),
                checkpoint.certified_prefix_bytes,
                *prior.content_digest(),
            )?;
            return Ok(CustomHistorySourceBackedStagedOutcome::Present(Box::new(
                CustomHistorySourceBackedStage {
                    source,
                    receipt: CustomHistorySourceBackedReceipt {
                        certificate: prior.clone(),
                        disposition: CustomHistorySourceBackedDisposition::Unchanged,
                        append: Some(append),
                    },
                    projection: None,
                    emit_from: 0,
                },
            )));
        }
    }

    let opened = Arc::clone(
        opening_inventory
            .opened()
            .ok_or(CustomHistorySourceBackedError::InventoryChanged)?,
    );
    let prior_prefix_bytes = prior
        .filter(|prior| prior.parser_revision() == CUSTOM_SOURCE_BACKED_PARSER_REVISION)
        .and_then(|prior| decode_checkpoint(prior).ok())
        .map(|checkpoint| checkpoint.certified_prefix_bytes);
    let projection =
        parse_projection(&opened, prior_prefix_bytes).map_err(|error| match error {
            CustomHistorySourceBackedError::StructuralManifest { kind, detail } => {
                CustomHistorySourceBackedError::Capture(CaptureError::ProviderSource {
                    provider: CaptureProvider::Custom.as_str(),
                    path: input.path().to_path_buf(),
                    kind,
                    detail,
                })
            }
            error => error,
        })?;
    opened.revalidate()?;
    let closing_inventory = observe_custom_history_source_backed_explicit(input)?;
    let closing_ordinary = closing_inventory
        .ordinary()
        .ok_or(CustomHistorySourceBackedError::InventoryChanged)?;
    if opening_inventory.ordinary() != Some(closing_ordinary) {
        return Err(CustomHistorySourceBackedError::InventoryChanged);
    }
    opening_inventory.certify_against(&closing_inventory)?;
    let closing_observation = source_observation(source.clone(), closing_ordinary)?;
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

    let (disposition, emit_from, append) = classify_projection(prior, &projection, &certificate)?;
    Ok(CustomHistorySourceBackedStagedOutcome::Present(Box::new(
        CustomHistorySourceBackedStage {
            source,
            receipt: CustomHistorySourceBackedReceipt {
                certificate,
                disposition,
                append,
            },
            projection: Some(projection),
            emit_from,
        },
    )))
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

#[cfg(test)]
pub(super) fn validate_custom_history_catalog_bounds(
    input: &CustomHistorySourceBackedInput,
    max_records: usize,
    max_metadata_bytes: usize,
) -> CustomHistorySourceBackedResult<ScannedSourceCounts> {
    let opened = open_explicit_source(input.path())?;
    let projection = parser::parse_projection_with_limits(
        &opened,
        None,
        CustomHistoryCatalogLimits::new(max_records, max_metadata_bytes),
    )?;
    opened.revalidate()?;
    Ok(projection.counts)
}

fn classify_projection(
    prior: Option<&CertifiedSource>,
    projection: &ParsedProjection,
    certificate: &CertifiedSource,
) -> CustomHistorySourceBackedResult<(
    CustomHistorySourceBackedDisposition,
    u64,
    Option<CertifiedSourceAppend>,
)> {
    let Some(prior) = prior else {
        return Ok((CustomHistorySourceBackedDisposition::Cold, 0, None));
    };
    let replacement = || (CustomHistorySourceBackedDisposition::Replacement, 0, None);
    if prior.parser_revision() != CUSTOM_SOURCE_BACKED_PARSER_REVISION {
        return Ok(replacement());
    }
    let checkpoint = match decode_checkpoint(prior) {
        Ok(checkpoint) => checkpoint,
        Err(CustomHistorySourceBackedError::InvalidCheckpoint) => return Ok(replacement()),
        Err(error) => return Err(error),
    };
    if projection.checkpoint.certified_prefix_bytes < checkpoint.certified_prefix_bytes {
        return Ok(replacement());
    }
    if projection.observed_prior_prefix_digest != Some(*prior.content_digest()) {
        return Ok(replacement());
    }
    if projection.retained_records_before_prior_prefix != Some(prior.counts().indexed_documents) {
        return Ok(replacement());
    }
    match CertifiedSourceAppend::certify(
        prior,
        certificate.clone(),
        checkpoint.certified_prefix_bytes,
        *prior.content_digest(),
    ) {
        Ok(append) => Ok((
            CustomHistorySourceBackedDisposition::Append,
            checkpoint.certified_prefix_bytes,
            Some(append),
        )),
        Err(ProjectionContractError::AppendCountRegression) => Ok(replacement()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn custom_session_identity(
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

pub(super) fn custom_event_typed_key_parts(
    event_id: Option<&str>,
    event_index: u64,
) -> CustomHistorySourceBackedResult<TypedKey> {
    Ok(match event_id.filter(|id| !id.is_empty()) {
        Some(event_id) => {
            TypedKey::composite(vec![TypedKey::utf8("event_id")?, TypedKey::utf8(event_id)?])?
        }
        None => TypedKey::composite(vec![
            TypedKey::utf8("event_index")?,
            TypedKey::U64(event_index),
        ])?,
    })
}

fn lexical_body(event: &CtxHistoryJsonlEventRecord) -> String {
    let candidate = ["text", "content", "message", "summary", "command"]
        .into_iter()
        .find_map(|key| event.payload.get(key).and_then(provider_value_text))
        .or_else(|| provider_value_text(&event.payload))
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_default();
    let retained = provider_policy_event_text(event.event_type, &candidate, &event.payload);
    if retained.text.is_empty() {
        event.event_type.as_str().to_owned()
    } else {
        retained.text
    }
}

pub(super) fn bounded_metadata(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= CUSTOM_DOCUMENT_METADATA_MAX_BYTES)
        .then(|| value.to_owned())
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
