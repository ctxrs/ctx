//! Provider-local source-backed ForgeCode projection.
//!
//! Discovery and parsing remain ForgeCode-owned. Publication, replacement,
//! deletion, and projection frontiers remain shared concerns: this module
//! emits bounded lexical pages, one certified SQLite snapshot, and exact
//! native-row hydration without retaining publication state.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, ContentSourceResolver,
    EventHydrationRequest, EventIdentityInput, HydratedProviderRecord, HydrationFailure,
    HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, PositionStability, ProjectionContractError, ScannedSourceCounts,
    SessionHydrationRequest, SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation,
    SourceRecordLocator, SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::{LexicalDocument, MAX_BODY_PREVIEW_CHARS};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    provider::normalization::provider_local_preview, CaptureError, ProviderAdapterContext,
    ProviderImportFailure, FORGECODE_SQLITE_SOURCE_FORMAT,
};

use super::super::complete_content::{
    forgecode_complete_message, forgecode_logical_record_digest, load_forgecode_conversation_values,
};
use super::source::{
    discover_forgecode_source, ForgeCodeConversationRow, ForgeCodeDiscovery, ForgeCodeFrontier,
    ForgeCodePage, ForgeCodeScanner, ForgeCodeSourceObservation, FORGECODE_NATIVE_PAGE_MAX_BYTES,
    FORGECODE_NATIVE_PARSER_REVISION, FORGECODE_NATIVE_POLICY_REVISION,
};

const FORGECODE_PROVIDER_ID: &str = "forgecode";
const FORGECODE_SOURCE_SCHEMA_VARIANT: &str = "conversations-messages-v1";
const FORGECODE_SELECTED_SOURCE_NAMESPACE: &str = "forgecode-selected-database-v1";
const FORGECODE_SELECTED_SOURCE_KEY: &str = "selected";
const FORGECODE_SOURCE_REVISION_KIND: &str = "forgecode-sqlite-snapshot-v1";
const FORGECODE_LOGICAL_SESSION_KIND: &str = "forgecode-conversation";
const FORGECODE_NATIVE_SESSION_NAMESPACE: &str = "forgecode-conversation-id-v1";
const FORGECODE_LOGICAL_EVENT_KIND: &str = "forgecode-message";
const FORGECODE_NATIVE_EVENT_POSITION_KIND: &str = "forgecode-message-index-v1";
const FORGECODE_LOCATOR_RELATION: &str = "conversations.messages";
const FORGECODE_RECORD_DIGEST_DOMAIN: &[u8] = b"ctx.forgecode.source-backed-scan-v0\0";

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

pub(crate) enum ForgeCodeSourceBackedDiscoveryV0 {
    Missing { preferred_path: PathBuf },
    Live(ForgeCodeSourceBackedScanV0),
}

#[derive(Debug, Clone)]
pub(crate) struct ForgeCodeSourceBackedSourceV0 {
    source: SourceKey,
    canonical_path: PathBuf,
}

impl ForgeCodeSourceBackedSourceV0 {
    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }
}

pub(crate) struct ForgeCodeSourceBackedPageV0 {
    pub(crate) documents: Vec<LexicalDocument>,
    pub(crate) failures: Vec<ProviderImportFailure>,
    pub(crate) retained_bytes: usize,
    pub(crate) ignored_records: u64,
    pub(crate) terminal: bool,
}

pub(crate) struct ForgeCodeSourceBackedScanV0 {
    source: ForgeCodeSourceBackedSourceV0,
    opening: SourceObservation,
    scanner: ForgeCodeScanner,
    content_digest: Sha256,
    counts: ScannedSourceCounts,
    last_observed_rowid: Option<i64>,
    terminal: bool,
}

pub(crate) fn open_forgecode_source_backed_v0(
    selection: ForgeCodeSourceSelectionV0,
) -> ForgeCodeSourceBackedResultV0<ForgeCodeSourceBackedDiscoveryV0> {
    let source = selection.source_key()?;
    let native_source = match discover_forgecode_source(&selection.path)? {
        ForgeCodeDiscovery::Missing(missing) => {
            return Ok(ForgeCodeSourceBackedDiscoveryV0::Missing {
                preferred_path: missing.preferred_path,
            });
        }
        ForgeCodeDiscovery::Live(source) => source,
    };
    let opening = source_observation(&source, &native_source)?;
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
            opening,
            scanner,
            content_digest,
            counts: ScannedSourceCounts::default(),
            last_observed_rowid: None,
            terminal: false,
        },
    ))
}

