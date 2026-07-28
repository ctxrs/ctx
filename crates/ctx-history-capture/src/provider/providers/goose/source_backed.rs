use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, ContentSourceResolver,
    EventHydrationRequest, EventIdentityInput, EventType, HydratedProviderRecord, HydrationFailure,
    HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ProjectionContractError, ScannedSourceCounts, SessionHydrationRequest,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::{LexicalDocument, MAX_BODY_PREVIEW_CHARS};
use thiserror::Error;

use super::{
    content::{goose_logical_row_digest, load_message_values},
    native_path::{
        GooseNativePage, GooseNativePathReader, GooseNativeProfile, GooseNativeScanner,
        GooseNativeSourceSelection,
    },
    normalization::{
        goose_complete_content_text, goose_timestamp, normalize_goose_native_message,
        normalize_goose_native_output_diagnostic, GooseNativeEvent, GooseNativeEventKind,
    },
    position::{decode_goose_message_locator, goose_message_locator, GooseNativeRowKeyset},
    stream::{
        goose_fetch_native_message_page, goose_prepare_native_identity_index,
        GooseMessageCellDisposition, GooseNativePageLimits, GooseScannedMessage,
    },
};
use crate::{
    provider::normalization::{provider_local_preview, provider_role, provider_timestamp_seconds},
    CaptureError, GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
};

const GOOSE_SOURCE_ANCHOR_NAMESPACE: &str = "goose.installed-sessions";
const GOOSE_SOURCE_ANCHOR_KEY: &str = "selected-platform-sessions-db";
const GOOSE_SOURCE_SCHEMA_VARIANT: &str = "goose-sessions-sqlite-v0";
const GOOSE_SOURCE_REVISION_KIND: &str = "goose-private-sqlite-snapshot-v1";
const GOOSE_PARSER_REVISION: &str = "goose-nativepath-source-backed-v0";
const GOOSE_NATIVE_SESSION_NAMESPACE: &str = "goose.session";
const GOOSE_NATIVE_EVENT_NAMESPACE: &str = "goose.message";
const GOOSE_LOGICAL_SESSION_KIND: &str = "goose-session";
const GOOSE_LOGICAL_EVENT_KIND: &str = "goose-event";
const GOOSE_LOGICAL_RELATION: &str = "goose-logical-row-v3";
const GOOSE_AGENT_TYPE: &str = "goose";
const GOOSE_MAX_EXPLICIT_RETAINED_ROUTES: usize = 32;

#[derive(Debug, Error)]
pub(crate) enum GooseSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error("Goose source-backed count overflow")]
    CountOverflow,
    #[error("Goose source-backed projection emitted an empty lexical preview")]
    EmptyLexicalPreview,
    #[error("Goose source-backed selection has too many explicit retained routes")]
    TooManyRetainedRoutes,
    #[error("Goose source-backed selection contains a duplicate database route")]
    DuplicateDatabaseRoute,
    #[error("Goose source-backed scan must be exhausted before certification")]
    IncompleteScan,
}

pub(crate) type GooseSourceBackedResultV0<T> = Result<T, GooseSourceBackedErrorV0>;

/// One caller-selected Goose installation route.
///
/// Both paths retain their caller-provided spelling. The adapter never
/// discovers alternates or treats a historical platform root as current.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GooseSourceRouteV0 {
    selected_database: PathBuf,
    platform_root: PathBuf,
}

impl GooseSourceRouteV0 {
    pub(crate) fn exact(
        selected_database: impl Into<PathBuf>,
        platform_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            selected_database: selected_database.into(),
            platform_root: platform_root.into(),
        }
    }

    pub(crate) fn selected_database(&self) -> &Path {
        &self.selected_database
    }

    pub(crate) fn platform_root(&self) -> &Path {
        &self.platform_root
    }
}

/// Exact selected route plus explicitly retained historical routes.
///
/// Retained routes are resolver-only. They are never scanned as current and
/// are never inferred from the filesystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GooseSourceBackedSelectionV0 {
    selected: GooseSourceRouteV0,
    retained: Vec<GooseSourceRouteV0>,
}

