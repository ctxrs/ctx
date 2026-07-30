//! Provider-local source-backed ForgeCode projection.
//!
//! Discovery and parsing remain ForgeCode-owned. Publication, replacement,
//! deletion, and projection frontiers remain shared concerns: this module
//! emits bounded lexical pages, one certified SQLite snapshot, and exact
//! native-row hydration without retaining publication state.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Mutex,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, BatchHydrationRequest, BatchHydrationResult,
    CaptureProvider, CertifiedSource, ContentSourceResolver, EventHydrationRequest,
    EventIdentityInput, HydratedProviderRecord, HydrationFailure, HydrationFailureKind,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    PositionStability, ProjectionContractError, ScannedSourceCounts, SessionHydrationRequest,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    provider::source_backed::{
        family::document::{
            ChangedDocumentSink, CompleteDocumentTree, DocumentLeafFingerprint,
            DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
        },
        route_error, SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
    },
    provider_sources::{SqliteLogicalSnapshot, SqliteSourceEvidence},
    CaptureError, ProviderAdapterContext, ProviderImportFailure, FORGECODE_SQLITE_SOURCE_FORMAT,
};

use super::super::complete_content::{
    forgecode_complete_message, forgecode_logical_record_digest, load_forgecode_conversation_values,
};
use super::source::{
    discover_forgecode_source, ForgeCodeConversationRow, ForgeCodeDiscovery, ForgeCodeFrontier,
    ForgeCodePage, ForgeCodeScanner, ForgeCodeSourceObservation, ForgeCodeSqliteDatabase,
    FORGECODE_NATIVE_PAGE_MAX_BYTES, FORGECODE_NATIVE_PARSER_REVISION,
    FORGECODE_NATIVE_POLICY_REVISION,
};

const FORGECODE_PROVIDER_ID: &str = "forgecode";
const FORGECODE_SOURCE_SCHEMA_VARIANT: &str = "conversations-messages-v1";
const FORGECODE_SELECTED_SOURCE_NAMESPACE: &str = "forgecode-selected-database-v1";
const FORGECODE_SELECTED_SOURCE_KEY: &str = "selected";
const FORGECODE_LOGICAL_SESSION_KIND: &str = "forgecode-conversation";
const FORGECODE_NATIVE_SESSION_NAMESPACE: &str = "forgecode-conversation-id-v1";
const FORGECODE_LOGICAL_EVENT_KIND: &str = "forgecode-message";
const FORGECODE_NATIVE_EVENT_POSITION_KIND: &str = "forgecode-message-index-v1";
const FORGECODE_LOCATOR_RELATION: &str = "conversations.messages";
const FORGECODE_RECORD_DIGEST_DOMAIN: &[u8] = b"ctx.forgecode.source-backed-scan-v0\0";
const FORGECODE_HYDRATION_NATIVE_KEY_BATCH: usize = 256;
const FORGECODE_SOURCE_BACKED_PARSER_REVISION: &str =
    "forgecode-nativepath-source-backed-v0:parser=1;policy=6";

#[derive(Debug, Error)]
pub(crate) enum ForgeCodeSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error("ForgeCode source-backed scan was finished before a terminal page")]
    IncompleteScan,
    #[error("ForgeCode source-backed scan lost its conversation row")]
    MissingConversationRow,
    #[error("ForgeCode source-backed scan counters overflowed")]
    CountOverflow,
    #[error("ForgeCode source-backed resolver was registered twice for one source")]
    DuplicateResolverSource,
}

pub(crate) type ForgeCodeSourceBackedResultV0<T> = Result<T, ForgeCodeSourceBackedErrorV0>;

/// Selection authority is separate from physical location.
///
/// Automatic discovery owns one logical selected slot. A nonselected database
/// is importable only with catalog lineage assigned by the explicit/manual
/// caller, so its path never becomes public identity.
#[derive(Debug, Clone)]
pub(crate) struct ForgeCodeSourceSelectionV0 {
    path: PathBuf,
    authority: ForgeCodeSourceAuthorityV0,
}

