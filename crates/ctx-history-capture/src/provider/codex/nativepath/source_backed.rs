use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Seek, SeekFrom},
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering as AtomicOrdering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
        Arc,
    },
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceAppend,
    CertifiedSourceDeletion, CertifiedSourceInventory, EventIdentityInput, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, PositionStability,
    ProjectionContractError, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceFrontier, SourceInventoryObservation, SourceKey, SourceObservation, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
#[cfg(test)]
use ctx_history_index::VerifiedIndex;
use ctx_history_index::{
    CommitReceipt, GenerationWriter, IndexError, LexicalDocument, RevalidationTarget, WriterOptions,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    reader::{CodexParseDisposition, CodexScanCounters},
    rows::CodexSourceBackedRowV0,
    source::{CodexCatalogSource, CodexFileObservation, CodexSourceIdentity},
    CodexAppendProof, CodexCheckpointGeneration, CodexNativeCheckpoint, CodexNativeOwnedPage,
    CodexNativeScanner, CodexSessionRow,
};
use crate::{
    provider::codex::{
        catalog::discover_codex_session_catalog, events::codex_content_text,
        nativepath::revalidate_codex_source_observation,
    },
    provider_sources::open_ordinary_file_without_following,
    CaptureError, CODEX_SESSION_SOURCE_FORMAT,
};

const CODEX_SOURCE_ANCHOR_NAMESPACE: &str = "codex.session";
const CODEX_NATIVE_SESSION_NAMESPACE: &str = "codex.session";
const CODEX_NATIVE_EVENT_POSITION_KIND: &str = "codex.jsonl.raw-ordinal";
const CODEX_LOGICAL_SESSION_KIND: &str = "codex-session";
const CODEX_LOGICAL_EVENT_KIND: &str = "codex-event";
const CODEX_SOURCE_SCHEMA_VARIANT: &str = "codex-nativepath-jsonl-v0";
const CODEX_SOURCE_REVISION_KIND: &str = "codex-ordinary-file-observation-v0";
const CODEX_FRONTIER_KIND: &str = "codex-nativepath-checkpoint-v4";
const CODEX_PARSER_REVISION: &str = "codex-nativepath-source-backed-v0";
const CODEX_INVENTORY_AUTHORITY_NAMESPACE: &str = "codex.sessions-root";
const CODEX_INVENTORY_REVISION_KIND: &str = "codex-session-tree-inventory-v0";
const CODEX_DISCOVERY_REVISION: &str = "codex-session-catalog-v0";
const MAX_HYDRATED_CODEX_RECORD_BYTES: u64 = 16 * 1024 * 1024 + 1;
const MAX_CODEX_SCANNER_WORKERS: usize = 16;
const COLD_LANE_RECEIVE_TIMEOUT: Duration = Duration::from_millis(25);

#[derive(Debug, Error)]
pub enum CodexSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Codex catalog discovery rejected {rejected} sources and failed {failed} sources")]
    IncompleteCatalog { rejected: usize, failed: usize },
    #[error("Codex catalog source {path:?} has no native session ID")]
    MissingNativeSessionId { path: PathBuf },
    #[error("Codex native session ID {0:?} resolves to more than one source")]
    DuplicateNativeSessionId(String),
    #[error("Codex source {0:?} is not a cold source or exact append")]
    UnsupportedLifecycle(String),
    #[error("Codex source certificate has no NativePath checkpoint frontier")]
    MissingCheckpoint,
    #[error("Codex source certificate has an unsupported checkpoint kind or payload")]
    InvalidCheckpoint,
    #[error("Codex scanner emitted a row without exact source-record evidence")]
    MissingRecordEvidence,
    #[error("Codex scanner emitted a row without a bounded lexical preview")]
    MissingLexicalPreview,
    #[error("Codex scanner emitted a row without its native session owner")]
    MissingPageOwner,
    #[error("Codex scanner owner {actual:?} does not match catalog owner {expected:?}")]
    OwnerMismatch { expected: String, actual: String },
    #[error("Codex scan counters do not reconcile with streamed lexical documents")]
    ScanCountMismatch,
    #[error("Codex source count overflow")]
    CountOverflow,
    #[error("Codex cold scanner lane {lane} disconnected before completing its sources")]
    ColdLaneDisconnected { lane: usize },
    #[error("Codex cold scanner lane {lane} panicked")]
    ColdWorkerPanicked { lane: usize },
    #[error("Codex cold scanner protocol mismatch: {0}")]
    ColdProtocolMismatch(&'static str),
    #[cfg(test)]
    #[error("injected Codex cold scanner failure for source {source_index}")]
    InjectedColdWorkerFailure { source_index: usize },
    #[error("Codex Core-only scanner emitted a Pro page")]
    UnexpectedProPage,
    #[error("Codex source-backed scanner emitted a legacy Core publication row")]
    UnexpectedLegacyRow,
    #[error("locator is not a Codex NativePath JSONL record")]
    InvalidCodexLocator,
    #[error("Codex locator native session {0:?} was not found below the supplied session root")]
    LocatorSourceNotFound(String),
    #[error("Codex locator byte range exceeds the bounded NativePath record size")]
    LocatorRangeTooLarge,
    #[error("Codex locator byte range ends after the provider source")]
    LocatorRangeMissing,
    #[error("Codex locator record digest no longer matches provider bytes")]
    LocatorDigestMismatch,
}

pub type CodexSourceBackedResultV0<T> = Result<T, CodexSourceBackedErrorV0>;

#[derive(Debug, Clone, Copy, Default)]
pub struct CodexSourceBackedPhaseTimingsV0 {
    pub discovery: Duration,
    pub writer_open: Duration,
    pub scan_and_stage: Duration,
    pub scanner_worker_busy: Duration,
    pub writer_add_document: Duration,
    pub certification: Duration,
    pub commit: Duration,
    pub total: Duration,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodexSourceBackedCountersV0 {
    pub catalog_sources: u64,
    pub catalog_source_bytes: u64,
    pub cold_sources: u64,
    pub appended_sources: u64,
    pub replaced_sources: u64,
    pub replayed_sources: u64,
    pub deleted_sources: u64,
    pub scanner_workers: u64,
    pub staged_documents: u64,
    pub complete_records_scanned: u64,
    pub retained_records_scanned: u64,
    pub rejected_records_scanned: u64,
    pub ignored_records_scanned: u64,
    pub scanner_bytes_read: u64,
    pub checkpoint_validation_bytes: u64,
    pub prefiltered_records: u64,
    pub structural_json_parses: u64,
    pub typed_json_parses: u64,
    pub emitted_pages: u64,
    pub scanner_legacy_body_json_serializations: u64,
    pub scanner_legacy_row_json_serializations: u64,
    pub scanner_legacy_json_serialized_bytes: u64,
    pub scanner_legacy_normalized_payload_hashes: u64,
    pub scanner_legacy_file_touch_rows: u64,
    pub scanner_legacy_complete_content_locators: u64,
    pub scanner_legacy_duplicate_preview_allocations: u64,
    pub scanner_legacy_page_owner_json_serializations: u64,
    pub scanner_legacy_page_identity_owner_json_serializations: u64,
    pub scanner_legacy_page_identity_row_json_serializations: u64,
}

impl CodexSourceBackedCountersV0 {
    fn add_scan(&mut self, scan: CodexScanCounters) {
        self.complete_records_scanned = self
            .complete_records_scanned
            .saturating_add(scan.complete_records);
        self.retained_records_scanned = self
            .retained_records_scanned
            .saturating_add(scan.retained_records);
        self.rejected_records_scanned = self
            .rejected_records_scanned
            .saturating_add(scan.rejected_complete_records);
        let classified = scan
            .retained_records
            .saturating_add(scan.rejected_complete_records);
        self.ignored_records_scanned = self
            .ignored_records_scanned
            .saturating_add(scan.complete_records.saturating_sub(classified));
        self.scanner_bytes_read = self.scanner_bytes_read.saturating_add(scan.bytes_read);
        self.checkpoint_validation_bytes = self
            .checkpoint_validation_bytes
            .saturating_add(scan.checkpoint_validation_bytes);
        self.prefiltered_records = self
            .prefiltered_records
            .saturating_add(scan.prefiltered_records);
        self.structural_json_parses = self
            .structural_json_parses
            .saturating_add(scan.structural_json_parses);
        self.typed_json_parses = self
            .typed_json_parses
            .saturating_add(scan.typed_json_parses);
        self.emitted_pages = self.emitted_pages.saturating_add(scan.emitted_pages);
        self.scanner_legacy_body_json_serializations = self
            .scanner_legacy_body_json_serializations
            .saturating_add(scan.legacy_body_json_serializations);
        self.scanner_legacy_row_json_serializations = self
            .scanner_legacy_row_json_serializations
            .saturating_add(scan.legacy_row_json_serializations);
        self.scanner_legacy_json_serialized_bytes = self
            .scanner_legacy_json_serialized_bytes
            .saturating_add(scan.legacy_json_serialized_bytes);
        self.scanner_legacy_normalized_payload_hashes = self
            .scanner_legacy_normalized_payload_hashes
            .saturating_add(scan.retained_hashes_created);
        self.scanner_legacy_file_touch_rows = self
            .scanner_legacy_file_touch_rows
            .saturating_add(scan.legacy_file_touch_rows_created);
        self.scanner_legacy_complete_content_locators = self
            .scanner_legacy_complete_content_locators
            .saturating_add(scan.legacy_complete_content_locators_created);
        self.scanner_legacy_page_owner_json_serializations = self
            .scanner_legacy_page_owner_json_serializations
            .saturating_add(scan.legacy_page_owner_json_serializations);
        self.scanner_legacy_page_identity_owner_json_serializations = self
            .scanner_legacy_page_identity_owner_json_serializations
            .saturating_add(scan.legacy_page_identity_owner_json_serializations);
        self.scanner_legacy_page_identity_row_json_serializations = self
            .scanner_legacy_page_identity_row_json_serializations
            .saturating_add(scan.legacy_page_identity_row_json_serializations);
    }
}

#[derive(Debug, Clone)]
pub struct CodexSourceBackedIngestReceiptV0 {
    pub commit: CommitReceipt,
    pub timings: CodexSourceBackedPhaseTimingsV0,
    pub counters: CodexSourceBackedCountersV0,
}

#[derive(Debug, Clone, Copy, Default)]
struct ColdParallelOptionsV0 {
    scanner_workers: Option<usize>,
    #[cfg(test)]
    fail_source_index: Option<usize>,
    #[cfg(test)]
    before_commit_revalidation: Option<fn(&Path)>,
}

#[derive(Debug)]
struct ColdSourcePlanV0 {
    source_key: SourceKey,
    native_session_id: String,
    session_id: StableEntityId,
}

#[derive(Debug)]
struct CodexRootInventoryV0 {
    sources: Vec<(CodexCatalogSource, SourceKey, String)>,
    certificate: CertifiedSourceInventory,
}

#[derive(Debug)]
struct ColdSourceJobV0 {
    source_index: usize,
    source: CodexCatalogSource,
    source_key: SourceKey,
    native_session_id: String,
    session_id: StableEntityId,
}

#[derive(Debug)]
struct ColdPreparedPageV0 {
    source_index: usize,
    page_index: u64,
    documents: Vec<LexicalDocument>,
}

#[derive(Debug)]
struct ColdSourceCompleteV0 {
    source_index: usize,
    page_count: u64,
    staged_documents: u64,
    scan: super::CodexSourceScan,
    worker_busy: Duration,
}

#[derive(Debug)]
enum ColdLaneMessageV0 {
    Page(ColdPreparedPageV0),
    Complete(ColdSourceCompleteV0),
}

#[derive(Debug)]
struct ColdWorkerFailureV0 {
    error: CodexSourceBackedErrorV0,
}

#[derive(Debug)]
struct ColdLaneStateV0 {
    source_indices: Vec<usize>,
    next_source: usize,
    next_page: u64,
    staged_documents: u64,
    last_event_sequence: Option<u64>,
}

impl ColdLaneStateV0 {
    fn expected_source(&self) -> Option<usize> {
        self.source_indices.get(self.next_source).copied()
    }