impl GooseSourceBackedSelectionV0 {
    pub(crate) fn exact(
        selected_database: impl Into<PathBuf>,
        platform_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            selected: GooseSourceRouteV0::exact(selected_database, platform_root),
            retained: Vec::new(),
        }
    }

    pub(crate) fn with_explicit_retained_routes(
        mut self,
        retained: Vec<GooseSourceRouteV0>,
    ) -> GooseSourceBackedResultV0<Self> {
        if retained.len() > GOOSE_MAX_EXPLICIT_RETAINED_ROUTES {
            return Err(GooseSourceBackedErrorV0::TooManyRetainedRoutes);
        }
        let mut databases = Vec::with_capacity(retained.len().saturating_add(1));
        databases.push(self.selected.selected_database.clone());
        databases.extend(retained.iter().map(|route| route.selected_database.clone()));
        databases.sort();
        if databases.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(GooseSourceBackedErrorV0::DuplicateDatabaseRoute);
        }
        self.retained = retained;
        Ok(self)
    }

    pub(crate) fn selected(&self) -> &GooseSourceRouteV0 {
        &self.selected
    }

    pub(crate) fn retained(&self) -> &[GooseSourceRouteV0] {
        &self.retained
    }

    fn routes(&self) -> impl Iterator<Item = &GooseSourceRouteV0> {
        std::iter::once(&self.selected).chain(self.retained.iter())
    }
}

pub(crate) struct GooseSourceBackedAdapterV0 {
    selection: GooseSourceBackedSelectionV0,
    reader: GooseNativePathReader,
    source: SourceKey,
    opening: SourceObservation,
    revision_digest: [u8; 32],
    certified_bytes: u64,
}

impl GooseSourceBackedAdapterV0 {
    pub(crate) fn open(selection: GooseSourceBackedSelectionV0) -> GooseSourceBackedResultV0<Self> {
        let reader = GooseNativePathReader::acquire(GooseNativeSourceSelection::exact(
            selection.selected.selected_database.clone(),
        ))?;
        let source = goose_source_key()?;
        let revision_digest = reader.source_observation().generation_digest_bytes();
        let opening = SourceObservation::new(
            source.clone(),
            GOOSE_SOURCE_REVISION_KIND,
            revision_digest.to_vec(),
        )?;
        let certified_bytes = reader.source_observation().certified_bytes()?;
        Ok(Self {
            selection,
            reader,
            source,
            opening,
            revision_digest,
            certified_bytes,
        })
    }

    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn selection(&self) -> &GooseSourceBackedSelectionV0 {
        &self.selection
    }

    pub(crate) fn scan(&self) -> GooseSourceBackedResultV0<GooseSourceBackedScanV0<'_>> {
        let scanner = self.reader.scanner_with_profile(
            GooseNativeProfile::CoreOnly,
            GooseNativePageLimits::default(),
        )?;
        Ok(GooseSourceBackedScanV0 {
            adapter: self,
            scanner,
            sessions: BTreeMap::new(),
            complete_records: 0,
            retained_records: 0,
            rejected_records: 0,
            ignored_records: 0,
            indexed_documents: 0,
            exhausted: false,
        })
    }

    /// Revalidates the exact selected route used by a completed scan.
    pub(crate) fn revalidate(&self, snapshot: &GooseSourceBackedSnapshotV0) -> bool {
        self.selection == snapshot.selection
            && self
                .source
                .exact_descriptor_eq(snapshot.certificate.observation().source())
            && snapshot.certificate.observation().revision() == self.opening.revision()
            && self.reader.revalidate_live().unwrap_or(false)
    }
}

#[derive(Clone, Debug)]
struct GooseSessionProjection {
    session_id: StableEntityId,
    cwd: Option<String>,
}

#[derive(Debug)]
pub(crate) struct GooseSourceBackedPageV0 {
    documents: Vec<LexicalDocument>,
    complete_records: u64,
    retained_records: u64,
    rejected_records: u64,
    ignored_records: u64,
}

impl GooseSourceBackedPageV0 {
    pub(crate) fn documents(&self) -> &[LexicalDocument] {
        &self.documents
    }

    pub(crate) fn into_documents(self) -> Vec<LexicalDocument> {
        self.documents
    }

    pub(crate) fn complete_records(&self) -> u64 {
        self.complete_records
    }

    pub(crate) fn retained_records(&self) -> u64 {
        self.retained_records
    }

    pub(crate) fn rejected_records(&self) -> u64 {
        self.rejected_records
    }

    pub(crate) fn ignored_records(&self) -> u64 {
        self.ignored_records
    }
}

