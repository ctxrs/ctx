use std::{
    collections::{HashMap, HashSet},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceAppend,
    EventIdentityInput, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, PositionStability, ProjectionContractError, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceFrontier, SourceKey, SourceObservation,
    SourceRecordLocator, SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::{
    CommitReceipt, GenerationWriter, IndexError, LexicalDocument, RevalidationTarget,
    VerifiedIndex, WriterOptions,
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
const MAX_HYDRATED_CODEX_RECORD_BYTES: u64 = 16 * 1024 * 1024 + 1;

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
    pub replayed_sources: u64,
    pub staged_documents: u64,
    pub complete_records_scanned: u64,
    pub retained_records_scanned: u64,
    pub rejected_records_scanned: u64,
    pub ignored_records_scanned: u64,
    pub scanner_bytes_read: u64,
    pub checkpoint_validation_bytes: u64,
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
    let total_started = Instant::now();
    let session_root = session_root.as_ref();
    let global_index_root = global_index_root.as_ref();
    let mut timings = CodexSourceBackedPhaseTimingsV0::default();
    let mut counters = CodexSourceBackedCountersV0::default();

    let phase_started = Instant::now();
    let (catalog_summary, sessions) = discover_codex_session_catalog(session_root)?;
    let discovery = super::discover_codex_catalog_sources(&sessions);
    if catalog_summary.failed_sessions != 0 || !discovery.rejections.is_empty() {
        return Err(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: discovery.rejections.len(),
            failed: catalog_summary.failed_sessions,
        });
    }
    counters.catalog_sources = u64::try_from(discovery.sources.len())
        .map_err(|_| CodexSourceBackedErrorV0::CountOverflow)?;
    counters.catalog_source_bytes = catalog_summary.source_bytes;
    let sources = bind_source_keys(discovery.sources)?;
    let base_sources = load_base_sources(global_index_root)?;
    timings.discovery = phase_started.elapsed();

    let phase_started = Instant::now();
    let mut writer = GenerationWriter::open(global_index_root, WriterOptions::default())?;
    timings.writer_open = phase_started.elapsed();
    let mut revalidation = HashMap::<SourceKey, (CodexCatalogSource, CodexFileObservation)>::new();

    for (source, source_key, native_session_id) in sources {
        let base = base_sources.get(&source_key).cloned();
        let proof = match base.as_ref() {
            Some(base) => {
                if !base.observation().source().exact_descriptor_eq(&source_key) {
                    return Err(CodexSourceBackedErrorV0::UnsupportedLifecycle(
                        native_session_id,
                    ));
                }
                Some(decode_append_proof(&source, &source_key, base)?)
            }
            None => None,
        };

        match base.as_ref() {
            Some(_) => {
                let writer_base = writer.begin_source_append(source_key.clone())?;
                if writer_base
                    != base
                        .as_ref()
                        .ok_or(CodexSourceBackedErrorV0::MissingCheckpoint)?
                {
                    return Err(CodexSourceBackedErrorV0::InvalidCheckpoint);
                }
            }
            None => writer.begin_source(source_key.clone())?,
        }

        // An unchanged strong file observation (identity + ctime-backed change
        // token + length + mtime) means the already-certified generation is
        // still the provider source. Rehashing every byte here made a no-op
        // refresh O(total history bytes). Final commit revalidation repeats
        // the observation before publishing.
        if let (Some(base), Some(proof)) = (base.as_ref(), proof.as_ref()) {
            if proof.checkpoint.observation == source.catalog_observation {
                let certification_started = Instant::now();
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
        let mut scanner = CodexNativeScanner::new_source_backed_v0(source.clone(), proof.as_ref())?;
        let session_id = codex_session_identity(&source_key, &native_session_id)?;
        let mut staged_for_source = 0_u64;
        while let Some(page) = scanner.next_page()? {
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
                writer.add_document(codex_lexical_document(
                    &source_key,
                    session_id,
                    &native_session_id,
                    cwd.as_deref(),
                    row,
                )?)?;
                staged_for_source = staged_for_source
                    .checked_add(1)
                    .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
            }
        }
        let scan = scanner.finish()?;
        timings.scan_and_stage += scan_started.elapsed();
        let scan_counters = scan.counters;
        counters.add_scan(scan_counters);
        counters.staged_documents = counters
            .staged_documents
            .checked_add(staged_for_source)
            .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;

        let certification_started = Instant::now();
        match (base.as_ref(), scan.disposition) {
            (None, CodexParseDisposition::FullGeneration) => {
                let current =
                    certify_scan(&source_key, &scan, None, staged_for_source, scan_counters)?;
                writer.certify_source(current)?;
                counters.cold_sources = counters.cold_sources.saturating_add(1);
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
            (Some(base), CodexParseDisposition::ObservationReplay) => {
                if staged_for_source != 0 {
                    return Err(CodexSourceBackedErrorV0::ScanCountMismatch);
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
                counters.replayed_sources = counters.replayed_sources.saturating_add(1);
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

    let commit_started = Instant::now();
    let commit = writer.commit(|target| match target {
        RevalidationTarget::Source(certificate) => revalidation
            .get_key_value(certificate.observation().source())
            .is_some_and(|(source_key, (source, observation))| {
                source_key.exact_descriptor_eq(certificate.observation().source())
                    && revalidate_codex_source_observation(source, observation).is_ok()
            }),
        RevalidationTarget::Deletion(_) => false,
    })?;
    timings.commit = commit_started.elapsed();
    timings.total = total_started.elapsed();
    Ok(CodexSourceBackedIngestReceiptV0 {
        commit,
        timings,
        counters,
    })
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

fn load_base_sources(
    global_index_root: &Path,
) -> CodexSourceBackedResultV0<HashMap<SourceKey, CertifiedSource>> {
    let meta_path = global_index_root.join("meta.json");
    if !meta_path.is_file() {
        return Ok(HashMap::new());
    }
    let verified = VerifiedIndex::open(global_index_root)?;
    Ok(verified
        .manifest()
        .sources
        .iter()
        .cloned()
        .map(|source| (source.observation().source().clone(), source))
        .collect())
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
        assert_eq!(replay.counters.replayed_sources, 1);
        assert_eq!(replay.counters.staged_documents, 0);
        assert_eq!(replay.counters.scanner_bytes_read, 0);
        assert_eq!(replay.counters.checkpoint_validation_bytes, 0);
        assert_eq!(replay.timings.scan_and_stage, Duration::ZERO);
        assert_eq!(replay.commit.indexed_documents, 2);
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