    fn complete_source(&mut self) {
        self.next_source = self.next_source.saturating_add(1);
        self.next_page = 0;
        self.staged_documents = 0;
        self.last_event_sequence = None;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexHydratedRecordV0 {
    pub provider_bytes: Vec<u8>,
    pub decoded_display_text: Option<String>,
}

/// One-invocation resolver for locator-backed event and session rendering.
///
/// Discovery is intentionally paid once so rendering a session does not
/// recatalog every provider tree for every event.
#[derive(Debug)]
pub struct CodexLocatorResolverV0 {
    sources_by_native_session: HashMap<String, (CodexCatalogSource, SourceKey)>,
}

impl CodexLocatorResolverV0 {
    pub fn discover<I, P>(session_roots: I) -> CodexSourceBackedResultV0<Self>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut sources_by_native_session = HashMap::new();
        for session_root in session_roots {
            let (catalog_summary, sessions) =
                discover_codex_session_catalog(session_root.as_ref())?;
            let discovery = super::discover_codex_catalog_sources(&sessions);
            if catalog_summary.failed_sessions != 0 || !discovery.rejections.is_empty() {
                return Err(CodexSourceBackedErrorV0::IncompleteCatalog {
                    rejected: discovery.rejections.len(),
                    failed: catalog_summary.failed_sessions,
                });
            }
            for (source, source_key, native_session_id) in bind_source_keys(discovery.sources)? {
                if sources_by_native_session
                    .insert(native_session_id.clone(), (source, source_key))
                    .is_some()
                {
                    return Err(CodexSourceBackedErrorV0::DuplicateNativeSessionId(
                        native_session_id,
                    ));
                }
            }
        }
        Ok(Self {
            sources_by_native_session,
        })
    }

    pub fn hydrate(
        &self,
        locator: &SourceRecordLocator,
    ) -> CodexSourceBackedResultV0<CodexHydratedRecordV0> {
        locator.validate_contract()?;
        let (native_session_id, byte_offset, byte_length, physical_ordinal) =
            validate_codex_locator(locator)?;
        if byte_length > MAX_HYDRATED_CODEX_RECORD_BYTES {
            return Err(CodexSourceBackedErrorV0::LocatorRangeTooLarge);
        }

        let (source, source_key) = self
            .sources_by_native_session
            .get(&native_session_id)
            .ok_or_else(|| {
                CodexSourceBackedErrorV0::LocatorSourceNotFound(native_session_id.clone())
            })?;
        if !source_key.exact_descriptor_eq(locator.source()) {
            return Err(CodexSourceBackedErrorV0::InvalidCodexLocator);
        }

        hydrate_codex_source_record(source, locator, byte_offset, byte_length, physical_ordinal)
    }
}

pub fn ingest_codex_source_backed_v0(
    session_root: impl AsRef<Path>,
    global_index_root: impl AsRef<Path>,
) -> CodexSourceBackedResultV0<CodexSourceBackedIngestReceiptV0> {
    ingest_codex_source_backed_inner_v0(
        session_root.as_ref(),
        global_index_root.as_ref(),
        ColdParallelOptionsV0::default(),
    )
}

fn ingest_codex_source_backed_inner_v0(
    session_root: &Path,
    global_index_root: &Path,
    cold_options: ColdParallelOptionsV0,
) -> CodexSourceBackedResultV0<CodexSourceBackedIngestReceiptV0> {
    let total_started = Instant::now();
    let mut timings = CodexSourceBackedPhaseTimingsV0::default();
    let mut counters = CodexSourceBackedCountersV0::default();

    let phase_started = Instant::now();
    let opening_inventory = discover_codex_root_inventory_v0(session_root)?;
    counters.catalog_sources = u64::try_from(opening_inventory.sources.len())
        .map_err(|_| CodexSourceBackedErrorV0::CountOverflow)?;
    timings.discovery = phase_started.elapsed();

    let writer_options = WriterOptions::default();
    let phase_started = Instant::now();
    let mut writer = GenerationWriter::open(global_index_root, writer_options.clone())?;
    timings.writer_open = phase_started.elapsed();
    let base_sources = writer_base_sources(&writer);
    let CodexRootInventoryV0 {
        mut sources,
        certificate: opening_certificate,
    } = opening_inventory;
    counters.catalog_source_bytes = sources.iter().fold(0_u64, |total, (source, _, _)| {
        total.saturating_add(source.catalog_observation.len)
    });
    let use_parallel_cold = sources.len() > 1
        && sources
            .iter()
            .all(|(_, source_key, _)| !base_sources.contains_key(source_key));
    if use_parallel_cold {
        sources.sort_by_key(|(_, source_key, _)| source_key.identity().digest());
    }
    let mut revalidation = HashMap::<SourceKey, (CodexCatalogSource, CodexFileObservation)>::new();

    if use_parallel_cold {
        let worker_count = cold_scanner_worker_count(
            counters.catalog_sources,
            writer_options.indexer_threads,
            cold_options.scanner_workers,
        )?;
        ingest_codex_cold_parallel_v0(
            sources,
            &mut writer,
            &mut revalidation,
            &mut timings,
            &mut counters,
            worker_count,
            cold_options,
        )?;
    } else {
        ingest_codex_sources_serial_v0(
            sources,
            &base_sources,
            &mut writer,
            &mut revalidation,
            &mut timings,
            &mut counters,
        )?;
    }

    for base in base_sources.values() {
        let source = base.observation().source();
        if managed_codex_session_source(source) && !opening_certificate.contains(source) {
            let deletion =
                CertifiedSourceDeletion::from_inventory(source.clone(), &opening_certificate)?;
            writer.delete_source(deletion)?;
            counters.deleted_sources = counters.deleted_sources.saturating_add(1);
        }
    }

    #[cfg(test)]
    if let Some(hook) = cold_options.before_commit_revalidation {
        hook(session_root);
    }

    // An empty first generation has no source or deletion target through which
    // GenerationWriter can invoke its terminal callback. Revalidate that
    // degenerate inventory explicitly; every non-empty refresh is fenced
    // inside prepare_commit below.
    if revalidation.is_empty() && counters.deleted_sources == 0 {
        let closing = discover_codex_root_inventory_v0(session_root)?;
        if closing.certificate != opening_certificate {
            return Err(CodexSourceBackedErrorV0::Capture(
                CaptureError::SourceChangedDuringCapture,
            ));
        }
    }

    let commit_started = Instant::now();
    let mut closing_inventory = None::<Option<CertifiedSourceInventory>>;
    let commit = writer.commit(|target| {
        if closing_inventory.is_none() {
            closing_inventory = Some(
                discover_codex_root_inventory_v0(session_root)
                    .ok()
                    .and_then(|closing| {
                        (closing.certificate == opening_certificate).then_some(closing.certificate)
                    }),
            );
        }
        let Some(closing) = closing_inventory
            .as_ref()
            .and_then(std::option::Option::as_ref)
        else {
            return false;
        };
        match target {
            RevalidationTarget::Source(certificate) => revalidation
                .get_key_value(certificate.observation().source())
                .is_some_and(|(source_key, (source, observation))| {
                    closing.contains(source_key)
                        && source_key.exact_descriptor_eq(certificate.observation().source())
                        && revalidate_codex_source_observation(source, observation).is_ok()
                }),
            RevalidationTarget::Deletion(deletion) => deletion.verifies(closing),
        }
    })?;
    timings.commit = commit_started.elapsed();
    timings.total = total_started.elapsed();
    Ok(CodexSourceBackedIngestReceiptV0 {
        commit,
        timings,
        counters,
    })
}

fn ingest_codex_sources_serial_v0(
    sources: Vec<(CodexCatalogSource, SourceKey, String)>,
    base_sources: &HashMap<SourceKey, CertifiedSource>,
    writer: &mut GenerationWriter,
    revalidation: &mut HashMap<SourceKey, (CodexCatalogSource, CodexFileObservation)>,
    timings: &mut CodexSourceBackedPhaseTimingsV0,
    counters: &mut CodexSourceBackedCountersV0,
) -> CodexSourceBackedResultV0<()> {
    for (source, source_key, native_session_id) in sources {
        let base = base_sources.get(&source_key).cloned();
        if base
            .as_ref()
            .is_some_and(|base| !base.observation().source().exact_descriptor_eq(&source_key))
        {
            return Err(CodexSourceBackedErrorV0::UnsupportedLifecycle(
                native_session_id,
            ));
        }
        let proof = base
            .as_ref()
            .filter(|base| base.parser_revision() == CODEX_PARSER_REVISION)
            .and_then(|base| decode_append_proof(&source, &source_key, base).ok());

        // An unchanged strong file observation (identity + ctime-backed change
        // token + length + mtime) means the already-certified generation is
        // still the provider source. Rehashing every byte here made a no-op
        // refresh O(total history bytes). Final commit revalidation repeats
        // the observation before publishing.
        if let (Some(base), Some(proof)) = (base.as_ref(), proof.as_ref()) {
            if proof.checkpoint.observation == source.catalog_observation {
                let certification_started = Instant::now();
                let writer_base = writer.begin_source_append(source_key.clone())?;
                if writer_base != base {
                    return Err(CodexSourceBackedErrorV0::InvalidCheckpoint);
                }
                let base_frontier = base
                    .frontier()
                    .ok_or(CodexSourceBackedErrorV0::MissingCheckpoint)?;
                let append = CertifiedSourceAppend::certify(
                    base,
                    base.clone(),
                    base_frontier.certified_prefix_bytes(),
                    *base_frontier.certified_prefix_digest(),
                )?;
                writer.certify_source_append(append)?;
                timings.certification += certification_started.elapsed();
                counters.replayed_sources = counters.replayed_sources.saturating_add(1);
                revalidation.insert(source_key, (source, proof.checkpoint.observation.clone()));
                continue;
            }
        }

        let scan_started = Instant::now();
        counters.scanner_workers = 1;
        let scanner_started = Instant::now();
        let append_base = match (base.as_ref(), proof.as_ref()) {
            (Some(base), Some(proof))
                if source.catalog_observation.len > proof.checkpoint.observation.len =>
            {
                match CodexNativeScanner::new_source_backed_v0(source.clone(), Some(proof)) {
                    Ok(scanner) => Some((base, scanner)),
                    Err(error) if invalid_append_proof(&error) => None,
                    Err(error) => return Err(error.into()),
                }
            }
            _ => None,
        };
        let (append_base, mut scanner) = match append_base {
            Some((base, scanner)) => {
                let writer_base = writer.begin_source_append(source_key.clone())?;
                if writer_base != base {
                    return Err(CodexSourceBackedErrorV0::InvalidCheckpoint);
                }
                (Some(base), scanner)
            }
            None => {
                writer.begin_source(source_key.clone())?;
                (
                    None,
                    CodexNativeScanner::new_source_backed_v0(source.clone(), None)?,
                )
            }
        };
        timings.scanner_worker_busy += scanner_started.elapsed();
        let session_id = codex_session_identity(&source_key, &native_session_id)?;
        let mut staged_for_source = 0_u64;
        loop {
            let scanner_started = Instant::now();
            let page = scanner.next_page()?;
            timings.scanner_worker_busy += scanner_started.elapsed();
            let Some(page) = page else {
                break;
            };
            let CodexNativeOwnedPage::Core(page) = page else {
                return Err(CodexSourceBackedErrorV0::UnexpectedProPage);
            };
            if !page.core_rows.is_empty() {
                return Err(CodexSourceBackedErrorV0::UnexpectedLegacyRow);
            }
            let owner = page
                .owner
                .as_ref()
                .ok_or(CodexSourceBackedErrorV0::MissingPageOwner)?;
            validate_owner(owner, &native_session_id)?;
            let cwd = owner.cwd.clone();
            for row in page.source_backed_rows {
                let conversion_started = Instant::now();
                let document = codex_lexical_document(
                    &source_key,
                    session_id,
                    &native_session_id,
                    cwd.as_deref(),
                    row,
                )?;
                timings.scanner_worker_busy += conversion_started.elapsed();
                let add_started = Instant::now();
                let add_result = writer.add_document(document);
                timings.writer_add_document += add_started.elapsed();
                add_result?;
                staged_for_source = staged_for_source
                    .checked_add(1)
                    .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
            }
        }
        let scanner_started = Instant::now();
        let scan = scanner.finish()?;
        timings.scanner_worker_busy += scanner_started.elapsed();
        timings.scan_and_stage += scan_started.elapsed();
        let scan_counters = scan.counters;
        counters.add_scan(scan_counters);
        counters.staged_documents = counters
            .staged_documents
            .checked_add(staged_for_source)
            .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;

        let certification_started = Instant::now();
        match (append_base, scan.disposition) {
            (None, CodexParseDisposition::FullGeneration) => {
                let current =
                    certify_scan(&source_key, &scan, None, staged_for_source, scan_counters)?;
                writer.certify_source(current)?;
                if base.is_some() {
                    counters.replaced_sources = counters.replaced_sources.saturating_add(1);
                } else {
                    counters.cold_sources = counters.cold_sources.saturating_add(1);
                }
            }
            (Some(base), CodexParseDisposition::AppendDelta) => {
                let current = certify_scan(
                    &source_key,
                    &scan,
                    Some(base),
                    staged_for_source,
                    scan_counters,
                )?;
                let base_frontier = base
                    .frontier()
                    .ok_or(CodexSourceBackedErrorV0::MissingCheckpoint)?;
                let append = CertifiedSourceAppend::certify(
                    base,
                    current,
                    base_frontier.certified_prefix_bytes(),
                    *base_frontier.certified_prefix_digest(),
                )?;
                writer.certify_source_append(append)?;
                counters.appended_sources = counters.appended_sources.saturating_add(1);
            }
            _ => {
                return Err(CodexSourceBackedErrorV0::UnsupportedLifecycle(
                    native_session_id,
                ));
            }
        }
        timings.certification += certification_started.elapsed();
        revalidation.insert(source_key, (source, scan.after_observation.clone()));
    }
    Ok(())
}

fn invalid_append_proof(error: &CaptureError) -> bool {
    matches!(
        error,
        CaptureError::InvalidPayload(detail)
            if detail.starts_with("invalid Codex append proof:")
    )
}

fn cold_scanner_worker_count(
    source_count: u64,
    indexer_threads: usize,
    override_workers: Option<usize>,
) -> CodexSourceBackedResultV0<usize> {
    let source_count =
        usize::try_from(source_count).map_err(|_| CodexSourceBackedErrorV0::CountOverflow)?;
    let available = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let reserved = indexer_threads.clamp(1, 8).saturating_add(2);
    let automatic = available.saturating_sub(reserved).max(1);
    Ok(override_workers
        .unwrap_or(automatic)
        .clamp(1, MAX_CODEX_SCANNER_WORKERS)
        .min(source_count.max(1)))
}

fn ingest_codex_cold_parallel_v0(
    sources: Vec<(CodexCatalogSource, SourceKey, String)>,
    writer: &mut GenerationWriter,
    revalidation: &mut HashMap<SourceKey, (CodexCatalogSource, CodexFileObservation)>,
    timings: &mut CodexSourceBackedPhaseTimingsV0,
    counters: &mut CodexSourceBackedCountersV0,
    worker_count: usize,
    cold_options: ColdParallelOptionsV0,
) -> CodexSourceBackedResultV0<()> {
    let mut plans = Vec::with_capacity(sources.len());
    let mut lane_jobs = (0..worker_count).map(|_| Vec::new()).collect::<Vec<_>>();
    let mut lane_source_indices = (0..worker_count).map(|_| Vec::new()).collect::<Vec<_>>();

    for (source_index, (source, source_key, native_session_id)) in sources.into_iter().enumerate() {
        writer.begin_source(source_key.clone())?;
        let session_id = codex_session_identity(&source_key, &native_session_id)?;
        plans.push(ColdSourcePlanV0 {
            source_key: source_key.clone(),
            native_session_id: native_session_id.clone(),
            session_id,
        });
        let lane_index = source_index % worker_count;
        lane_source_indices[lane_index].push(source_index);
        lane_jobs[lane_index].push(ColdSourceJobV0 {
            source_index,
            source,
            source_key,
            native_session_id,
            session_id,
        });
    }

    counters.scanner_workers =
        u64::try_from(worker_count).map_err(|_| CodexSourceBackedErrorV0::CountOverflow)?;
    let cancellation = Arc::new(AtomicBool::new(false));
    let pipeline_started = Instant::now();
    let pipeline_result = thread::scope(|scope| {
        let (failure_sender, failure_receiver) = mpsc::channel::<ColdWorkerFailureV0>();
        let mut receivers = Vec::with_capacity(worker_count);
        let mut handles = Vec::with_capacity(worker_count);

        for (lane_index, jobs) in lane_jobs.into_iter().enumerate() {
            let (sender, receiver) = mpsc::sync_channel::<ColdLaneMessageV0>(0);
            receivers.push(receiver);
            let worker_cancellation = Arc::clone(&cancellation);
            let worker_failure_sender = failure_sender.clone();
            handles.push((
                lane_index,
                scope.spawn(move || {
                    let outcome = catch_unwind(AssertUnwindSafe(|| {
                        run_cold_scan_lane_v0(
                            lane_index,
                            jobs,
                            &sender,
                            &worker_cancellation,
                            cold_options,
                        )
                    }));
                    match outcome {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            let _ = worker_failure_sender.send(ColdWorkerFailureV0 { error });
                            worker_cancellation.store(true, AtomicOrdering::Release);
                        }
                        Err(_) => {
                            let _ = worker_failure_sender.send(ColdWorkerFailureV0 {
                                error: CodexSourceBackedErrorV0::ColdWorkerPanicked {
                                    lane: lane_index,
                                },
                            });
                            worker_cancellation.store(true, AtomicOrdering::Release);
                        }
                    }
                }),
            ));
        }
        drop(failure_sender);

        let mut lane_states = lane_source_indices
            .into_iter()
            .map(|source_indices| ColdLaneStateV0 {
                source_indices,
                next_source: 0,
                next_page: 0,
                staged_documents: 0,
                last_event_sequence: None,
            })
            .collect::<Vec<_>>();
        let mut result = consume_cold_lanes_v0(
            &receivers,
            &failure_receiver,
            &cancellation,
            &mut lane_states,
            &plans,
            writer,
            revalidation,
            timings,
            counters,
        );
        if result.is_err() {
            cancellation.store(true, AtomicOrdering::Release);
        }
        drop(receivers);

        let mut join_error = None;
        for (lane_index, handle) in handles {
            if handle.join().is_err() && join_error.is_none() {
                join_error =
                    Some(CodexSourceBackedErrorV0::ColdWorkerPanicked { lane: lane_index });
            }
        }
        if result.is_ok() {
            if let Ok(failure) = failure_receiver.try_recv() {
                result = Err(failure.error);
            } else if let Some(error) = join_error {
                result = Err(error);
            }
        }
        result
    });
    timings.scan_and_stage += pipeline_started.elapsed();
    pipeline_result
}

fn run_cold_scan_lane_v0(
    lane_index: usize,
    jobs: Vec<ColdSourceJobV0>,
    sender: &SyncSender<ColdLaneMessageV0>,
    cancellation: &AtomicBool,
    cold_options: ColdParallelOptionsV0,
) -> CodexSourceBackedResultV0<()> {
    for job in jobs {
        if cancellation.load(AtomicOrdering::Acquire) {
            return Ok(());
        }
        #[cfg(test)]
        if cold_options.fail_source_index == Some(job.source_index) {
            return Err(CodexSourceBackedErrorV0::InjectedColdWorkerFailure {
                source_index: job.source_index,
            });
        }
        #[cfg(not(test))]
        let _ = cold_options;

        let mut worker_busy = Duration::ZERO;
        let busy_started = Instant::now();
        let mut scanner = CodexNativeScanner::new_source_backed_v0(job.source.clone(), None)?;
        worker_busy += busy_started.elapsed();
        let mut page_index = 0_u64;
        let mut staged_documents = 0_u64;

        loop {
            if cancellation.load(AtomicOrdering::Acquire) {
                return Ok(());
            }
            let busy_started = Instant::now();
            let page = scanner.next_page()?;
            worker_busy += busy_started.elapsed();
            let Some(page) = page else {
                break;
            };
            let busy_started = Instant::now();
            let CodexNativeOwnedPage::Core(page) = page else {
                return Err(CodexSourceBackedErrorV0::UnexpectedProPage);
            };
            if !page.core_rows.is_empty() {
                return Err(CodexSourceBackedErrorV0::UnexpectedLegacyRow);
            }
            let owner = page
                .owner
                .as_ref()
                .ok_or(CodexSourceBackedErrorV0::MissingPageOwner)?;
            validate_owner(owner, &job.native_session_id)?;
            let cwd = owner.cwd.clone();
            let mut documents = Vec::with_capacity(page.source_backed_rows.len());
            for row in page.source_backed_rows {
                documents.push(codex_lexical_document(
                    &job.source_key,
                    job.session_id,
                    &job.native_session_id,
                    cwd.as_deref(),
                    row,
                )?);
                staged_documents = staged_documents
                    .checked_add(1)
                    .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
            }
            scanner.release_transient_record_buffer();
            worker_busy += busy_started.elapsed();
            if !send_cold_lane_message_v0(
                sender,
                ColdLaneMessageV0::Page(ColdPreparedPageV0 {
                    source_index: job.source_index,
                    page_index,
                    documents,
                }),
                cancellation,
                lane_index,
            )? {
                return Ok(());
            }
            page_index = page_index
                .checked_add(1)
                .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
        }

        if cancellation.load(AtomicOrdering::Acquire) {
            return Ok(());
        }
        let busy_started = Instant::now();
        let scan = scanner.finish()?;
        worker_busy += busy_started.elapsed();
        if !send_cold_lane_message_v0(
            sender,
            ColdLaneMessageV0::Complete(ColdSourceCompleteV0 {
                source_index: job.source_index,
                page_count: page_index,
                staged_documents,
                scan,
                worker_busy,
            }),
            cancellation,
            lane_index,
        )? {
            return Ok(());
        }
    }
    Ok(())
}

fn send_cold_lane_message_v0(
    sender: &SyncSender<ColdLaneMessageV0>,
    message: ColdLaneMessageV0,
    cancellation: &AtomicBool,
    lane_index: usize,
) -> CodexSourceBackedResultV0<bool> {
    match sender.send(message) {
        Ok(()) => Ok(true),
        Err(_) if cancellation.load(AtomicOrdering::Acquire) => Ok(false),
        Err(_) => Err(CodexSourceBackedErrorV0::ColdLaneDisconnected { lane: lane_index }),
    }
}

#[allow(clippy::too_many_arguments)]
fn consume_cold_lanes_v0(
    receivers: &[Receiver<ColdLaneMessageV0>],
    failure_receiver: &Receiver<ColdWorkerFailureV0>,
    cancellation: &AtomicBool,
    lane_states: &mut [ColdLaneStateV0],
    plans: &[ColdSourcePlanV0],
    writer: &mut GenerationWriter,
    revalidation: &mut HashMap<SourceKey, (CodexCatalogSource, CodexFileObservation)>,
    timings: &mut CodexSourceBackedPhaseTimingsV0,
    counters: &mut CodexSourceBackedCountersV0,
) -> CodexSourceBackedResultV0<()> {
    let mut completed_sources = 0_usize;
    let mut next_lane = 0_usize;
    while completed_sources < plans.len() {
        if let Ok(failure) = failure_receiver.try_recv() {
            return Err(failure.error);
        }
        if cancellation.load(AtomicOrdering::Acquire) {
            return Err(wait_for_cold_worker_failure_v0(failure_receiver)?);
        }

        let lane_index = (0..lane_states.len())
            .map(|offset| (next_lane + offset) % lane_states.len())
            .find(|lane_index| lane_states[*lane_index].expected_source().is_some())
            .ok_or(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                "no lane owns an incomplete source",
            ))?;
        next_lane = (lane_index + 1) % lane_states.len();
        let message = match receivers[lane_index].recv_timeout(COLD_LANE_RECEIVE_TIMEOUT) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                if let Ok(failure) = failure_receiver.recv_timeout(COLD_LANE_RECEIVE_TIMEOUT) {
                    return Err(failure.error);
                }
                return Err(CodexSourceBackedErrorV0::ColdLaneDisconnected { lane: lane_index });
            }
        };

        let lane_state = &mut lane_states[lane_index];
        let expected_source =
            lane_state
                .expected_source()
                .ok_or(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                    "lane emitted after completing all assigned sources",
                ))?;
        match message {
            ColdLaneMessageV0::Page(page) => {
                if page.source_index != expected_source || page.page_index != lane_state.next_page {
                    return Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                        "source or page arrived out of order",
                    ));
                }
                let plan = plans.get(page.source_index).ok_or(
                    CodexSourceBackedErrorV0::ColdProtocolMismatch(
                        "page references an unknown source",
                    ),
                )?;
                for document in page.documents {
                    if !document.source.exact_descriptor_eq(&plan.source_key)
                        || document.session_id != plan.session_id
                        || document.provider_session_id.as_deref()
                            != Some(plan.native_session_id.as_str())
                    {
                        return Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                            "document identity does not match its assigned source",
                        ));
                    }
                    let (_, _, _, physical_ordinal) = validate_codex_locator(&document.locator)?;
                    if physical_ordinal != document.event_sequence
                        || lane_state
                            .last_event_sequence
                            .is_some_and(|last| document.event_sequence <= last)
                    {
                        return Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                            "document event sequence is not strictly increasing",
                        ));
                    }
                    let event_sequence = document.event_sequence;
                    let add_started = Instant::now();
                    let add_result = writer.add_document(document);
                    timings.writer_add_document += add_started.elapsed();
                    add_result?;
                    lane_state.last_event_sequence = Some(event_sequence);
                    lane_state.staged_documents = lane_state
                        .staged_documents
                        .checked_add(1)
                        .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
                }
                lane_state.next_page = lane_state
                    .next_page
                    .checked_add(1)
                    .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
            }
            ColdLaneMessageV0::Complete(complete) => {
                if complete.source_index != expected_source
                    || complete.page_count != lane_state.next_page
                    || complete.staged_documents != lane_state.staged_documents
                {
                    return Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                        "source completion does not match accepted pages",
                    ));
                }
                let plan = plans.get(complete.source_index).ok_or(
                    CodexSourceBackedErrorV0::ColdProtocolMismatch(
                        "completion references an unknown source",
                    ),
                )?;
                if complete.scan.disposition != CodexParseDisposition::FullGeneration
                    || complete.scan.source.catalog_native_session_id.as_deref()
                        != Some(plan.native_session_id.as_str())
                {
                    return Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                        "cold scanner completed with the wrong source or disposition",
                    ));
                }
                let scan_counters = complete.scan.counters;
                let certification_started = Instant::now();
                let current = certify_scan(
                    &plan.source_key,
                    &complete.scan,
                    None,
                    complete.staged_documents,
                    scan_counters,
                )?;
                writer.certify_source(current)?;
                timings.certification += certification_started.elapsed();
                timings.scanner_worker_busy += complete.worker_busy;
                counters.add_scan(scan_counters);
                counters.staged_documents = counters
                    .staged_documents
                    .checked_add(complete.staged_documents)
                    .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
                counters.cold_sources = counters.cold_sources.saturating_add(1);
                let after_observation = complete.scan.after_observation.clone();
                revalidation.insert(
                    plan.source_key.clone(),
                    (complete.scan.source, after_observation),
                );
                lane_state.complete_source();
                completed_sources = completed_sources
                    .checked_add(1)
                    .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
            }
        }
    }
    Ok(())
}