impl ForgeCodeSourceBackedScanV0 {
    pub(crate) fn source(&self) -> &ForgeCodeSourceBackedSourceV0 {
        &self.source
    }

    pub(crate) fn next_page(
        &mut self,
    ) -> ForgeCodeSourceBackedResultV0<Option<ForgeCodeSourceBackedPageV0>> {
        let Some(page) = self.scanner.next_page()? else {
            return Ok(None);
        };
        self.observe_source_record(&page)?;
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
        let provider_rejections = u64::try_from(failures.len())
            .map_err(|_| ForgeCodeSourceBackedErrorV0::CountOverflow)?;
        let mut projection_rejections = 0_u64;
        for retained in page.events {
            let Some(row) = row else {
                return Err(ForgeCodeSourceBackedErrorV0::MissingConversationRow);
            };
            match lexical_document(&self.source, &self.opening, row, retained, &direct_touches) {
                Ok(document) => documents.push(document),
                Err(error) => {
                    projection_rejections = checked_add(projection_rejections, 1)?;
                    failures.push(ProviderImportFailure {
                        line: usize::try_from(row.rowid.max(0)).unwrap_or(usize::MAX),
                        error: format!(
                            "ForgeCode source-backed projection rejected event: {error}"
                        ),
                    });
                }
            }
        }
        let retained_records = u64::try_from(documents.len())
            .map_err(|_| ForgeCodeSourceBackedErrorV0::CountOverflow)?;
        let rejected_records = checked_add(provider_rejections, projection_rejections)?;
        let complete_records = checked_add(
            checked_add(retained_records, rejected_records)?,
            ignored_records,
        )?;
        self.counts.complete_records = checked_add(self.counts.complete_records, complete_records)?;
        self.counts.retained_records = checked_add(self.counts.retained_records, retained_records)?;
        self.counts.rejected_records = checked_add(self.counts.rejected_records, rejected_records)?;
        self.counts.ignored_records = checked_add(self.counts.ignored_records, ignored_records)?;
        self.counts.indexed_documents =
            checked_add(self.counts.indexed_documents, retained_records)?;
        self.terminal = terminal;
        Ok(Some(ForgeCodeSourceBackedPageV0 {
            documents,
            failures,
            retained_bytes,
            ignored_records,
            terminal,
        }))
    }

    pub(crate) fn finish(self) -> ForgeCodeSourceBackedResultV0<CertifiedSource> {
        if !self.terminal {
            return Err(ForgeCodeSourceBackedErrorV0::IncompleteScan);
        }
        let closing_native = match discover_forgecode_source(&self.source.canonical_path)? {
            ForgeCodeDiscovery::Live(source) => source,
            ForgeCodeDiscovery::Missing(_) => {
                return Err(CaptureError::SourceChangedDuringCapture.into());
            }
        };
        let closing = source_observation(&self.source.source, &closing_native)?;
        let parser_revision = format!(
            "forgecode-nativepath-source-backed-v0:parser={FORGECODE_NATIVE_PARSER_REVISION};policy={FORGECODE_NATIVE_POLICY_REVISION}"
        );
        Ok(CertifiedSource::certify(
            self.opening,
            closing,
            parser_revision,
            self.content_digest.finalize().into(),
            self.counts,
        )?)
    }