#[derive(Debug, Clone, Copy)]
enum ForgeCodeSourceAuthorityV0 {
    Selected,
    ExplicitCatalogLineage([u8; 32]),
}

impl ForgeCodeSourceSelectionV0 {
    pub(crate) fn selected(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            authority: ForgeCodeSourceAuthorityV0::Selected,
        }
    }

    pub(crate) fn explicit(path: impl Into<PathBuf>, catalog_lineage: [u8; 32]) -> Self {
        Self {
            path: path.into(),
            authority: ForgeCodeSourceAuthorityV0::ExplicitCatalogLineage(catalog_lineage),
        }
    }

    fn source_key(&self) -> ForgeCodeSourceBackedResultV0<SourceKey> {
        let anchor = match self.authority {
            ForgeCodeSourceAuthorityV0::Selected => SourceAnchor::provider_native(
                FORGECODE_SELECTED_SOURCE_NAMESPACE,
                TypedKey::utf8(FORGECODE_SELECTED_SOURCE_KEY)?,
            )?,
            ForgeCodeSourceAuthorityV0::ExplicitCatalogLineage(lineage) => {
                SourceAnchor::CatalogLineage(lineage)
            }
        };
        Ok(SourceKey::derive(
            FORGECODE_PROVIDER_ID,
            FORGECODE_SQLITE_SOURCE_FORMAT,
            FORGECODE_SOURCE_SCHEMA_VARIANT,
            1,
            anchor,
        )?)
    }
}

// Discovery transfers the live 984-byte scan directly into the source-backed
// route; boxing it to match the 24-byte missing path adds an avoidable allocation.
#[cfg(test)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum ForgeCodeSourceBackedDiscoveryV0 {
    Missing {
        // Preserve the selected missing path for fail-closed route diagnostics.
        #[allow(dead_code)]
        preferred_path: PathBuf,
    },
    Live(ForgeCodeSourceBackedScanV0),
}

#[derive(Debug, Clone)]
pub(crate) struct ForgeCodeSourceBackedSourceV0 {
    source: SourceKey,
    canonical_path: PathBuf,
}

impl ForgeCodeSourceBackedSourceV0 {
    #[cfg(test)]
    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    #[cfg(test)]
    pub(crate) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }
}

pub(crate) struct ForgeCodeSourceBackedPageV0 {
    pub(crate) documents: Vec<LexicalDocument>,
    // Rejection and page-bound accounting remain attached to emitted pages as
    // release evidence even when Core consumes only lexical documents.
    #[allow(dead_code)]
    pub(crate) failures: Vec<ProviderImportFailure>,
    #[allow(dead_code)]
    pub(crate) retained_bytes: usize,
    #[allow(dead_code)]
    pub(crate) ignored_records: u64,
    #[allow(dead_code)]
    pub(crate) terminal: bool,
}

#[cfg(test)]
pub(crate) struct ForgeCodeSourceBackedScanV0 {
    source: ForgeCodeSourceBackedSourceV0,
    schema_evidence: Vec<u8>,
    scanner: ForgeCodeScanner,
    content_digest: Sha256,
    counts: ScannedSourceCounts,
    last_observed_rowid: Option<i64>,
    terminal: bool,
}