pub(crate) struct GooseSourceBackedScanV0<'adapter> {
    adapter: &'adapter GooseSourceBackedAdapterV0,
    scanner: GooseNativeScanner<'adapter>,
    sessions: BTreeMap<String, GooseSessionProjection>,
    complete_records: u64,
    retained_records: u64,
    rejected_records: u64,
    ignored_records: u64,
    indexed_documents: u64,
    exhausted: bool,
}

impl GooseSourceBackedScanV0<'_> {
    pub(crate) fn next_page(
        &mut self,
    ) -> GooseSourceBackedResultV0<Option<GooseSourceBackedPageV0>> {
        let Some(page) = self.scanner.next_page()? else {
            self.exhausted = true;
            return Ok(None);
        };
        let projected = self.project_page(page)?;
        self.complete_records = checked_add(self.complete_records, projected.complete_records)?;
        self.retained_records = checked_add(self.retained_records, projected.retained_records)?;
        self.rejected_records = checked_add(self.rejected_records, projected.rejected_records)?;
        self.ignored_records = checked_add(self.ignored_records, projected.ignored_records)?;
        self.indexed_documents = checked_add(
            self.indexed_documents,
            u64::try_from(projected.documents.len())
                .map_err(|_| GooseSourceBackedErrorV0::CountOverflow)?,
        )?;
        Ok(Some(projected))
    }

    pub(crate) fn finish(mut self) -> GooseSourceBackedResultV0<GooseSourceBackedSnapshotV0> {
        if !self.exhausted {
            return Err(GooseSourceBackedErrorV0::IncompleteScan);
        }
        self.scanner.finish_core()?;
        let counts = ScannedSourceCounts {
            complete_records: self.complete_records,
            retained_records: self.retained_records,
            rejected_records: self.rejected_records,
            ignored_records: self.ignored_records,
            indexed_documents: self.indexed_documents,
            certified_bytes: self.adapter.certified_bytes,
        };
        let certificate = CertifiedSource::certify(
            self.adapter.opening.clone(),
            self.adapter.opening.clone(),
            GOOSE_PARSER_REVISION,
            self.adapter.revision_digest,
            counts,
        )?;
        Ok(GooseSourceBackedSnapshotV0 {
            selection: self.adapter.selection.clone(),
            certificate,
        })
    }

    fn project_page(
        &mut self,
        page: GooseNativePage,
    ) -> GooseSourceBackedResultV0<GooseSourceBackedPageV0> {
        let complete_records = u64::try_from(page.accounting.logical_units)
            .map_err(|_| GooseSourceBackedErrorV0::CountOverflow)?;
        let rejected_records = u64::try_from(page.rejections.len())
            .map_err(|_| GooseSourceBackedErrorV0::CountOverflow)?;
        for session in page.sessions {
            let session_id = goose_session_id(&self.adapter.source, &session.native_identity)?;
            self.sessions.insert(
                session.native_identity,
                GooseSessionProjection {
                    session_id,
                    cwd: session.row.working_dir,
                },
            );
        }
        let mut documents = Vec::with_capacity(page.events.len());
        for event in page.events {
            if event.logical_row_digest.is_none() {
                continue;
            }
            let session =
                self.sessions
                    .get(&event.session_identity)
                    .ok_or(CaptureError::SystemInvariant(
                        "Goose source-backed event omitted its scanned session owner",
                    ))?;
            documents.push(goose_lexical_document(
                &self.adapter.source,
                self.adapter.revision_digest,
                self.adapter.selection.selected(),
                session,
                event,
            )?);
        }
        let retained_records =
            u64::try_from(documents.len()).map_err(|_| GooseSourceBackedErrorV0::CountOverflow)?;
        let ignored_records = complete_records
            .checked_sub(retained_records)
            .and_then(|count| count.checked_sub(rejected_records))
            .ok_or(GooseSourceBackedErrorV0::CountOverflow)?;
        Ok(GooseSourceBackedPageV0 {
            documents,
            complete_records,
            retained_records,
            rejected_records,
            ignored_records,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct GooseSourceBackedSnapshotV0 {
    selection: GooseSourceBackedSelectionV0,
    certificate: CertifiedSource,
}

impl GooseSourceBackedSnapshotV0 {
    pub(crate) fn selection(&self) -> &GooseSourceBackedSelectionV0 {
        &self.selection
    }

    pub(crate) fn certificate(&self) -> &CertifiedSource {
        &self.certificate
    }
}

fn goose_source_key() -> GooseSourceBackedResultV0<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        GOOSE_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(GOOSE_SOURCE_ANCHOR_KEY)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::Goose.as_str(),
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        GOOSE_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn goose_session_id(
    source: &SourceKey,
    native_session_id: &str,
) -> GooseSourceBackedResultV0<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        GOOSE_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: GOOSE_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn goose_lexical_document(
    source: &SourceKey,
    revision_digest: [u8; 32],
    selected_route: &GooseSourceRouteV0,
    session: &GooseSessionProjection,
    event: GooseNativeEvent,
) -> GooseSourceBackedResultV0<LexicalDocument> {
    let native_item_key = NativeItemKey::native_id(
        GOOSE_NATIVE_EVENT_NAMESPACE,
        TypedKey::utf8(event.native_identity.clone())?,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: session.session_id,
        logical_item_kind: GOOSE_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let (relation, primary_key) = goose_message_locator(event.sqlite_rowid);
    debug_assert_eq!(relation, GOOSE_LOGICAL_RELATION);
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: relation.to_owned(),
            primary_key: TypedKey::bytes(primary_key)?,
            row_version: Some(TypedKey::bytes(
                event
                    .logical_row_digest
                    .ok_or(CaptureError::SystemInvariant(
                        "Goose source-backed event omitted exact logical-row evidence",
                    ))?
                    .to_vec(),
            )?),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(revision_digest),
        event
            .logical_row_digest
            .ok_or(CaptureError::SystemInvariant(
                "Goose source-backed event omitted exact logical-row evidence",
            ))?,
    )?;
    let body = provider_local_preview(&event.searchable_text, MAX_BODY_PREVIEW_CHARS).0;
    let body = if body.is_empty() {
        format!("{} {}", event_type(&event).as_str(), event.role)
    } else {
        body
    };
    if body.is_empty() {
        return Err(GooseSourceBackedErrorV0::EmptyLexicalPreview);
    }
    let event_sequence = (event.native_order as u64) ^ (1_u64 << 63);
    let occurred_at_unix_ms = event.created_timestamp.map_or_else(
        || {
            event.timestamp.as_deref().map(|timestamp| {
                goose_timestamp(Some(timestamp), DateTime::<Utc>::UNIX_EPOCH).timestamp_millis()
            })
        },
        |timestamp| {
            Some(
                provider_timestamp_seconds(Some(timestamp as f64), DateTime::<Utc>::UNIX_EPOCH)
                    .timestamp_millis(),
            )
        },
    );
    let event_type = event_type(&event).as_str().to_owned();
    let role = Some(provider_role(Some(&event.role)).as_str().to_owned());
    let touched_files = event
        .file_touches
        .into_iter()
        .map(|touch| touch.path)
        .collect();
    Ok(LexicalDocument {
        event_id,
        session_id: session.session_id,
        parent_session_id: None,
        root_session_id: session.session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(event.session_identity),
        branch: None,
        source_path: Some(
            selected_route
                .selected_database()
                .to_string_lossy()
                .into_owned(),
        ),
        agent_type: GOOSE_AGENT_TYPE.to_owned(),
        is_primary: true,
        event_sequence,
        occurred_at_unix_ms,
        event_type,
        role,
        body,
        workspace: None,
        cwd: session.cwd.clone(),
        touched_files,
    })
}

fn event_type(event: &GooseNativeEvent) -> EventType {
    match event.kind {
        GooseNativeEventKind::Message => EventType::Message,
        GooseNativeEventKind::ToolCall => EventType::ToolCall,
        GooseNativeEventKind::ToolOutput => EventType::ToolOutput,
    }
}

fn checked_add(left: u64, right: u64) -> GooseSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(GooseSourceBackedErrorV0::CountOverflow)
}

/// Resolver bound to one exact selected route and optional explicit retained routes.
#[derive(Clone, Debug)]
pub(crate) struct GooseSourceBackedResolverV0 {
    selection: GooseSourceBackedSelectionV0,
    source: SourceKey,
}

impl GooseSourceBackedResolverV0 {
    pub(crate) fn new(selection: GooseSourceBackedSelectionV0) -> GooseSourceBackedResultV0<Self> {
        Ok(Self {
            selection,
            source: goose_source_key()?,
        })
    }

    fn acquire(
        &self,
        locator: &SourceRecordLocator,
    ) -> Result<GooseNativePathReader, HydrationFailure> {
        locator.validate_contract().map_err(|_| {
            hydration_failure(HydrationFailureKind::InvalidLocator, "invalid locator")
        })?;
        if !self.source.exact_descriptor_eq(locator.source())
            || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
        {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "locator does not identify the selected Goose source",
            ));
        }
        let expected_revision = locator.certified_source_revision_digest().ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Goose locator omitted exact source revision evidence",
            )
        })?;
        let mut opened_route = false;
        for route in self.selection.routes() {
            let reader = match GooseNativePathReader::acquire(GooseNativeSourceSelection::exact(
                route.selected_database.clone(),
            )) {
                Ok(reader) => reader,
                Err(_) => continue,
            };
            opened_route = true;
            if &reader.source_observation().generation_digest_bytes() == expected_revision {
                return Ok(reader);
            }
        }
        Err(if opened_route {
            hydration_failure(
                HydrationFailureKind::StaleSourceEvidence,
                "no explicit Goose route matches the certified source revision",
            )
        } else {
            hydration_failure(
                HydrationFailureKind::TemporarilyUnavailable,
                "the selected and explicitly retained Goose routes are unavailable",
            )
        })
    }

    fn hydrate_with_reader(
        &self,
        reader: &GooseNativePathReader,
        connection: &rusqlite::Connection,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let rowid = validate_goose_locator(request.locator())?;
        let exists = connection
            .query_row(
                "select exists(select 1 from messages where rowid = ?1)",
                [rowid],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|_| {
                hydration_failure(
                    HydrationFailureKind::TemporarilyUnavailable,
                    "Goose exact row could not be read",
                )
            })?;
        if !exists {
            return Err(hydration_failure(
                HydrationFailureKind::MissingRecord,
                "Goose exact message row is missing",
            ));
        }
        let values = load_message_values(connection, rowid).map_err(|_| {
            hydration_failure(
                HydrationFailureKind::StaleRecordEvidence,
                "Goose exact row no longer matches the supported schema",
            )
        })?;
        let digest = goose_logical_row_digest(&values).map_err(|_| {
            hydration_failure(
                HydrationFailureKind::StaleRecordEvidence,
                "Goose exact row digest could not be reconstructed",
            )
        })?;
        if &digest != request.locator().record_digest() {
            return Err(hydration_failure(
                HydrationFailureKind::StaleRecordEvidence,
                "Goose exact row digest changed",
            ));
        }
        let scanned = goose_scanned_message_at(reader, connection, rowid)?;
        if scanned.logical_row_digest != Some(digest) {
            return Err(hydration_failure(
                HydrationFailureKind::StaleRecordEvidence,
                "Goose stream parser disagrees with exact row evidence",
            ));
        }
        let text = match scanned.disposition {
            GooseMessageCellDisposition::Retained => {
                let event =
                    normalize_goose_native_message(scanned.into_retained().map_err(|_| {
                        hydration_failure(
                            HydrationFailureKind::StaleRecordEvidence,
                            "Goose retained row changed parser disposition",
                        )
                    })?)
                    .map_err(|_| {
                        hydration_failure(
                            HydrationFailureKind::StaleRecordEvidence,
                            "Goose retained row could not be normalized",
                        )
                    })?;
                goose_complete_content_text(&event.content)
                    .unwrap_or_else(|| event.searchable_text.clone())
            }
            GooseMessageCellDisposition::OutputFailure
            | GooseMessageCellDisposition::OutputTimeout => {
                normalize_goose_native_output_diagnostic(&scanned)
                    .map_err(|_| {
                        hydration_failure(
                            HydrationFailureKind::StaleRecordEvidence,
                            "Goose output diagnostic could not be normalized",
                        )
                    })?
                    .searchable_text
            }
            _ => {
                return Err(hydration_failure(
                    HydrationFailureKind::StaleRecordEvidence,
                    "Goose exact row is no longer retained by the parser",
                ));
            }
        };
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes: text.into_bytes(),
        })
    }
}