fn wait_for_cold_worker_failure_v0(
    failure_receiver: &Receiver<ColdWorkerFailureV0>,
) -> CodexSourceBackedResultV0<CodexSourceBackedErrorV0> {
    match failure_receiver.recv_timeout(COLD_LANE_RECEIVE_TIMEOUT) {
        Ok(failure) => Ok(failure.error),
        Err(_) => Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
            "scanner cancellation was signaled without a worker failure",
        )),
    }
}

pub fn hydrate_codex_locator(
    session_root: impl AsRef<Path>,
    locator: &SourceRecordLocator,
) -> CodexSourceBackedResultV0<CodexHydratedRecordV0> {
    CodexLocatorResolverV0::discover([session_root])?.hydrate(locator)
}

fn hydrate_codex_source_record(
    source: &CodexCatalogSource,
    locator: &SourceRecordLocator,
    byte_offset: u64,
    byte_length: u64,
    physical_ordinal: u64,
) -> CodexSourceBackedResultV0<CodexHydratedRecordV0> {
    let range_end = byte_offset
        .checked_add(byte_length)
        .ok_or(CodexSourceBackedErrorV0::LocatorRangeTooLarge)?;
    let mut file = open_ordinary_file_without_following(&source.source_path)?;
    if file.metadata()?.len() < range_end {
        return Err(CodexSourceBackedErrorV0::LocatorRangeMissing);
    }
    file.seek(SeekFrom::Start(byte_offset))?;
    let byte_length =
        usize::try_from(byte_length).map_err(|_| CodexSourceBackedErrorV0::LocatorRangeTooLarge)?;
    let mut provider_bytes = vec![0_u8; byte_length];
    file.read_exact(&mut provider_bytes)?;
    let actual_digest: [u8; 32] = Sha256::digest(&provider_bytes).into();
    if &actual_digest != locator.record_digest() {
        return Err(CodexSourceBackedErrorV0::LocatorDigestMismatch);
    }
    let decoded_display_text = decode_exact_display_text(&provider_bytes, physical_ordinal)?;
    Ok(CodexHydratedRecordV0 {
        provider_bytes,
        decoded_display_text,
    })
}

