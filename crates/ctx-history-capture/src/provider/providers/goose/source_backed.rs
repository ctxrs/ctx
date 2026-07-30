use std::{
    collections::BTreeMap,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, BatchHydrationRequest, BatchHydrationResult,
    CaptureProvider, EventIdentityInput, EventType, HydrationFailure, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, ProjectionContractError,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    normalization::{
        goose_timestamp, normalize_goose_native_message, normalize_goose_native_output_diagnostic,
        GooseNativeEvent, GooseNativeEventKind,
    },
    position::{goose_message_locator, GooseNativeRowKeyset},
    schema::{GooseNativeSchema, GooseSessionRow},
    stream::{
        goose_fetch_native_message_page, goose_fetch_native_session_page,
        goose_prepare_native_identity_index, GooseMessageCellDisposition, GooseNativePageLimits,
        GooseScannedMessage,
    },
};
use crate::{
    common::io::{OpenedProviderSourcePath, ProviderSourceDirectory, ProviderSourceRoot},
    provider::{
        normalization::{provider_local_preview, provider_role, provider_timestamp_seconds},
        source_backed::{
            family::document::{
                ChangedDocumentSink, CompleteDocumentTree, DocumentLeafFingerprint,
                DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
            },
            route_error, SourceBackedRouteError, SourceBackedRouteErrorKind,
            SourceBackedRouteResult,
        },
    },
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteLogicalSnapshot, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
        SqliteSourceReadSnapshot,
    },
    CaptureError, GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_TEXT_CHARS,
};

mod hydration;

pub(crate) use hydration::GooseSourceBackedResolverV0;

const GOOSE_SOURCE_ANCHOR_NAMESPACE: &str = "goose.installed-sessions";
const GOOSE_SOURCE_ANCHOR_KEY: &str = "selected-platform-sessions-db";
const GOOSE_SOURCE_SCHEMA_VARIANT: &str = "goose-sessions-sqlite-v0";
const GOOSE_PARSER_REVISION: &str = "goose-logical-sqlite-v1";
const GOOSE_NATIVE_SESSION_NAMESPACE: &str = "goose.session";
const GOOSE_NATIVE_EVENT_NAMESPACE: &str = "goose.message";
const GOOSE_LOGICAL_SESSION_KIND: &str = "goose-session";
const GOOSE_LOGICAL_EVENT_KIND: &str = "goose-event";
const GOOSE_LOGICAL_RELATION: &str = "goose-messages-native-id-v4";
const GOOSE_AGENT_TYPE: &str = "goose";
const GOOSE_MAX_EXPLICIT_RETAINED_ROUTES: usize = 32;
const GOOSE_LOGICAL_DATABASE_DOMAIN: &[u8] = b"ctx.goose.logical-database.v1\0";
const GOOSE_LOGICAL_SESSION_DOMAIN: &[u8] = b"ctx.goose.logical-session.v1\0";
const GOOSE_LOGICAL_MESSAGE_DOMAIN: &[u8] = b"ctx.goose.logical-message.v1\0";
const GOOSE_MISSING_TREE_DOMAIN: &[u8] = b"ctx.goose.missing-logical-tree.v1\0";

#[derive(Debug, Error)]
pub(crate) enum GooseSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("Goose source-backed count overflow")]
    CountOverflow,
    #[error("Goose source-backed projection emitted an empty lexical body")]
    EmptyLexicalBody,
    #[error("Goose source-backed selection has too many explicit retained routes")]
    TooManyRetainedRoutes,
    #[error("Goose source-backed selection contains a duplicate database route")]
    DuplicateDatabaseRoute,
    #[error("Goose source-backed locator is invalid")]
    InvalidLocator,
}

pub(crate) type GooseSourceBackedResultV0<T> = Result<T, GooseSourceBackedErrorV0>;

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
}

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

    fn routes(&self) -> impl Iterator<Item = &GooseSourceRouteV0> {
        std::iter::once(&self.selected).chain(self.retained.iter())
    }
}

pub(crate) struct GooseSourceBackedAdapterV0 {
    selection: GooseSourceBackedSelectionV0,
    resolver: GooseSourceBackedResolverV0,
    source: SourceKey,
}