impl ContentSourceResolver for GooseSourceBackedResolverV0 {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let reader = self.acquire(request.locator())?;
        let snapshot = reader.snapshot_connection().map_err(|_| {
            hydration_failure(
                HydrationFailureKind::TemporarilyUnavailable,
                "Goose exact snapshot is unavailable",
            )
        })?;
        let connection = reader.snapshot_connection_ref(&snapshot).map_err(|_| {
            hydration_failure(
                HydrationFailureKind::TemporarilyUnavailable,
                "Goose exact snapshot is unavailable",
            )
        })?;
        goose_prepare_native_identity_index(
            connection,
            reader.schema(),
            GooseNativePageLimits::default(),
        )
        .map_err(|_| {
            hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
                "Goose stream parser could not prepare the exact snapshot",
            )
        })?;
        let hydrated = self.hydrate_with_reader(&reader, connection, request)?;
        finish_goose_hydration(&reader, snapshot)?;
        Ok(hydrated)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        let Some(first) = request.events().first() else {
            return Ok(Vec::new());
        };
        let reader = self.acquire(first.locator())?;
        let expected_revision = first.locator().certified_source_revision_digest();
        if request.events().iter().any(|event| {
            event.locator().certified_source_revision_digest() != expected_revision
                || !event
                    .locator()
                    .source()
                    .exact_descriptor_eq(first.locator().source())
        }) {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Goose session hydration mixed source generations",
            ));
        }
        let snapshot = reader.snapshot_connection().map_err(|_| {
            hydration_failure(
                HydrationFailureKind::TemporarilyUnavailable,
                "Goose exact snapshot is unavailable",
            )
        })?;
        let connection = reader.snapshot_connection_ref(&snapshot).map_err(|_| {
            hydration_failure(
                HydrationFailureKind::TemporarilyUnavailable,
                "Goose exact snapshot is unavailable",
            )
        })?;
        goose_prepare_native_identity_index(
            connection,
            reader.schema(),
            GooseNativePageLimits::default(),
        )
        .map_err(|_| {
            hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
                "Goose stream parser could not prepare the exact snapshot",
            )
        })?;
        let hydrated = request
            .events()
            .iter()
            .map(|event| self.hydrate_with_reader(&reader, connection, event))
            .collect::<Result<Vec<_>, _>>()?;
        finish_goose_hydration(&reader, snapshot)?;
        Ok(hydrated)
    }
}