fn bind_source_keys(
    sources: Vec<CodexCatalogSource>,
) -> CodexSourceBackedResultV0<Vec<(CodexCatalogSource, SourceKey, String)>> {
    let mut native_ids = HashSet::new();
    let mut bound = Vec::with_capacity(sources.len());
    for source in sources {
        let native_session_id = source.catalog_native_session_id.clone().ok_or_else(|| {
            CodexSourceBackedErrorV0::MissingNativeSessionId {
                path: source.source_path.clone(),
            }
        })?;
        if !native_ids.insert(native_session_id.clone()) {
            return Err(CodexSourceBackedErrorV0::DuplicateNativeSessionId(
                native_session_id,
            ));
        }
        let source_key = codex_source_key(&native_session_id)?;
        bound.push((source, source_key, native_session_id));
    }
    Ok(bound)
}

fn discover_codex_root_inventory_v0(
    session_root: &Path,
) -> CodexSourceBackedResultV0<CodexRootInventoryV0> {
    let opening_root_revision = codex_root_revision_v0(session_root)?;
    let (catalog_summary, sessions) = discover_codex_session_catalog(session_root)?;
    let discovery = super::discover_codex_catalog_sources(&sessions);
    if catalog_summary.failed_sessions != 0 || !discovery.rejections.is_empty() {
        return Err(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: discovery.rejections.len(),
            failed: catalog_summary.failed_sessions,
        });
    }
    let sources = bind_source_keys(discovery.sources)?;
    let closing_root_revision = codex_root_revision_v0(session_root)?;
    if opening_root_revision != closing_root_revision {
        return Err(CodexSourceBackedErrorV0::Capture(
            CaptureError::SourceChangedDuringCapture,
        ));
    }
    let observation =
        codex_inventory_observation_v0(session_root, &opening_root_revision, &sources)?;
    let source_keys = sources
        .iter()
        .map(|(_, source_key, _)| source_key.clone())
        .collect::<Vec<_>>();
    let certificate = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        CODEX_DISCOVERY_REVISION,
        source_keys,
    )?;
    Ok(CodexRootInventoryV0 {
        sources,
        certificate,
    })
}