#[cfg(test)]
pub(crate) fn open_forgecode_source_backed_v0(
    selection: ForgeCodeSourceSelectionV0,
) -> ForgeCodeSourceBackedResultV0<ForgeCodeSourceBackedDiscoveryV0> {
    let source = selection.source_key()?;
    let native_source = match discover_forgecode_source(&selection.path)? {
        ForgeCodeDiscovery::Missing => {
            return Ok(ForgeCodeSourceBackedDiscoveryV0::Missing {
                preferred_path: selection.path,
            });
        }
        ForgeCodeDiscovery::Live(source) => source,
    };
    let schema_evidence = forgecode_schema_evidence(&native_source);
    let canonical_path = native_source.canonical_path.clone();
    let context = ProviderAdapterContext {
        machine_id: "source-backed".to_owned(),
        source_path: Some(canonical_path.clone()),
        source_root: canonical_path.parent().map(Path::to_path_buf),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let scanner = ForgeCodeScanner::new(
        native_source.clone(),
        ForgeCodeFrontier::initial(),
        context,
        true,
    )?;
    let mut content_digest = Sha256::new();
    content_digest.update(FORGECODE_RECORD_DIGEST_DOMAIN);
    Ok(ForgeCodeSourceBackedDiscoveryV0::Live(
        ForgeCodeSourceBackedScanV0 {
            source: ForgeCodeSourceBackedSourceV0 {
                source,
                canonical_path,
            },
            schema_evidence,
            scanner,
            content_digest,
            counts: ScannedSourceCounts::default(),
            last_observed_rowid: None,
            terminal: false,
        },
    ))
}

#[cfg(test)]
impl ForgeCodeSourceBackedScanV0 {
    pub(crate) fn source(&self) -> &ForgeCodeSourceBackedSourceV0 {
        &self.source
    }

    #[cfg(test)]
    pub(crate) fn next_page(
        &mut self,
    ) -> ForgeCodeSourceBackedResultV0<Option<ForgeCodeSourceBackedPageV0>> {
        let Some(page) = self.scanner.next_page()? else {
            return Ok(None);
        };
        project_source_backed_page(
            &self.source,
            &mut self.content_digest,
            &mut self.counts,
            &mut self.last_observed_rowid,
            &mut self.terminal,
            page,
        )
        .map(Some)
    }

    #[cfg(test)]
    pub(crate) fn finish(self) -> ForgeCodeSourceBackedResultV0<CertifiedSource> {
        if !self.terminal {
            return Err(ForgeCodeSourceBackedErrorV0::IncompleteScan);
        }
        self.scanner.source_database().revalidate()?;
        Ok(SqliteLogicalSnapshot::new(
            FORGECODE_SOURCE_BACKED_PARSER_REVISION,
            &self.schema_evidence,
            self.content_digest.finalize().into(),
            self.counts,
        )
        .certify(self.source.source)?)
    }
}

fn project_source_backed_page(
    source: &ForgeCodeSourceBackedSourceV0,
    content_digest: &mut Sha256,
    counts: &mut ScannedSourceCounts,
    last_observed_rowid: &mut Option<i64>,
    terminal_scan: &mut bool,
    page: ForgeCodePage,
) -> ForgeCodeSourceBackedResultV0<ForgeCodeSourceBackedPageV0> {
    observe_source_record(content_digest, counts, last_observed_rowid, &page)?;
    let terminal = page.terminal;
    let retained_bytes = page.retained_bytes;
    if retained_bytes > FORGECODE_NATIVE_PAGE_MAX_BYTES {
        return Err(CaptureError::InvalidPayload(
            "ForgeCode source-backed page exceeds its retained byte bound".to_owned(),
        )
        .into());
    }
    let ignored_records = ignored_output_records(&page)?;
    let direct_touches = direct_touches(&page);
    let row = page.row.as_ref();
    let mut documents = Vec::with_capacity(page.events.len());
    let mut failures = page.rejections;
    let provider_rejections =
        u64::try_from(failures.len()).map_err(|_| ForgeCodeSourceBackedErrorV0::CountOverflow)?;
    let mut projection_rejections = 0_u64;
    for retained in page.events {
        let Some(row) = row else {
            return Err(ForgeCodeSourceBackedErrorV0::MissingConversationRow);
        };
        match lexical_document(source, row, retained, &direct_touches) {
            Ok(document) => documents.push(document),
            Err(error) => {
                projection_rejections = checked_add(projection_rejections, 1)?;
                failures.push(ProviderImportFailure {
                    line: usize::try_from(row.rowid.max(0)).unwrap_or(usize::MAX),
                    error: format!("ForgeCode source-backed projection rejected event: {error}"),
                });
            }
        }
    }
    let retained_records =
        u64::try_from(documents.len()).map_err(|_| ForgeCodeSourceBackedErrorV0::CountOverflow)?;
    let rejected_records = checked_add(provider_rejections, projection_rejections)?;
    let complete_records = checked_add(
        checked_add(retained_records, rejected_records)?,
        ignored_records,
    )?;
    counts.complete_records = checked_add(counts.complete_records, complete_records)?;
    counts.retained_records = checked_add(counts.retained_records, retained_records)?;
    counts.rejected_records = checked_add(counts.rejected_records, rejected_records)?;
    counts.ignored_records = checked_add(counts.ignored_records, ignored_records)?;
    counts.indexed_documents = checked_add(counts.indexed_documents, retained_records)?;
    *terminal_scan = terminal;
    Ok(ForgeCodeSourceBackedPageV0 {
        documents,
        failures,
        retained_bytes,
        ignored_records,
        terminal,
    })
}

fn observe_source_record(
    content_digest: &mut Sha256,
    counts: &mut ScannedSourceCounts,
    last_observed_rowid: &mut Option<i64>,
    page: &ForgeCodePage,
) -> ForgeCodeSourceBackedResultV0<()> {
    if let Some(row) = page.row.as_ref() {
        if *last_observed_rowid != Some(row.rowid) {
            content_digest.update([1]);
            content_digest.update(row.source_record_digest);
            counts.certified_bytes =
                checked_add(counts.certified_bytes, row.canonical_record_bytes)?;
            *last_observed_rowid = Some(row.rowid);
        }
        return Ok(());
    }
    let Some(rowid) = page.next_frontier.rowid else {
        return Ok(());
    };
    if *last_observed_rowid == Some(rowid) {
        return Ok(());
    }
    content_digest.update([2]);
    let mut certified_bytes = 1_u64;
    for failure in &page.rejections {
        let error_bytes = failure.error.as_bytes();
        content_digest.update((error_bytes.len() as u64).to_be_bytes());
        content_digest.update(error_bytes);
        certified_bytes = checked_add(
            certified_bytes,
            u64::try_from(error_bytes.len())
                .map_err(|_| ForgeCodeSourceBackedErrorV0::CountOverflow)?,
        )?;
    }
    counts.certified_bytes = checked_add(counts.certified_bytes, certified_bytes)?;
    *last_observed_rowid = Some(rowid);
    Ok(())
}

fn lexical_document(
    source: &ForgeCodeSourceBackedSourceV0,
    row: &ForgeCodeConversationRow,
    retained: super::source::ForgeCodeRetainedEvent,
    direct_touches: &BTreeMap<u64, Vec<String>>,
) -> ForgeCodeSourceBackedResultV0<LexicalDocument> {
    let session_id = forgecode_session_id(&source.source, &row.conversation_id)?;
    let subrecord_index = retained
        .provider_event_index
        .checked_sub(1)
        .ok_or(ProjectionContractError::InvalidDerivedIdentity)?;
    let event_id = forgecode_event_id(&source.source, session_id, subrecord_index)?;
    let primary_key = TypedKey::composite(vec![
        TypedKey::utf8(&row.conversation_id)?,
        TypedKey::U64(subrecord_index),
    ])?;
    let locator = SourceRecordLocator::new(
        source.source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: FORGECODE_LOCATOR_RELATION.to_owned(),
            primary_key,
            row_version: Some(TypedKey::bytes(row.source_record_digest.to_vec())?),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        row.source_record_digest,
    )?;
    let lexical_text = retained
        .event
        .payload
        .get("text")
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| retained.event.event_type.as_str().replace('_', " "));
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.source.clone(),
        locator,
        provider_session_id: Some(row.conversation_id.clone()),
        branch: forgecode_branch(row),
        source_path: Some(source.canonical_path.display().to_string()),
        agent_type: "primary".to_owned(),
        is_primary: true,
        event_sequence: subrecord_index,
        occurred_at_unix_ms: Some(retained.event.occurred_at.timestamp_millis()),
        event_type: retained.event.event_type.as_str().to_owned(),
        role: retained.event.role.map(|role| role.as_str().to_owned()),
        body: lexical_text,
        workspace: Some(row.workspace_id.to_string()),
        cwd: None,
        touched_files: direct_touches
            .get(&retained.provider_event_index)
            .cloned()
            .unwrap_or_default(),
    })
}