impl GooseSourceBackedAdapterV0 {
    pub(crate) fn open(
        selection: GooseSourceBackedSelectionV0,
        resolver: GooseSourceBackedResolverV0,
    ) -> GooseSourceBackedResultV0<Self> {
        if selection != resolver.selection {
            return Err(GooseSourceBackedErrorV0::InvalidLocator);
        }
        Ok(Self {
            source: resolver.source.clone(),
            selection,
            resolver,
        })
    }
}

pub(crate) struct GoosePresentAuthority {
    retained: RetainedGooseDirectory,
    physical_evidence: SqliteSourceEvidence,
    snapshot: Mutex<Option<SqliteSourceReadSnapshot>>,
}

pub(crate) enum GooseTreeAuthority {
    Present(Box<GoosePresentAuthority>),
    Missing(RetainedGooseDirectory),
}

impl ReplacementDocumentTree for GooseSourceBackedAdapterV0 {
    type Leaf = ();
    type TreeAuthority = GooseTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        GOOSE_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        self.source.exact_descriptor_eq(source)
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let retained = RetainedGooseDirectory::open(self.selection.selected().selected_database())
            .map_err(route_error)?;
        let Some(snapshot) = retained.open_snapshot()? else {
            let fingerprint = missing_tree_fingerprint(&self.source);
            return Ok(CompleteDocumentTree::new(
                fingerprint,
                Vec::new(),
                GooseTreeAuthority::Missing(retained),
            ));
        };
        let physical_evidence = snapshot.evidence().clone();
        let fingerprint = *physical_evidence.revision();
        Ok(CompleteDocumentTree::new(
            fingerprint,
            vec![ObservedDocumentLeaf::with_durable_replay(
                DocumentLeafFingerprint::new(fingerprint),
                (),
                false,
            )],
            GooseTreeAuthority::Present(Box::new(GoosePresentAuthority {
                retained,
                physical_evidence,
                snapshot: Mutex::new(Some(snapshot)),
            })),
        ))
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        _leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let GooseTreeAuthority::Present(authority) = authority else {
            return Err(internal_route_error(
                "Goose lifecycle requested a scan for a missing database",
            ));
        };
        let snapshot = take_snapshot(&authority.snapshot, "Goose")?;
        sink.begin_source(self.source.clone())?;
        let terminal = scan_goose_logical_snapshot(
            snapshot.connection().map_err(route_error)?,
            &self.source,
            self.selection.selected(),
            sink,
        )
        .map_err(route_error)?;
        snapshot.revalidate().map_err(route_error)?;
        authority.retained.revalidate()?;
        restore_snapshot(&authority.snapshot, snapshot, "Goose")?;
        Ok(terminal)
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        match &tree.authority {
            GooseTreeAuthority::Present(authority) => {
                let snapshot = take_snapshot(&authority.snapshot, "Goose")?;
                finish_present_authority(authority, snapshot)?;
            }
            GooseTreeAuthority::Missing(retained) => {
                if retained.open_snapshot()?.is_some() {
                    return Err(source_changed("Goose database appeared"));
                }
                retained.revalidate()?;
            }
        }
        Ok(tree.tree_fingerprint)
    }

    fn hydrate_group(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        self.resolver.hydrate_batch(request)
    }
}

fn restore_snapshot(
    slot: &Mutex<Option<SqliteSourceReadSnapshot>>,
    snapshot: SqliteSourceReadSnapshot,
    provider: &str,
) -> SourceBackedRouteResult<()> {
    let mut slot = slot
        .lock()
        .map_err(|_| internal_route_error(format!("{provider} snapshot lock was poisoned")))?;
    if slot.replace(snapshot).is_some() {
        return Err(internal_route_error(format!(
            "{provider} snapshot slot was already occupied"
        )));
    }
    Ok(())
}

fn take_snapshot(
    snapshot: &Mutex<Option<SqliteSourceReadSnapshot>>,
    provider: &str,
) -> SourceBackedRouteResult<SqliteSourceReadSnapshot> {
    snapshot
        .lock()
        .map_err(|_| internal_route_error(format!("{provider} snapshot lock was poisoned")))?
        .take()
        .ok_or_else(|| internal_route_error(format!("{provider} snapshot was already consumed")))
}