fn validate_goose_locator(locator: &SourceRecordLocator) -> Result<i64, HydrationFailure> {
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Goose locator is not a SQLite coordinate",
        ));
    };
    let (TypedKey::Bytes(primary_key), Some(TypedKey::Bytes(row_version))) =
        (primary_key, row_version)
    else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Goose locator has invalid logical-row evidence",
        ));
    };
    if logical_relation != GOOSE_LOGICAL_RELATION
        || row_version.as_slice() != locator.record_digest()
    {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Goose locator logical-row evidence is inconsistent",
        ));
    }
    decode_goose_message_locator(primary_key).ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Goose logical-row-v3 coordinate is invalid",
        )
    })
}

fn goose_scanned_message_at(
    reader: &GooseNativePathReader,
    connection: &rusqlite::Connection,
    rowid: i64,
) -> Result<GooseScannedMessage, HydrationFailure> {
    let keyset = if rowid == i64::MIN {
        GooseNativeRowKeyset::Unstarted
    } else {
        GooseNativeRowKeyset::After(rowid - 1)
    };
    let limits = GooseNativePageLimits::new(1, GooseNativePageLimits::default().retained_bytes)
        .map_err(|_| {
            hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
                "Goose exact-row parser limits are invalid",
            )
        })?;
    let mut rows = goose_fetch_native_message_page(connection, reader.schema(), keyset, limits)
        .map_err(|_| {
            hydration_failure(
                HydrationFailureKind::StaleRecordEvidence,
                "Goose exact row could not be parsed",
            )
        })?;
    if rows.len() != 1 || rows[0].sqlite_rowid != rowid {
        return Err(hydration_failure(
            HydrationFailureKind::MissingRecord,
            "Goose exact message row is missing from the parser stream",
        ));
    }
    Ok(rows.remove(0))
}

fn finish_goose_hydration(
    reader: &GooseNativePathReader,
    snapshot: crate::provider_sources::SqliteSourceReadSnapshot,
) -> Result<(), HydrationFailure> {
    match reader.finish_snapshot_connection(snapshot) {
        Ok(true) => Ok(()),
        Ok(false) => Err(hydration_failure(
            HydrationFailureKind::StaleSourceEvidence,
            "Goose exact source changed before hydration finished",
        )),
        Err(_) => Err(hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            "Goose exact source could not be revalidated",
        )),
    }
}

fn hydration_failure(kind: HydrationFailureKind, detail: impl Into<String>) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.into(),
    }
}