fn forgecode_branch(row: &ForgeCodeConversationRow) -> Option<String> {
    [
        "/branch",
        "/git_branch",
        "/gitBranch",
        "/repository/branch",
        "/workspace/branch",
    ]
    .into_iter()
    .find_map(|pointer| {
        row.context_metadata
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|branch| !branch.is_empty())
            .map(str::to_owned)
    })
}

fn direct_touches(page: &ForgeCodePage) -> BTreeMap<u64, Vec<String>> {
    let mut touches = BTreeMap::<u64, Vec<String>>::new();
    for touch in &page.touches {
        if let Some(event_index) = touch.provider_event_index {
            touches
                .entry(event_index)
                .or_default()
                .push(touch.path.clone());
        }
    }
    touches
}

fn ignored_output_records(page: &ForgeCodePage) -> ForgeCodeSourceBackedResultV0<u64> {
    let retained = page
        .events
        .iter()
        .filter_map(|event| event.provider_event_index.checked_sub(1))
        .filter_map(|index| u32::try_from(index).ok())
        .collect::<BTreeSet<_>>();
    page.outputs.iter().try_fold(0_u64, |count, output| {
        let retained_in_core = output
            .coordinate
            .source_record_subrecord_index
            .is_some_and(|index| retained.contains(&index));
        if retained_in_core {
            Ok(count)
        } else {
            checked_add(count, 1)
        }
    })
}