fn finish_present_authority(
    authority: &GoosePresentAuthority,
    snapshot: SqliteSourceReadSnapshot,
) -> SourceBackedRouteResult<()> {
    let evidence = snapshot.finish().map_err(route_error)?;
    authority.retained.revalidate()?;
    if evidence != authority.physical_evidence {
        return Err(source_changed("Goose database changed during its snapshot"));
    }
    Ok(())
}

pub(crate) struct RetainedGooseDirectory {
    root: ProviderSourceRoot,
    directory: ProviderSourceDirectory,
    sqlite: SqliteSourceDirectoryAuthority,
    leaf: OsString,
}

impl RetainedGooseDirectory {
    fn open(path: &Path) -> GooseSourceBackedResultV0<Self> {
        let parent = path.parent().ok_or_else(|| {
            CaptureError::InvalidPayload("Goose SQLite source has no parent directory".to_owned())
        })?;
        let leaf = path.file_name().map(OsString::from).ok_or_else(|| {
            CaptureError::InvalidPayload("Goose SQLite source has no leaf name".to_owned())
        })?;
        let root = ProviderSourceRoot::open(parent)?;
        let directory = root.directory()?;
        let authority = directory.try_clone_authority_handle()?;
        let sqlite = retain_sqlite_source_directory_authority(&authority, parent)
            .map_err(sqlite_access_error)?;
        Ok(Self {
            root,
            directory,
            sqlite,
            leaf,
        })
    }

    fn open_snapshot(&self) -> SourceBackedRouteResult<Option<SqliteSourceReadSnapshot>> {
        match self.directory.open_child(&self.leaf) {
            Ok(OpenedProviderSourcePath::File(file)) => file.revalidate().map_err(route_error)?,
            Ok(OpenedProviderSourcePath::Directory(_)) => {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::InvalidSource,
                    "Goose SQLite leaf is a directory",
                ));
            }
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                self.revalidate()?;
                return Ok(None);
            }
            Err(error) => return Err(route_error(error)),
        }
        let snapshot = open_root_handle_sqlite_source_snapshot(&self.sqlite, &self.leaf)
            .map_err(route_error)?;
        self.revalidate()?;
        Ok(Some(snapshot))
    }

    fn revalidate(&self) -> SourceBackedRouteResult<()> {
        self.directory.revalidate().map_err(route_error)?;
        self.root.revalidate().map_err(route_error)
    }
}

#[derive(Clone)]
struct GooseSessionProjection {
    session_id: StableEntityId,
    cwd: Option<String>,
}