fn codex_inventory_observation_v0(
    session_root: &Path,
    root_revision: &[u8; 32],
    sources: &[(CodexCatalogSource, SourceKey, String)],
) -> CodexSourceBackedResultV0<SourceInventoryObservation> {
    let root_identity = crate::provider::importer::provider_path_identity(session_root)
        .map_err(CodexSourceBackedErrorV0::Capture)?;
    let authority_key: [u8; 32] = Sha256::digest(root_identity.as_bytes()).into();
    let mut ordered = sources.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|(_, source_key, _)| source_key.identity().digest());

    let mut revision = Sha256::new();
    revision.update(b"ctx.codex-session-tree-inventory-v0\0");
    revision.update(root_revision);
    hash_inventory_field(&mut revision, root_identity.as_bytes());
    revision.update((ordered.len() as u64).to_be_bytes());
    for (source, source_key, native_session_id) in ordered {
        revision.update(source_key.identity().digest());
        revision.update(source_key.exact_descriptor_digest());
        hash_inventory_field(&mut revision, source.source_root.as_bytes());
        let path_identity = crate::provider::importer::provider_path_identity(&source.source_path)?;
        hash_inventory_field(&mut revision, path_identity.as_bytes());
        hash_inventory_field(&mut revision, native_session_id.as_bytes());
        hash_inventory_field(
            &mut revision,
            &serde_json::to_vec(&source.catalog_observation)?,
        );
    }
    Ok(SourceInventoryObservation::new(
        CaptureProvider::Codex.as_str(),
        CODEX_INVENTORY_AUTHORITY_NAMESPACE,
        TypedKey::bytes(authority_key.to_vec())?,
        CODEX_INVENTORY_REVISION_KIND,
        revision.finalize().to_vec(),
    )?)
}

fn hash_inventory_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn codex_root_revision_v0(session_root: &Path) -> CodexSourceBackedResultV0<[u8; 32]> {
    let metadata = fs::symlink_metadata(session_root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CodexSourceBackedErrorV0::Capture(
            CaptureError::InvalidProviderTranscriptPath {
                path: session_root.to_path_buf(),
                reason: "Codex session root must be a non-symlink directory",
            },
        ));
    }
    let root_identity = crate::provider::importer::provider_path_identity(session_root)?;
    let mut revision = Sha256::new();
    revision.update(b"ctx.codex-session-root-revision-v0\0");
    hash_inventory_field(&mut revision, root_identity.as_bytes());
    revision.update(metadata.len().to_be_bytes());
    if let Ok(modified) = metadata.modified() {
        let since_epoch = modified.duration_since(UNIX_EPOCH).unwrap_or_default();
        revision.update([1]);
        revision.update(since_epoch.as_secs().to_be_bytes());
        revision.update(since_epoch.subsec_nanos().to_be_bytes());
    } else {
        revision.update([0]);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        revision.update(metadata.dev().to_be_bytes());
        revision.update(metadata.ino().to_be_bytes());
        revision.update(metadata.ctime().to_be_bytes());
        revision.update(metadata.ctime_nsec().to_be_bytes());
        revision.update(metadata.mode().to_be_bytes());
        revision.update(metadata.uid().to_be_bytes());
        revision.update(metadata.gid().to_be_bytes());
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt;

        revision.update(metadata.file_attributes().to_be_bytes());
        revision.update(metadata.creation_time().to_be_bytes());
        revision.update(metadata.last_write_time().to_be_bytes());
        revision.update(metadata.file_size().to_be_bytes());
        revision.update(
            metadata
                .volume_serial_number()
                .unwrap_or_default()
                .to_be_bytes(),
        );
        revision.update(metadata.file_index().unwrap_or_default().to_be_bytes());
    }
    Ok(revision.finalize().into())
}

fn writer_base_sources(writer: &GenerationWriter) -> HashMap<SourceKey, CertifiedSource> {
    writer
        .base_manifest()
        .into_iter()
        .flat_map(|manifest| manifest.sources.iter())
        .cloned()
        .map(|source| (source.observation().source().clone(), source))
        .collect()
}

fn managed_codex_session_source(source: &SourceKey) -> bool {
    source.provider() == CaptureProvider::Codex.as_str()
        && source.source_format() == CODEX_SESSION_SOURCE_FORMAT
        && source.schema_variant() == CODEX_SOURCE_SCHEMA_VARIANT
        && source.provider_identity_version() == 1
}

fn codex_source_key(native_session_id: &str) -> CodexSourceBackedResultV0<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        CODEX_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::Codex.as_str(),
        CODEX_SESSION_SOURCE_FORMAT,
        CODEX_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn codex_session_identity(
    source: &SourceKey,
    native_session_id: &str,
) -> CodexSourceBackedResultV0<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        CODEX_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: CODEX_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn codex_lexical_document(
    source: &SourceKey,
    session_id: StableEntityId,
    native_session_id: &str,
    cwd: Option<&str>,
    row: CodexSourceBackedRowV0,
) -> CodexSourceBackedResultV0<LexicalDocument> {
    let CodexSourceBackedRowV0 {
        raw_ordinal,
        source_record: evidence,
        occurred_at,
        event_type,
        role,
        lexical_body,
        touched_paths,
    } = row;
    let native_item_key = NativeItemKey::certified_position(
        CODEX_NATIVE_EVENT_POSITION_KIND,
        TypedKey::U64(raw_ordinal),
        PositionStability::AppendStable,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: CODEX_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: evidence.byte_offset,
            byte_length: evidence.byte_length,
            physical_ordinal: raw_ordinal,
            native_session_key: Some(TypedKey::utf8(native_session_id)?),
            native_event_key: Some(TypedKey::U64(raw_ordinal)),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        evidence.record_digest,
    )?;
    if lexical_body.is_empty() {
        return Err(CodexSourceBackedErrorV0::MissingLexicalPreview);
    }
    Ok(LexicalDocument {
        event_id,
        session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(native_session_id.to_owned()),
        event_sequence: raw_ordinal,
        occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
        event_type: event_type.as_str().to_owned(),
        role: role.map(|role| role.as_str().to_owned()),
        body: lexical_body,
        workspace: None,
        cwd: cwd.map(str::to_owned),
        touched_files: touched_paths,
    })
}

fn validate_owner(
    owner: &CodexSessionRow,
    native_session_id: &str,
) -> CodexSourceBackedResultV0<()> {
    if owner.native_session_id != native_session_id {
        return Err(CodexSourceBackedErrorV0::OwnerMismatch {
            expected: native_session_id.to_owned(),
            actual: owner.native_session_id.clone(),
        });
    }
    Ok(())
}

fn decode_append_proof(
    source: &CodexCatalogSource,
    source_key: &SourceKey,
    base: &CertifiedSource,
) -> CodexSourceBackedResultV0<CodexAppendProof> {
    let frontier = base
        .frontier()
        .ok_or(CodexSourceBackedErrorV0::MissingCheckpoint)?;
    if frontier.checkpoint_kind() != CODEX_FRONTIER_KIND {
        return Err(CodexSourceBackedErrorV0::InvalidCheckpoint);
    }
    let TypedKey::Bytes(checkpoint_bytes) = frontier.checkpoint() else {
        return Err(CodexSourceBackedErrorV0::InvalidCheckpoint);
    };
    let checkpoint = CodexNativeCheckpoint::decode(checkpoint_bytes)
        .map_err(|_| CodexSourceBackedErrorV0::InvalidCheckpoint)?;
    let identity = CodexSourceIdentity::new(
        source_key.identity().to_string(),
        source.source_root.clone(),
        source.source_path.clone(),
    )?;
    Ok(CodexAppendProof::new(
        identity,
        CodexCheckpointGeneration::new(base.counts().complete_records),
        checkpoint,
    ))
}

fn certify_scan(
    source_key: &SourceKey,
    scan: &super::CodexSourceScan,
    base: Option<&CertifiedSource>,
    staged_documents: u64,
    scan_counters: CodexScanCounters,
) -> CodexSourceBackedResultV0<CertifiedSource> {
    if scan_counters.retained_records != staged_documents {
        return Err(CodexSourceBackedErrorV0::ScanCountMismatch);
    }
    let counts = cumulative_counts(base, scan, staged_documents, scan_counters)?;
    let opening = source_observation(source_key, &scan.before_observation)?;
    let closing = source_observation(source_key, &scan.after_observation)?;
    let checkpoint = scan
        .checkpoint()
        .ok_or(CodexSourceBackedErrorV0::MissingCheckpoint)?;
    let frontier = SourceFrontier::new(
        CODEX_FRONTIER_KIND,
        TypedKey::bytes(checkpoint.encode()?)?,
        scan.complete_prefix_end,
        scan.complete_prefix_sha256,
    )?;
    Ok(CertifiedSource::certify_with_frontier(
        opening,
        closing,
        CODEX_PARSER_REVISION,
        scan.complete_prefix_sha256,
        counts,
        Some(frontier),
    )?)
}

fn cumulative_counts(
    base: Option<&CertifiedSource>,
    scan: &super::CodexSourceScan,
    staged_documents: u64,
    scan_counters: CodexScanCounters,
) -> CodexSourceBackedResultV0<ScannedSourceCounts> {
    let base_counts = base.map(CertifiedSource::counts).unwrap_or_default();
    let complete_records =
        checked_add(base_counts.complete_records, scan_counters.complete_records)?;
    let retained_records =
        checked_add(base_counts.retained_records, scan_counters.retained_records)?;
    let rejected_records = checked_add(
        base_counts.rejected_records,
        scan_counters.rejected_complete_records,
    )?;
    let indexed_documents = checked_add(base_counts.indexed_documents, staged_documents)?;
    let classified = checked_add(retained_records, rejected_records)?;
    let ignored_records = complete_records
        .checked_sub(classified)
        .ok_or(CodexSourceBackedErrorV0::ScanCountMismatch)?;
    if complete_records != scan.next_raw_ordinal || indexed_documents != retained_records {
        return Err(CodexSourceBackedErrorV0::ScanCountMismatch);
    }
    Ok(ScannedSourceCounts {
        complete_records,
        retained_records,
        rejected_records,
        ignored_records,
        indexed_documents,
        certified_bytes: scan.complete_prefix_end,
    })
}

fn checked_add(left: u64, right: u64) -> CodexSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(CodexSourceBackedErrorV0::CountOverflow)
}

fn source_observation(
    source: &SourceKey,
    observation: &CodexFileObservation,
) -> CodexSourceBackedResultV0<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        CODEX_SOURCE_REVISION_KIND,
        serde_json::to_vec(observation)?,
    )?)
}