fn forgecode_session_id(
    source: &SourceKey,
    conversation_id: &str,
) -> Result<StableEntityId, ProjectionContractError> {
    let native_session_key = NativeSessionKey::native_id(
        FORGECODE_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(conversation_id)?,
    )?;
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: FORGECODE_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })
}

fn forgecode_event_id(
    source: &SourceKey,
    session_id: StableEntityId,
    subrecord_index: u64,
) -> Result<StableEntityId, ProjectionContractError> {
    let native_item_key = NativeItemKey::certified_position(
        FORGECODE_NATIVE_EVENT_POSITION_KIND,
        TypedKey::U64(subrecord_index),
        PositionStability::StableSlot,
    )?;
    derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: FORGECODE_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
}

fn forgecode_schema_evidence(native: &ForgeCodeSourceObservation) -> Vec<u8> {
    format!(
        "forgecode-logical-schema-v1;parser={FORGECODE_NATIVE_PARSER_REVISION};\
         policy={FORGECODE_NATIVE_POLICY_REVISION};user_version={};schema={}",
        native.user_version, native.schema_fingerprint
    )
    .into_bytes()
}

fn checked_add(left: u64, right: u64) -> ForgeCodeSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(ForgeCodeSourceBackedErrorV0::CountOverflow)
}

#[derive(Debug, Default)]
pub(crate) struct ForgeCodeSourceBackedResolverV0 {
    sources: Vec<ForgeCodeSourceBackedSourceV0>,
}