fn scan_goose_logical_snapshot(
    connection: &Connection,
    source: &SourceKey,
    selected: &GooseSourceRouteV0,
    sink: &mut ChangedDocumentSink<'_, '_>,
) -> GooseSourceBackedResultV0<DocumentSourceTerminal> {
    let schema = GooseNativeSchema::probe(connection)?;
    let limits = GooseNativePageLimits::default();
    goose_prepare_native_identity_index(connection, &schema, limits)?;
    let mut sessions = BTreeMap::new();
    let mut row_evidence = Vec::new();
    let mut complete_records = 0_u64;
    let mut retained_records = 0_u64;
    let mut rejected_records = 0_u64;
    let mut certified_bytes = 0_u64;

    let mut keyset = GooseNativeRowKeyset::Unstarted;
    loop {
        let rows = goose_fetch_native_session_page(connection, &schema, keyset, limits)?;
        let Some(last) = rows.last().map(|row| row.sqlite_rowid) else {
            break;
        };
        keyset = GooseNativeRowKeyset::After(last);
        for scanned in rows {
            complete_records = checked_add(complete_records, 1)?;
            certified_bytes = checked_add(certified_bytes, scanned.observed_bytes)?;
            row_evidence.push(goose_session_evidence(&scanned));
            let Some(row) = scanned.row else {
                rejected_records = checked_add(rejected_records, 1)?;
                continue;
            };
            if row.id.trim().is_empty() {
                rejected_records = checked_add(rejected_records, 1)?;
                continue;
            }
            let session_id = goose_session_id(source, &row.id)?;
            sessions.insert(
                row.id,
                GooseSessionProjection {
                    session_id,
                    cwd: row.working_dir,
                },
            );
        }
    }

    keyset = GooseNativeRowKeyset::Unstarted;
    loop {
        let rows = goose_fetch_native_message_page(connection, &schema, keyset, limits)?;
        let Some(last) = rows.last().map(|row| row.sqlite_rowid) else {
            break;
        };
        keyset = GooseNativeRowKeyset::After(last);
        for scanned in rows {
            complete_records = checked_add(complete_records, 1)?;
            certified_bytes = checked_add(certified_bytes, scanned.content_bytes)?;
            row_evidence.push(goose_message_evidence(&scanned));
            let session_identity = scanned.session_identity.clone();
            let event = match scanned.disposition {
                GooseMessageCellDisposition::Retained => {
                    match normalize_goose_native_message(scanned.into_retained()?) {
                        Ok(event) => Some(event),
                        Err(_) => {
                            rejected_records = checked_add(rejected_records, 1)?;
                            None
                        }
                    }
                }
                GooseMessageCellDisposition::OutputFailure
                | GooseMessageCellDisposition::OutputTimeout => {
                    Some(normalize_goose_native_output_diagnostic(&scanned)?)
                }
                GooseMessageCellDisposition::OutputSuccess
                | GooseMessageCellDisposition::OutputUnknown => None,
                _ => {
                    rejected_records = checked_add(rejected_records, 1)?;
                    None
                }
            };
            let Some(event) = event else {
                continue;
            };
            let session = sessions
                .get(&session_identity)
                .ok_or(CaptureError::SystemInvariant(
                    "Goose retained event omitted its accepted session owner",
                ))?;
            sink.emit_document(goose_lexical_document(source, selected, session, event)?)
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
            retained_records = checked_add(retained_records, 1)?;
        }
    }

    let ignored_records = complete_records
        .checked_sub(retained_records)
        .and_then(|count| count.checked_sub(rejected_records))
        .ok_or(GooseSourceBackedErrorV0::CountOverflow)?;
    let counts = ScannedSourceCounts {
        complete_records,
        retained_records,
        rejected_records,
        ignored_records,
        indexed_documents: retained_records,
        certified_bytes,
    };
    row_evidence.sort_unstable();
    let mut digest = Sha256::new();
    digest.update(GOOSE_LOGICAL_DATABASE_DOMAIN);
    hash_bytes(&mut digest, schema.capability_digest.as_bytes());
    digest.update(
        u64::try_from(row_evidence.len())
            .map_err(|_| GooseSourceBackedErrorV0::CountOverflow)?
            .to_be_bytes(),
    );
    for row in row_evidence {
        digest.update(row);
    }
    let content_digest = digest.finalize().into();
    let logical = SqliteLogicalSnapshot::new(
        GOOSE_PARSER_REVISION,
        schema.capability_digest.as_bytes(),
        content_digest,
        counts,
    );
    let certificate = logical.certify(source.clone())?;
    Ok(DocumentSourceTerminal {
        source: source.clone(),
        opening: certificate.observation().clone(),
        closing: certificate.observation().clone(),
        parser_revision: GOOSE_PARSER_REVISION,
        content_digest,
        counts,
    })
}

fn goose_session_evidence(session: &super::stream::GooseScannedSession) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(GOOSE_LOGICAL_SESSION_DOMAIN);
    hash_optional_text(&mut digest, session.bounded_native_identity.as_deref());
    digest.update(session.observed_bytes.to_be_bytes());
    digest.update([u8::from(session.storage_class_supported)]);
    if let Some(row) = &session.row {
        digest.update([1]);
        hash_session_row(&mut digest, row);
    } else {
        digest.update([0]);
        if session.bounded_native_identity.is_none() {
            digest.update(session.sqlite_rowid.to_be_bytes());
        }
    }
    digest.finalize().into()
}