fn validate_codex_locator(
    locator: &SourceRecordLocator,
) -> CodexSourceBackedResultV0<(String, u64, u64, u64)> {
    if locator.source().provider() != CaptureProvider::Codex.as_str()
        || locator.source().source_format() != CODEX_SESSION_SOURCE_FORMAT
        || locator.source().schema_variant() != CODEX_SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
    {
        return Err(CodexSourceBackedErrorV0::InvalidCodexLocator);
    }
    let SourceAnchor::ProviderNative { namespace, key } = locator.source().anchor() else {
        return Err(CodexSourceBackedErrorV0::InvalidCodexLocator);
    };
    let TypedKey::Utf8(source_native_session_id) = key else {
        return Err(CodexSourceBackedErrorV0::InvalidCodexLocator);
    };
    if namespace != CODEX_SOURCE_ANCHOR_NAMESPACE {
        return Err(CodexSourceBackedErrorV0::InvalidCodexLocator);
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        native_event_key,
    } = locator.coordinate()
    else {
        return Err(CodexSourceBackedErrorV0::InvalidCodexLocator);
    };
    if native_session_key.as_ref() != Some(&TypedKey::Utf8(source_native_session_id.clone()))
        || native_event_key.as_ref() != Some(&TypedKey::U64(*physical_ordinal))
    {
        return Err(CodexSourceBackedErrorV0::InvalidCodexLocator);
    }
    Ok((
        source_native_session_id.clone(),
        *byte_offset,
        *byte_length,
        *physical_ordinal,
    ))
}