impl ForgeCodeSourceBackedResolverV0 {
    pub(crate) fn new(
        sources: impl IntoIterator<Item = ForgeCodeSourceBackedSourceV0>,
    ) -> ForgeCodeSourceBackedResultV0<Self> {
        let mut registered = Vec::<ForgeCodeSourceBackedSourceV0>::new();
        for source in sources {
            if registered
                .iter()
                .any(|candidate| candidate.source == source.source)
            {
                return Err(ForgeCodeSourceBackedErrorV0::DuplicateResolverSource);
            }
            registered.push(source);
        }
        Ok(Self {
            sources: registered,
        })
    }

    pub(crate) fn hydrate(
        &self,
        requests: &[EventHydrationRequest],
        expected_session_id: Option<StableEntityId>,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        let route = self
            .sources
            .iter()
            .find(|source| source.source.exact_descriptor_eq(first.locator().source()))
            .ok_or_else(|| hydration_failure(HydrationFailureKind::InvalidLocator))?;
        let mut coordinates = Vec::with_capacity(requests.len());
        for request in requests {
            request
                .locator()
                .validate_contract()
                .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
            if !route.source.exact_descriptor_eq(request.locator().source()) {
                return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
            }
            coordinates.push(decode_locator(request.locator())?);
        }
        let (_, cached_rows) = ForgeCodeSqliteDatabase::open(&route.canonical_path, |connection| {
            let mut rows = BTreeMap::new();
            for chunk in coordinates.chunks(FORGECODE_HYDRATION_NATIVE_KEY_BATCH) {
                for coordinate in chunk {
                    if rows.contains_key(&coordinate.conversation_id) {
                        continue;
                    }
                    let mut statement = connection.prepare(
                        "select rowid from conversations \
                             where cast(conversation_id as text) = ?1 collate binary limit 2",
                    )?;
                    let mut matches = statement.query([&coordinate.conversation_id])?;
                    let rowid = matches
                        .next()?
                        .ok_or(rusqlite::Error::QueryReturnedNoRows)?
                        .get(0)?;
                    if matches.next()?.is_some() {
                        return Err(CaptureError::InvalidPayload(
                            "ForgeCode conversation key is ambiguous".to_owned(),
                        ));
                    }
                    let values = load_forgecode_conversation_values(connection, rowid)?;
                    let digest = forgecode_logical_record_digest(&values);
                    rows.insert(coordinate.conversation_id.clone(), (values, digest));
                }
            }
            Ok(rows)
        })
        .map_err(|error| match error {
            CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows) => {
                hydration_failure(HydrationFailureKind::MissingRecord)
            }
            _ => hydration_failure(HydrationFailureKind::StaleRecordEvidence),
        })?;
        let mut hydrated = Vec::with_capacity(requests.len());
        for (request, coordinate) in requests.iter().zip(coordinates) {
            let (values, digest) = cached_rows
                .get(&coordinate.conversation_id)
                .ok_or_else(|| hydration_failure(HydrationFailureKind::MissingRecord))?;
            if digest != request.locator().record_digest() || coordinate.row_version != *digest {
                return Err(hydration_failure(HydrationFailureKind::StaleRecordEvidence));
            }
            let subrecord_index = u32::try_from(coordinate.subrecord_index)
                .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
            let (conversation_id, _, text) = forgecode_complete_message(values, subrecord_index)
                .map_err(|_| hydration_failure(HydrationFailureKind::MissingRecord))?;
            if conversation_id != coordinate.conversation_id {
                return Err(hydration_failure(HydrationFailureKind::StaleRecordEvidence));
            }
            let session_id = forgecode_session_id(&route.source, &conversation_id)
                .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
            if expected_session_id.is_some_and(|expected| expected != session_id) {
                return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
            }
            let event_id =
                forgecode_event_id(&route.source, session_id, coordinate.subrecord_index)
                    .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
            if request.event_id() != event_id {
                return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
            }
            hydrated.push(HydratedProviderRecord {
                event_id,
                provider_bytes: text.into_bytes(),
            });
        }
        Ok(hydrated)
    }
}