    fn observe_source_record(&mut self, page: &ForgeCodePage) -> ForgeCodeSourceBackedResultV0<()> {
        if let Some(row) = page.row.as_ref() {
            if self.last_observed_rowid != Some(row.rowid) {
                self.content_digest.update([1]);
                self.content_digest.update(row.rowid.to_be_bytes());
                self.content_digest.update(row.source_record_digest);
                self.counts.certified_bytes =
                    checked_add(self.counts.certified_bytes, row.canonical_record_bytes)?;
                self.last_observed_rowid = Some(row.rowid);
            }
            return Ok(());
        }
        let Some(rowid) = page.next_frontier.rowid else {
            return Ok(());
        };
        if self.last_observed_rowid == Some(rowid) {
            return Ok(());
        }
        self.content_digest.update([2]);
        self.content_digest.update(rowid.to_be_bytes());
        let mut certified_bytes = 9_u64;
        for failure in &page.rejections {
            let error_bytes = failure.error.as_bytes();
            self.content_digest
                .update((error_bytes.len() as u64).to_be_bytes());
            self.content_digest.update(error_bytes);
            certified_bytes = checked_add(
                certified_bytes,
                u64::try_from(error_bytes.len())
                    .map_err(|_| ForgeCodeSourceBackedErrorV0::CountOverflow)?,
            )?;
        }
        self.counts.certified_bytes = checked_add(self.counts.certified_bytes, certified_bytes)?;
        self.last_observed_rowid = Some(rowid);
        Ok(())
    }
}

fn lexical_document(
    source: &ForgeCodeSourceBackedSourceV0,
    opening: &SourceObservation,
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
        TypedKey::I64(row.rowid),
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
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(source_revision_digest(opening)),
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
    let (body, _) = provider_local_preview(&lexical_text, MAX_BODY_PREVIEW_CHARS);
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
        body,
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

fn source_observation(
    source: &SourceKey,
    native: &ForgeCodeSourceObservation,
) -> Result<SourceObservation, ProjectionContractError> {
    let mut digest = Sha256::new();
    digest.update(b"ctx.forgecode.sqlite-snapshot-v1\0");
    digest.update(native.database.evidence().identity());
    digest.update(native.database.evidence().length().to_be_bytes());
    digest.update(native.database.evidence().revision());
    digest.update(native.schema_fingerprint.as_bytes());
    digest.update(native.user_version.to_be_bytes());
    SourceObservation::new(
        source.clone(),
        FORGECODE_SOURCE_REVISION_KIND,
        digest.finalize().to_vec(),
    )
}

fn source_revision_digest(observation: &SourceObservation) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.source-revision-evidence\0");
    digest.update((observation.revision_kind().len() as u64).to_be_bytes());
    digest.update(observation.revision_kind().as_bytes());
    digest.update((observation.revision().len() as u64).to_be_bytes());
    digest.update(observation.revision());
    digest.finalize().into()
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

    fn hydrate(
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
        let current = match discover_forgecode_source(&route.canonical_path) {
            Ok(ForgeCodeDiscovery::Live(source)) => source,
            Ok(ForgeCodeDiscovery::Missing(_)) => {
                return Err(hydration_failure(HydrationFailureKind::ConfirmedDeleted));
            }
            Err(_) => {
                return Err(hydration_failure(
                    HydrationFailureKind::TemporarilyUnavailable,
                ));
            }
        };
        let current_observation = source_observation(&route.source, &current)
            .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
        let expected_revision = source_revision_digest(&current_observation);
        let mut coordinates = Vec::with_capacity(requests.len());
        for request in requests {
            if !route.source.exact_descriptor_eq(request.locator().source()) {
                return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
            }
            if request.locator().certified_source_revision_digest() != Some(&expected_revision) {
                return Err(hydration_failure(HydrationFailureKind::StaleSourceEvidence));
            }
            coordinates.push(decode_locator(request.locator())?);
        }
        let cached_rows = current
            .database
            .read(&route.canonical_path, |connection| {
                let mut rows = BTreeMap::new();
                for coordinate in &coordinates {
                    if rows.contains_key(&coordinate.rowid) {
                        continue;
                    }
                    let values = load_forgecode_conversation_values(connection, coordinate.rowid)?;
                    let digest = forgecode_logical_record_digest(&values);
                    rows.insert(coordinate.rowid, (values, digest));
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
                .get(&coordinate.rowid)
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
}

struct ForgeCodeLocatorCoordinate {
    rowid: i64,
    conversation_id: String,
    subrecord_index: u64,
    row_version: [u8; 32],
}

fn decode_locator(
    locator: &SourceRecordLocator,
) -> Result<ForgeCodeLocatorCoordinate, HydrationFailure> {
    if locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision {
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
    let [TypedKey::I64(rowid), TypedKey::Utf8(conversation_id), TypedKey::U64(subrecord_index)] =
        parts.as_slice()
    else {
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
        rowid: *rowid,
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

#[cfg(test)]
mod tests;