fn hash_session_row(digest: &mut Sha256, row: &GooseSessionRow) {
    hash_text(digest, &row.id);
    for value in [
        row.name.as_deref(),
        row.description.as_deref(),
        row.session_type.as_deref(),
        row.working_dir.as_deref(),
        row.created_at.as_deref(),
        row.updated_at.as_deref(),
        row.extension_data.as_deref(),
        row.provider_name.as_deref(),
        row.model_config_json.as_deref(),
        row.goose_mode.as_deref(),
        row.archived_at.as_deref(),
        row.project_id.as_deref(),
    ] {
        hash_optional_text(digest, value);
    }
    digest.update([u8::from(row.user_set_name)]);
    for value in [
        row.total_tokens,
        row.input_tokens,
        row.output_tokens,
        row.accumulated_total_tokens,
        row.accumulated_input_tokens,
        row.accumulated_output_tokens,
    ] {
        hash_optional_i64(digest, value);
    }
    match row.accumulated_cost {
        Some(value) => {
            digest.update([1]);
            digest.update(value.to_bits().to_be_bytes());
        }
        None => digest.update([0]),
    }
}

fn goose_message_evidence(message: &GooseScannedMessage) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(GOOSE_LOGICAL_MESSAGE_DOMAIN);
    digest.update(message.native_order.to_be_bytes());
    hash_text(&mut digest, &message.native_identity);
    hash_text(&mut digest, &message.session_identity);
    hash_text(&mut digest, &message.role);
    digest.update([message_disposition_code(message.disposition)]);
    if let Some(row) = message.logical_row_digest {
        digest.update([1]);
        digest.update(row);
    } else {
        digest.update([0]);
        digest.update(message.content_bytes.to_be_bytes());
    }
    digest.finalize().into()
}

fn message_disposition_code(disposition: GooseMessageCellDisposition) -> u8 {
    match disposition {
        GooseMessageCellDisposition::Retained => 0,
        GooseMessageCellDisposition::OutputSuccess => 1,
        GooseMessageCellDisposition::OutputFailure => 2,
        GooseMessageCellDisposition::OutputTimeout => 3,
        GooseMessageCellDisposition::OutputUnknown => 4,
        GooseMessageCellDisposition::MalformedJson => 5,
        GooseMessageCellDisposition::UnsupportedJsonRoot => 6,
        GooseMessageCellDisposition::NonObjectBlock => 7,
        GooseMessageCellDisposition::UnknownBlockType => 8,
        GooseMessageCellDisposition::OversizedRetainedContent => 9,
        GooseMessageCellDisposition::MissingSession => 10,
        GooseMessageCellDisposition::UnsupportedStorageClass => 11,
        GooseMessageCellDisposition::DuplicateBlockType => 12,
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
    let record_digest = event
        .logical_row_digest
        .ok_or(CaptureError::SystemInvariant(
            "Goose retained event omitted logical-row evidence",
        ))?;
    let (relation, primary_key) = goose_message_locator(event.native_order);
    debug_assert_eq!(relation, GOOSE_LOGICAL_RELATION);
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: relation.to_owned(),
            primary_key: TypedKey::bytes(primary_key)?,
            row_version: Some(TypedKey::bytes(record_digest.to_vec())?),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        record_digest,
    )?;
    let body = provider_local_preview(&event.searchable_text, PROVIDER_MAX_TEXT_CHARS).0;
    let body = if body.is_empty() {
        format!("{} {}", event_type(&event).as_str(), event.role)
    } else {
        body
    };
    if body.is_empty() {
        return Err(GooseSourceBackedErrorV0::EmptyLexicalBody);
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

fn hash_bytes(digest: &mut Sha256, value: &[u8]) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(value);
}

fn hash_text(digest: &mut Sha256, value: &str) {
    hash_bytes(digest, value.as_bytes());
}

fn hash_optional_text(digest: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            digest.update([1]);
            hash_text(digest, value);
        }
        None => digest.update([0]),
    }
}

fn hash_optional_i64(digest: &mut Sha256, value: Option<i64>) {
    match value {
        Some(value) => {
            digest.update([1]);
            digest.update(value.to_be_bytes());
        }
        None => digest.update([0]),
    }
}

fn missing_tree_fingerprint(source: &SourceKey) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(GOOSE_MISSING_TREE_DOMAIN);
    digest.update(source.exact_descriptor_digest());
    digest.finalize().into()
}

fn source_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn internal_route_error(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

fn sqlite_access_error(error: crate::provider_sources::SqliteSourceAccessError) -> CaptureError {
    CaptureError::SystemIo {
        operation: "accessing a retained Goose SQLite source",
        source: io::Error::other(error),
    }
}