impl ContentSourceResolver for ForgeCodeSourceBackedResolverV0 {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        self.hydrate(std::slice::from_ref(request), None)?
            .pop()
            .ok_or_else(|| hydration_failure(HydrationFailureKind::MissingRecord))
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        self.hydrate(request.events(), Some(request.session_id()))
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let records = self.hydrate(request.events(), None)?;
        let result = BatchHydrationResult::new(records).map_err(|error| HydrationFailure {
            kind: HydrationFailureKind::InvalidLocator,
            detail: error.to_string(),
        })?;
        result.validate_for_request(request)?;
        Ok(result)
    }
}

struct ForgeCodeLocatorCoordinate {
    conversation_id: String,
    subrecord_index: u64,
    row_version: [u8; 32],
}

fn decode_locator(
    locator: &SourceRecordLocator,
) -> Result<ForgeCodeLocatorCoordinate, HydrationFailure> {
    if locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    }
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    if logical_relation != FORGECODE_LOCATOR_RELATION {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    }
    let TypedKey::Composite(parts) = primary_key else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    let [TypedKey::Utf8(conversation_id), TypedKey::U64(subrecord_index)] = parts.as_slice() else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    let Some(TypedKey::Bytes(row_version)) = row_version.as_ref() else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    let row_version: [u8; 32] = row_version
        .as_slice()
        .try_into()
        .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
    Ok(ForgeCodeLocatorCoordinate {
        conversation_id: conversation_id.clone(),
        subrecord_index: *subrecord_index,
        row_version,
    })
}

fn hydration_failure(kind: HydrationFailureKind) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: "ForgeCode source-backed native hydration failed".to_owned(),
    }
}

pub(crate) struct ForgeCodeTreeAuthority {
    native: ForgeCodeSourceObservation,
    fence: Mutex<Option<SqliteSourceEvidence>>,
}