fn decode_exact_display_text(
    provider_bytes: &[u8],
    _physical_ordinal: u64,
) -> CodexSourceBackedResultV0<Option<String>> {
    let record = provider_bytes.strip_suffix(b"\n").unwrap_or(provider_bytes);
    let record = record.strip_suffix(b"\r").unwrap_or(record);
    let envelope: Value = serde_json::from_slice(record)?;
    let record_type = envelope.get("type").and_then(Value::as_str);
    let Some(payload) = envelope.get("payload") else {
        return Ok(None);
    };
    let item_type = payload.get("type").and_then(Value::as_str);
    let display = match (record_type, item_type) {
        (Some("response_item"), Some("message")) => {
            payload.get("content").and_then(codex_content_text)
        }
        (Some("response_item"), Some("reasoning")) => payload
            .get("summary")
            .and_then(codex_content_text)
            .or_else(|| {
                payload
                    .get("summary_text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }),
        (
            Some("response_item"),
            Some("function_call" | "custom_tool_call" | "web_search_call" | "tool_search_call"),
        ) => payload
            .get("arguments")
            .or_else(|| payload.get("input"))
            .or_else(|| payload.get("action"))
            .or_else(|| payload.get("execution"))
            .and_then(exact_typed_text),
        (
            Some("response_item"),
            Some(
                "function_call_output"
                | "custom_tool_call_output"
                | "tool_search_output"
                | "tool_output"
                | "tool_result",
            ),
        ) => payload
            .get("output")
            .or_else(|| payload.get("tools"))
            .or_else(|| payload.get("result"))
            .and_then(exact_typed_text),
        (Some("compacted"), _) => codex_content_text(payload),
        _ => None,
    };
    Ok(display)
}

fn exact_typed_text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| codex_content_text(value))
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::Write,
    };

    use ctx_history_core::{
        EventType, LocatorRevisionPolicy, NativeRecordCoordinate, SourceRecordLocator,
    };

    use super::*;

    #[test]
    fn source_backed_cold_parallel_matches_single_lane_semantics() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let single_index = temp.path().join("single-index");
        let parallel_index = temp.path().join("parallel-index");
        fs::create_dir_all(&sessions).unwrap();
        let native_session_ids = [
            "019fa000-0000-7000-8000-000000000011",
            "019fa000-0000-7000-8000-000000000012",
            "019fa000-0000-7000-8000-000000000013",
            "019fa000-0000-7000-8000-000000000014",
        ];
        for (index, native_session_id) in native_session_ids.iter().enumerate() {
            write_session(
                &sessions,
                native_session_id,
                &[
                    message(
                        "user",
                        &format!("parallel semantic sentinel source {index}"),
                    ),
                    message(
                        "assistant",
                        &format!("ordered locator sentinel source {index}"),
                    ),
                ],
            );
        }

        let single = ingest_codex_source_backed_inner_v0(
            &sessions,
            &single_index,
            ColdParallelOptionsV0 {
                scanner_workers: Some(1),
                ..ColdParallelOptionsV0::default()
            },
        )
        .unwrap();
        let parallel = ingest_codex_source_backed_inner_v0(
            &sessions,
            &parallel_index,
            ColdParallelOptionsV0 {
                scanner_workers: Some(4),
                ..ColdParallelOptionsV0::default()
            },
        )
        .unwrap();
        assert_eq!(single.counters.scanner_workers, 1);
        assert_eq!(parallel.counters.scanner_workers, 4);
        assert_eq!(single.commit.indexed_documents, 8);
        assert_eq!(parallel.commit.indexed_documents, 8);
        let mut single_counters = single.counters;
        let mut parallel_counters = parallel.counters;
        single_counters.scanner_workers = 0;
        parallel_counters.scanner_workers = 0;
        assert_eq!(single_counters, parallel_counters);

        let single_verified = VerifiedIndex::open(&single_index).unwrap();
        let parallel_verified = VerifiedIndex::open(&parallel_index).unwrap();
        assert_eq!(
            single_verified.manifest().sources,
            parallel_verified.manifest().sources
        );
        assert_eq!(
            single_verified.manifest().generation_id().unwrap(),
            parallel_verified.manifest().generation_id().unwrap()
        );
        assert_eq!(
            single_verified.document_count(),
            parallel_verified.document_count()
        );
        for native_session_id in native_session_ids {
            let source_key = codex_source_key(native_session_id).unwrap();
            let session_id = codex_session_identity(&source_key, native_session_id).unwrap();
            assert_eq!(
                single_verified
                    .events_for_session(session_id.as_uuid())
                    .unwrap(),
                parallel_verified
                    .events_for_session(session_id.as_uuid())
                    .unwrap()
            );
        }
        assert_eq!(
            search_event_ids(&single_verified, "parallel semantic sentinel"),
            search_event_ids(&parallel_verified, "parallel semantic sentinel")
        );
        assert_eq!(
            search_event_ids(&single_verified, "ordered locator sentinel"),
            search_event_ids(&parallel_verified, "ordered locator sentinel")
        );
    }

    #[test]
    fn source_backed_incremental_mixed_run_stays_serial() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let index = temp.path().join("global-index");
        fs::create_dir_all(&sessions).unwrap();
        let first_id = "019fa000-0000-7000-8000-000000000021";
        let second_id = "019fa000-0000-7000-8000-000000000022";
        let cold_id = "019fa000-0000-7000-8000-000000000023";
        write_session(
            &sessions,
            first_id,
            &[message("user", "first initial sentinel")],
        );
        write_session(
            &sessions,
            second_id,
            &[message("user", "second initial sentinel")],
        );
        let initial = ingest_codex_source_backed_inner_v0(
            &sessions,
            &index,
            ColdParallelOptionsV0 {
                scanner_workers: Some(2),
                ..ColdParallelOptionsV0::default()
            },
        )
        .unwrap();
        assert_eq!(initial.counters.scanner_workers, 2);
        assert_eq!(initial.counters.cold_sources, 2);

        let first_path = session_path(&sessions, first_id);
        OpenOptions::new()
            .append(true)
            .open(first_path)
            .unwrap()
            .write_all(format!("{}\n", message("assistant", "append sentinel")).as_bytes())
            .unwrap();
        write_session(&sessions, cold_id, &[message("user", "new cold sentinel")]);

        let mixed = ingest_codex_source_backed_inner_v0(
            &sessions,
            &index,
            ColdParallelOptionsV0 {
                scanner_workers: Some(4),
                ..ColdParallelOptionsV0::default()
            },
        )
        .unwrap();
        assert_eq!(mixed.counters.scanner_workers, 1);
        assert_eq!(mixed.counters.appended_sources, 1);
        assert_eq!(mixed.counters.replayed_sources, 1);
        assert_eq!(mixed.counters.cold_sources, 1);
        assert_eq!(mixed.counters.staged_documents, 2);

        let replay = ingest_codex_source_backed_inner_v0(
            &sessions,
            &index,
            ColdParallelOptionsV0 {
                scanner_workers: Some(4),
                ..ColdParallelOptionsV0::default()
            },
        )
        .unwrap();
        assert_eq!(replay.counters.scanner_workers, 0);
        assert_eq!(replay.counters.replayed_sources, 3);
        assert_eq!(replay.counters.staged_documents, 0);
        assert_eq!(replay.timings.scan_and_stage, Duration::ZERO);
        assert_eq!(VerifiedIndex::open(&index).unwrap().document_count(), 4);
    }

    #[test]
    fn source_backed_worker_failure_does_not_publish_a_generation() {
        let temp = tempfile::tempdir().unwrap();
        let baseline_sessions = temp.path().join("baseline-sessions");
        let failing_sessions = temp.path().join("failing-sessions");
        let index = temp.path().join("global-index");
        fs::create_dir_all(&baseline_sessions).unwrap();
        fs::create_dir_all(&failing_sessions).unwrap();
        let baseline_id = "019fa000-0000-7000-8000-000000000031";
        write_session(
            &baseline_sessions,
            baseline_id,
            &[message("user", "visible baseline sentinel")],
        );
        ingest_codex_source_backed_v0(&baseline_sessions, &index).unwrap();
        let before = VerifiedIndex::open(&index).unwrap();
        let before_generation = before.generation_id().to_owned();
        let before_sources = before.manifest().sources.clone();
        let before_events = search_event_ids(&before, "visible baseline sentinel");

        for (native_session_id, sentinel) in [
            (
                "019fa000-0000-7000-8000-000000000032",
                "uncommitted failure sentinel one",
            ),
            (
                "019fa000-0000-7000-8000-000000000033",
                "uncommitted failure sentinel two",
            ),
        ] {
            write_session(
                &failing_sessions,
                native_session_id,
                &[message("assistant", sentinel)],
            );
        }
        let error = ingest_codex_source_backed_inner_v0(
            &failing_sessions,
            &index,
            ColdParallelOptionsV0 {
                scanner_workers: Some(2),
                fail_source_index: Some(0),
                ..ColdParallelOptionsV0::default()
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            CodexSourceBackedErrorV0::InjectedColdWorkerFailure { source_index: 0 }
        ));

        let after = VerifiedIndex::open(&index).unwrap();
        assert_eq!(after.generation_id(), before_generation);
        assert_eq!(after.manifest().sources, before_sources);
        assert_eq!(after.document_count(), 1);
        assert_eq!(
            search_event_ids(&after, "visible baseline sentinel"),
            before_events
        );
        assert!(search_event_ids(&after, "uncommitted failure sentinel").is_empty());
    }

    #[test]
    fn source_backed_cold_append_and_replay_keep_cumulative_counts() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let index = temp.path().join("global-index");
        fs::create_dir_all(&sessions).unwrap();
        let native_session_id = "019fa000-0000-7000-8000-000000000001";
        let session_path = sessions.join(format!("rollout-{native_session_id}.jsonl"));
        let cold_bytes = format!(
            "{}\n{}\n",
            session_meta(native_session_id),
            message("user", "cold sentinel")
        )
        .into_bytes();
        fs::write(&session_path, &cold_bytes).unwrap();

        let cold = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
        assert_no_legacy_operations(cold.counters);
        assert_eq!(cold.counters.scanner_workers, 1);
        assert_eq!(cold.counters.cold_sources, 1);
        assert_eq!(cold.counters.staged_documents, 1);
        assert_eq!(cold.commit.indexed_documents, 1);
        let cold_index = VerifiedIndex::open(&index).unwrap();
        assert_eq!(cold_index.document_count(), 1);
        let session_id = codex_session_identity(
            &codex_source_key(native_session_id).unwrap(),
            native_session_id,
        )
        .unwrap();
        let cold_events = cold_index.events_for_session(session_id.as_uuid()).unwrap();
        let cold_event_ids = cold_events
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>();
        assert_eq!(cold_event_ids.len(), 1);
        let cold_counts = cold_index.manifest().sources[0].counts();
        assert_eq!(cold_counts.complete_records, 2);
        assert_eq!(cold_counts.retained_records, 1);
        assert_eq!(cold_counts.indexed_documents, 1);
        assert_eq!(cold_counts.certified_bytes, cold_bytes.len() as u64);

        let append_offset = cold_bytes.len() as u64;
        let appended_bytes = format!("{}\n", message("assistant", "append sentinel")).into_bytes();
        OpenOptions::new()
            .append(true)
            .open(&session_path)
            .unwrap()
            .write_all(&appended_bytes)
            .unwrap();

        let append = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
        assert_no_legacy_operations(append.counters);
        assert_eq!(append.counters.scanner_workers, 1);
        assert_eq!(append.counters.appended_sources, 1);
        assert_eq!(append.counters.staged_documents, 1);
        assert_eq!(append.counters.complete_records_scanned, 1);
        assert_eq!(append.commit.indexed_documents, 2);
        let appended_index = VerifiedIndex::open(&index).unwrap();
        assert_eq!(appended_index.document_count(), 2);
        let appended_events = appended_index
            .events_for_session(session_id.as_uuid())
            .unwrap();
        assert_eq!(
            appended_events
                .iter()
                .map(|event| event.event_id)
                .take(cold_event_ids.len())
                .collect::<Vec<_>>(),
            cold_event_ids
        );
        let appended_event_ids = appended_events
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>();
        assert_eq!(appended_event_ids.len(), 2);
        let appended_counts = appended_index.manifest().sources[0].counts();
        assert_eq!(appended_counts.complete_records, 3);
        assert_eq!(appended_counts.retained_records, 2);
        assert_eq!(appended_counts.indexed_documents, 2);
        assert_eq!(
            appended_counts.certified_bytes,
            append_offset + appended_bytes.len() as u64
        );

        let replay = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
        assert_no_legacy_operations(replay.counters);
        assert_eq!(replay.counters.scanner_workers, 0);
        assert_eq!(replay.counters.replayed_sources, 1);
        assert_eq!(replay.counters.staged_documents, 0);
        assert_eq!(replay.counters.scanner_bytes_read, 0);
        assert_eq!(replay.counters.checkpoint_validation_bytes, 0);
        assert_eq!(replay.timings.scan_and_stage, Duration::ZERO);
        assert_eq!(replay.commit.indexed_documents, 2);
        assert_eq!(replay.commit.generation_id, append.commit.generation_id);
        let replayed_index = VerifiedIndex::open(&index).unwrap();
        assert_eq!(replayed_index.document_count(), 2);
        assert_eq!(
            replayed_index
                .events_for_session(session_id.as_uuid())
                .unwrap()
                .into_iter()
                .map(|event| event.event_id)
                .collect::<Vec<_>>(),
            appended_event_ids
        );
        assert_eq!(
            replayed_index.manifest().sources[0].counts(),
            appended_counts
        );

        let source = codex_source_key(native_session_id).unwrap();
        let locator = SourceRecordLocator::new(
            source,
            NativeRecordCoordinate::Jsonl {
                byte_offset: append_offset,
                byte_length: appended_bytes.len() as u64,
                physical_ordinal: 2,
                native_session_key: Some(TypedKey::utf8(native_session_id).unwrap()),
                native_event_key: Some(TypedKey::U64(2)),
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            None,
            Sha256::digest(&appended_bytes).into(),
        )
        .unwrap();
        let hydrated = hydrate_codex_locator(&sessions, &locator).unwrap();
        assert_eq!(hydrated.provider_bytes, appended_bytes);
        assert_eq!(
            hydrated.decoded_display_text.as_deref(),
            Some("append sentinel")
        );
    }

    #[test]
    fn source_backed_rewrite_with_failed_append_proof_replaces_the_source() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let index = temp.path().join("global-index");
        fs::create_dir_all(&sessions).unwrap();
        let native_session_id = "019fa000-0000-7000-8000-000000000041";
        write_session(
            &sessions,
            native_session_id,
            &[message("user", "rewrite old sentinel")],
        );
        let cold = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
        let before = VerifiedIndex::open(&index).unwrap();
        let before_events = before
            .events_for_session(
                codex_session_identity(
                    &codex_source_key(native_session_id).unwrap(),
                    native_session_id,
                )
                .unwrap()
                .as_uuid(),
            )
            .unwrap();

        write_session(
            &sessions,
            native_session_id,
            &[
                message("assistant", "rewrite replacement sentinel"),
                message("user", "rewrite longer tail sentinel"),
            ],
        );
        let replacement = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
        assert_eq!(replacement.counters.appended_sources, 0);
        assert_eq!(replacement.counters.replaced_sources, 1);
        assert_eq!(replacement.counters.staged_documents, 2);
        assert_ne!(replacement.commit.generation_id, cold.commit.generation_id);

        let after = VerifiedIndex::open(&index).unwrap();
        assert_eq!(after.document_count(), 2);
        assert!(search_event_ids(&after, "rewrite old sentinel").is_empty());
        assert_eq!(
            search_event_ids(&after, "rewrite replacement sentinel").len(),
            1
        );
        let after_events = after
            .events_for_session(
                codex_session_identity(
                    &codex_source_key(native_session_id).unwrap(),
                    native_session_id,
                )
                .unwrap()
                .as_uuid(),
            )
            .unwrap();
        assert_eq!(after_events[0].event_id, before_events[0].event_id);
    }

    #[test]
    fn source_backed_truncation_replaces_the_source_without_stale_documents() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let index = temp.path().join("global-index");
        fs::create_dir_all(&sessions).unwrap();
        let native_session_id = "019fa000-0000-7000-8000-000000000042";
        write_session(
            &sessions,
            native_session_id,
            &[
                message("user", "truncation retained sentinel"),
                message("assistant", "truncation removed sentinel"),
            ],
        );
        ingest_codex_source_backed_v0(&sessions, &index).unwrap();

        write_session(
            &sessions,
            native_session_id,
            &[message("user", "truncation retained sentinel")],
        );
        let replacement = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
        assert_eq!(replacement.counters.appended_sources, 0);
        assert_eq!(replacement.counters.replaced_sources, 1);
        assert_eq!(replacement.counters.staged_documents, 1);

        let after = VerifiedIndex::open(&index).unwrap();
        assert_eq!(after.document_count(), 1);
        assert_eq!(
            search_event_ids(&after, "truncation retained sentinel").len(),
            1
        );
        assert!(search_event_ids(&after, "truncation removed sentinel").is_empty());
    }

    #[test]
    fn source_backed_native_session_replacement_is_one_atomic_generation() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let index = temp.path().join("global-index");
        fs::create_dir_all(&sessions).unwrap();
        let previous_id = "019fa000-0000-7000-8000-000000000047";
        let replacement_id = "019fa000-0000-7000-8000-000000000048";
        write_session(
            &sessions,
            previous_id,
            &[message("user", "native owner before replacement")],
        );
        ingest_codex_source_backed_v0(&sessions, &index).unwrap();

        fs::write(
            session_path(&sessions, previous_id),
            format!(
                "{}\n{}\n",
                session_meta(replacement_id),
                message("assistant", "native owner after replacement")
            ),
        )
        .unwrap();
        let replacement = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
        assert_eq!(replacement.counters.cold_sources, 1);
        assert_eq!(replacement.counters.deleted_sources, 1);
        assert_eq!(replacement.commit.certified_sources, 1);

        let after = VerifiedIndex::open(&index).unwrap();
        assert_eq!(after.document_count(), 1);
        assert!(search_event_ids(&after, "native owner before replacement").is_empty());
        assert_eq!(
            search_event_ids(&after, "native owner after replacement").len(),
            1
        );
        assert_eq!(
            after.manifest().sources[0].observation().source(),
            &codex_source_key(replacement_id).unwrap()
        );
    }

    #[test]
    fn source_backed_complete_inventory_certifies_deletion() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let index = temp.path().join("global-index");
        fs::create_dir_all(&sessions).unwrap();
        let native_session_id = "019fa000-0000-7000-8000-000000000043";
        write_session(
            &sessions,
            native_session_id,
            &[message("user", "certified deletion sentinel")],
        );
        let cold = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
        fs::remove_file(session_path(&sessions, native_session_id)).unwrap();

        let deletion = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
        assert_eq!(deletion.counters.deleted_sources, 1);
        assert_ne!(deletion.commit.generation_id, cold.commit.generation_id);
        let after = VerifiedIndex::open(&index).unwrap();
        assert_eq!(after.document_count(), 0);
        assert!(after.manifest().sources.is_empty());
        assert!(search_event_ids(&after, "certified deletion sentinel").is_empty());
    }

    #[test]
    fn source_backed_unavailable_root_preserves_the_prior_generation() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let unavailable = temp.path().join("sessions-unavailable");
        let index = temp.path().join("global-index");
        fs::create_dir_all(&sessions).unwrap();
        let native_session_id = "019fa000-0000-7000-8000-000000000044";
        write_session(
            &sessions,
            native_session_id,
            &[message("user", "unavailable root sentinel")],
        );
        ingest_codex_source_backed_v0(&sessions, &index).unwrap();
        let before = VerifiedIndex::open(&index).unwrap();
        let before_generation = before.generation_id().to_owned();
        fs::rename(&sessions, &unavailable).unwrap();

        assert!(ingest_codex_source_backed_v0(&sessions, &index).is_err());
        let after = VerifiedIndex::open(&index).unwrap();
        assert_eq!(after.generation_id(), before_generation);
        assert_eq!(after.document_count(), 1);
        assert_eq!(
            search_event_ids(&after, "unavailable root sentinel").len(),
            1
        );
    }

    #[test]
    fn source_backed_incomplete_inventory_preserves_the_prior_generation() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let index = temp.path().join("global-index");
        fs::create_dir_all(&sessions).unwrap();
        let native_session_id = "019fa000-0000-7000-8000-000000000049";
        write_session(
            &sessions,
            native_session_id,
            &[message("user", "incomplete inventory baseline")],
        );
        ingest_codex_source_backed_v0(&sessions, &index).unwrap();
        let before = VerifiedIndex::open(&index).unwrap();
        let before_generation = before.generation_id().to_owned();
        fs::write(
            sessions.join("duplicate-native-session.jsonl"),
            format!(
                "{}\n{}\n",
                session_meta(native_session_id),
                message("assistant", "ambiguous duplicate inventory")
            ),
        )
        .unwrap();

        let error = ingest_codex_source_backed_v0(&sessions, &index).unwrap_err();
        assert!(matches!(
            error,
            CodexSourceBackedErrorV0::DuplicateNativeSessionId(id)
                if id == native_session_id
        ));
        let after = VerifiedIndex::open(&index).unwrap();
        assert_eq!(after.generation_id(), before_generation);
        assert_eq!(after.document_count(), 1);
        assert_eq!(
            search_event_ids(&after, "incomplete inventory baseline").len(),
            1
        );
        assert!(search_event_ids(&after, "ambiguous duplicate inventory").is_empty());
    }

    #[test]
    fn source_backed_final_inventory_revalidation_blocks_partial_publication() {
        fn insert_source(session_root: &Path) {
            write_session(
                session_root,
                "019fa000-0000-7000-8000-000000000046",
                &[message("assistant", "late inventory sentinel")],
            );
        }

        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let index = temp.path().join("global-index");
        fs::create_dir_all(&sessions).unwrap();
        let baseline_id = "019fa000-0000-7000-8000-000000000045";
        write_session(
            &sessions,
            baseline_id,
            &[message("user", "inventory baseline sentinel")],
        );
        ingest_codex_source_backed_v0(&sessions, &index).unwrap();
        let before = VerifiedIndex::open(&index).unwrap();
        let before_generation = before.generation_id().to_owned();

        let error = ingest_codex_source_backed_inner_v0(
            &sessions,
            &index,
            ColdParallelOptionsV0 {
                before_commit_revalidation: Some(insert_source),
                ..ColdParallelOptionsV0::default()
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            CodexSourceBackedErrorV0::Index(IndexError::SourceInvalidated(_))
        ));

        let after = VerifiedIndex::open(&index).unwrap();
        assert_eq!(after.generation_id(), before_generation);
        assert_eq!(after.document_count(), 1);
        assert_eq!(
            search_event_ids(&after, "inventory baseline sentinel").len(),
            1
        );
        assert!(search_event_ids(&after, "late inventory sentinel").is_empty());
    }

    #[test]
    fn source_backed_projection_matches_legacy_semantics_without_legacy_operations() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let index = temp.path().join("global-index");
        fs::create_dir_all(&sessions).unwrap();
        let native_session_id = "019fa000-0000-7000-8000-000000000002";
        let session_path = sessions.join(format!("rollout-{native_session_id}.jsonl"));
        let long_message = format!(
            "long-message-sentinel {} complete-message-tail",
            "m".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 512)
        );
        let tool_record = tool_call_with_patch("touch-call");
        let failed_record = failed_tool_output("touch-call");
        fs::write(
            &session_path,
            format!(
                "{}\n{}\n{tool_record}\n{failed_record}\n",
                session_meta(native_session_id),
                message("assistant", &long_message)
            ),
        )
        .unwrap();

        let receipt = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
        assert_no_legacy_operations(receipt.counters);
        assert_eq!(receipt.counters.complete_records_scanned, 4);
        assert_eq!(receipt.counters.retained_records_scanned, 3);
        assert_eq!(receipt.counters.staged_documents, 3);
        assert_eq!(receipt.counters.structural_json_parses, 4);
        assert_eq!(receipt.counters.typed_json_parses, 3);

        let source_key = codex_source_key(native_session_id).unwrap();
        let session_id = codex_session_identity(&source_key, native_session_id).unwrap();
        let verified = VerifiedIndex::open(&index).unwrap();
        let events = verified.events_for_session(session_id.as_uuid()).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.event_sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(events[0].event_type, EventType::Message.as_str());
        assert!(events[0].preview.starts_with("long-message-sentinel"));
        assert!(
            events[0].preview.chars().count() <= super::super::rows::CODEX_LEXICAL_PREVIEW_CHARS
        );
        assert!(!events[0].preview.contains("complete-message-tail"));
        assert_eq!(events[1].event_type, EventType::ToolCall.as_str());
        assert_eq!(events[1].touched_files, vec!["src/source_backed.rs"]);
        assert_eq!(events[2].event_type, EventType::ToolOutput.as_str());
        assert_eq!(events[2].role.as_deref(), Some("tool"));
        assert!(
            events[2]
                .preview
                .starts_with("apply_patch output: exit_code=7"),
            "{}",
            events[2].preview
        );
        assert!(verified
            .search_event_candidates("long message sentinel", 10)
            .unwrap()
            .iter()
            .any(|candidate| candidate.event.event_id == events[0].event_id));
        let hydrated = hydrate_codex_locator(&sessions, &events[0].locator).unwrap();
        assert_eq!(
            hydrated.decoded_display_text.as_deref(),
            Some(long_message.as_str())
        );

        let (catalog_summary, catalog_sessions) =
            discover_codex_session_catalog(&sessions).unwrap();
        assert_eq!(catalog_summary.failed_sessions, 0);
        let discovery = super::super::discover_codex_catalog_sources(&catalog_sessions);
        assert!(discovery.rejections.is_empty());
        let source = discovery.sources.into_iter().next().unwrap();
        let mut scanner =
            CodexNativeScanner::new(source, None, super::super::CodexNativeProfile::CoreOnly)
                .unwrap();
        let mut legacy_rows = Vec::new();
        let mut legacy_counters = CodexSourceBackedCountersV0::default();
        while let Some(page) = scanner.next_page().unwrap() {
            let CodexNativeOwnedPage::Core(page) = page else {
                panic!("legacy Core-only control emitted Pro output");
            };
            assert!(page.source_backed_rows.is_empty());
            for row in page.core_rows {
                legacy_lexical_preview_for_control(&row, &mut legacy_counters);
                legacy_rows.push(row);
            }
        }
        let legacy_scan = scanner.finish().unwrap();
        legacy_counters.add_scan(legacy_scan.counters);
        assert_eq!(
            legacy_rows
                .iter()
                .map(|row| (
                    row.raw_ordinal,
                    row.provider_event.event_type.as_str(),
                    row.provider_event.role.map(|role| role.as_str()),
                    row.lexical_preview().unwrap(),
                    row.file_touches
                        .iter()
                        .map(|touch| touch.path.as_str())
                        .collect::<Vec<_>>(),
                ))
                .collect::<Vec<_>>(),
            events
                .iter()
                .map(|event| (
                    event.event_sequence,
                    event.event_type.as_str(),
                    event.role.as_deref(),
                    event.preview.clone(),
                    event
                        .touched_files
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                ))
                .collect::<Vec<_>>()
        );
        assert_eq!(legacy_counters.scanner_legacy_body_json_serializations, 3);
        assert_eq!(legacy_counters.scanner_legacy_row_json_serializations, 3);
        assert_eq!(legacy_counters.scanner_legacy_normalized_payload_hashes, 3);
        assert_eq!(legacy_counters.scanner_legacy_file_touch_rows, 1);
        assert_eq!(legacy_counters.scanner_legacy_complete_content_locators, 1);
        assert_eq!(
            legacy_counters.scanner_legacy_duplicate_preview_allocations,
            3
        );
        assert!(legacy_counters.scanner_legacy_page_owner_json_serializations > 0);
        assert!(legacy_counters.scanner_legacy_page_identity_owner_json_serializations > 0);
        assert_eq!(
            legacy_counters.scanner_legacy_page_identity_row_json_serializations,
            3
        );
    }

    fn assert_no_legacy_operations(counters: CodexSourceBackedCountersV0) {
        assert_eq!(counters.scanner_legacy_body_json_serializations, 0);
        assert_eq!(counters.scanner_legacy_row_json_serializations, 0);
        assert_eq!(counters.scanner_legacy_json_serialized_bytes, 0);
        assert_eq!(counters.scanner_legacy_normalized_payload_hashes, 0);
        assert_eq!(counters.scanner_legacy_file_touch_rows, 0);
        assert_eq!(counters.scanner_legacy_complete_content_locators, 0);
        assert_eq!(counters.scanner_legacy_duplicate_preview_allocations, 0);
        assert_eq!(counters.scanner_legacy_page_owner_json_serializations, 0);
        assert_eq!(
            counters.scanner_legacy_page_identity_owner_json_serializations,
            0
        );
        assert_eq!(
            counters.scanner_legacy_page_identity_row_json_serializations,
            0
        );
    }

    fn legacy_lexical_preview_for_control(
        row: &super::super::rows::CodexEventRow,
        counters: &mut CodexSourceBackedCountersV0,
    ) -> Option<String> {
        let preview = row.lexical_preview();
        if preview.is_some() {
            counters.scanner_legacy_duplicate_preview_allocations = counters
                .scanner_legacy_duplicate_preview_allocations
                .saturating_add(1);
        }
        preview
    }

    fn search_event_ids(index: &VerifiedIndex, query: &str) -> Vec<StableEntityId> {
        index
            .search_event_candidates(query, 32)
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.event.event_id)
            .collect()
    }

    fn session_path(sessions: &Path, native_session_id: &str) -> PathBuf {
        sessions.join(format!("rollout-{native_session_id}.jsonl"))
    }

    fn write_session(sessions: &Path, native_session_id: &str, events: &[String]) {
        let mut contents = format!("{}\n", session_meta(native_session_id));
        for event in events {
            contents.push_str(event);
            contents.push('\n');
        }
        fs::write(session_path(sessions, native_session_id), contents).unwrap();
    }

    fn session_meta(native_session_id: &str) -> String {
        serde_json::json!({
            "timestamp": "2026-07-28T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": native_session_id,
                "timestamp": "2026-07-28T12:00:00Z",
                "cwd": "/tmp/source-backed",
                "originator": "codex_cli_rs",
                "cli_version": "0.1.0",
                "source": "cli",
                "model_provider": "openai"
            }
        })
        .to_string()
    }

    fn message(role: &str, text: &str) -> String {
        serde_json::json!({
            "timestamp": "2026-07-28T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": role,
                "content": [{
                    "type": "input_text",
                    "text": text
                }]
            }
        })
        .to_string()
    }

    fn tool_call_with_patch(call_id: &str) -> String {
        serde_json::json!({
            "timestamp": "2026-07-28T12:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "name": "apply_patch",
                "call_id": call_id,
                "input": "*** Begin Patch\n*** Update File: src/source_backed.rs\n@@\n-old\n+new\n*** End Patch\n"
            }
        })
        .to_string()
    }

    fn failed_tool_output(call_id: &str) -> String {
        serde_json::json!({
            "timestamp": "2026-07-28T12:00:03Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call_output",
                "call_id": call_id,
                "output": "Process exited with code 7\nfailure body stays source-backed"
            }
        })
        .to_string()
    }
}