impl ReplacementDocumentTree for ForgeCodeSourceSelectionV0 {
    type Leaf = ForgeCodeSourceBackedSourceV0;
    type TreeAuthority = ForgeCodeTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        FORGECODE_SOURCE_BACKED_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        source.provider() == CaptureProvider::ForgeCode.as_str()
            && source.source_format() == FORGECODE_SQLITE_SOURCE_FORMAT
            && source.schema_variant() == FORGECODE_SOURCE_SCHEMA_VARIANT
            && source.provider_identity_version() == 1
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let native = match discover_forgecode_source(&self.path).map_err(route_error)? {
            ForgeCodeDiscovery::Live(native) => native,
            ForgeCodeDiscovery::Missing => {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unavailable,
                    "selected ForgeCode database is missing",
                ));
            }
        };
        let fingerprint = DocumentLeafFingerprint::new(*native.database.evidence().revision());
        let leaf = ForgeCodeSourceBackedSourceV0 {
            source: self.source_key().map_err(route_error)?,
            canonical_path: native.canonical_path.clone(),
        };
        Ok(CompleteDocumentTree::new(
            fingerprint.as_bytes(),
            vec![ObservedDocumentLeaf::with_durable_replay(
                fingerprint,
                leaf,
                false,
            )],
            ForgeCodeTreeAuthority {
                native,
                fence: Mutex::new(None),
            },
        ))
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        sink.begin_source(leaf.source.clone())?;
        let context = ProviderAdapterContext {
            machine_id: "source-backed".to_owned(),
            source_path: Some(leaf.canonical_path.clone()),
            source_root: leaf.canonical_path.parent().map(Path::to_path_buf),
            imported_at: DateTime::<Utc>::UNIX_EPOCH,
        };
        let mut scanner = ForgeCodeScanner::new(
            authority.native.clone(),
            ForgeCodeFrontier::initial(),
            context,
            true,
        )
        .map_err(route_error)?;
        let mut content_digest = Sha256::new();
        content_digest.update(FORGECODE_RECORD_DIGEST_DOMAIN);
        let mut counts = ScannedSourceCounts::default();
        let mut last_observed_rowid = None;
        let mut terminal = false;
        let mut emitted_pages = 0_u64;
        let mut peak_buffered_documents = 0_u64;
        let mut sink_error = None;
        let streamed = scanner.stream_pages(|page| {
            let page = project_source_backed_page(
                leaf,
                &mut content_digest,
                &mut counts,
                &mut last_observed_rowid,
                &mut terminal,
                page,
            )
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
            peak_buffered_documents = peak_buffered_documents.max(
                u64::try_from(page.documents.len())
                    .map_err(|_| CaptureError::SystemInvariant("ForgeCode page count overflow"))?,
            );
            if !page.documents.is_empty() {
                emitted_pages =
                    emitted_pages
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "ForgeCode emitted-page count overflow",
                        ))?;
            }
            for document in page.documents {
                if let Err(error) = sink.emit_document(document) {
                    let detail = error.to_string();
                    sink_error = Some(error);
                    return Err(CaptureError::InvalidPayload(detail));
                }
            }
            Ok(())
        });
        if let Some(error) = sink_error {
            return Err(error);
        }
        streamed.map_err(route_error)?;
        if !terminal {
            return Err(route_error(ForgeCodeSourceBackedErrorV0::IncompleteScan));
        }
        let decoded_rows = scanner.decoded_rows();
        if decoded_rows > counts.complete_records
            || peak_buffered_documents > 64
            || (counts.indexed_documents == 0) != (emitted_pages == 0)
        {
            return Err(forgecode_internal(
                "ForgeCode scan violated its one-pass bounded-page receipt",
            ));
        }
        scanner
            .source_database()
            .revalidate()
            .map_err(route_error)?;
        let logical = SqliteLogicalSnapshot::new(
            FORGECODE_SOURCE_BACKED_PARSER_REVISION,
            &forgecode_schema_evidence(&authority.native),
            content_digest.finalize().into(),
            counts,
        );
        let certificate = logical.certify(leaf.source.clone()).map_err(route_error)?;
        *authority
            .fence
            .lock()
            .map_err(|_| forgecode_internal("ForgeCode terminal fence lock was poisoned"))? =
            Some(authority.native.database.evidence().clone());
        Ok(forgecode_document_terminal(certificate))
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        let expected = tree
            .authority
            .fence
            .lock()
            .map_err(|_| forgecode_internal("ForgeCode terminal fence lock was poisoned"))?
            .clone()
            .ok_or_else(|| forgecode_changed("ForgeCode scan has no terminal fence"))?;
        let (current, ()) =
            ForgeCodeSqliteDatabase::open(&tree.authority.native.canonical_path, |_| Ok(()))
                .map_err(route_error)?;
        if current.evidence() == &expected {
            Ok(tree.tree_fingerprint)
        } else {
            Err(forgecode_changed(
                "ForgeCode physical source changed before commit",
            ))
        }
    }

    fn hydrate_group(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let source = ForgeCodeSourceBackedSourceV0 {
            source: self
                .source_key()
                .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?,
            canonical_path: if self.path.is_dir() {
                self.path.join(".forge.db")
            } else {
                self.path.clone()
            },
        };
        let resolver = ForgeCodeSourceBackedResolverV0::new([source])
            .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
        resolver.hydrate_batch(request)
    }
}

fn forgecode_document_terminal(certificate: CertifiedSource) -> DocumentSourceTerminal {
    DocumentSourceTerminal {
        source: certificate.observation().source().clone(),
        opening: certificate.observation().clone(),
        closing: certificate.observation().clone(),
        parser_revision: FORGECODE_SOURCE_BACKED_PARSER_REVISION,
        content_digest: *certificate.content_digest(),
        counts: certificate.counts(),
    }
}

fn forgecode_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn forgecode_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

#[cfg(test)]
mod tests;
