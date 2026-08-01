use std::{
    collections::BTreeMap,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CoreRecord, CoreRecordError,
    EventIdentityInput, EventType, NativeItemKey, NativeSessionKey, ProjectionContractError,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey, StableEntityId, TypedKey,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    normalization::{
        goose_timestamp, normalize_goose_native_message, normalize_goose_native_output,
        GooseNativeEvent, GooseNativeEventKind,
    },
    position::goose_message_locator,
    schema::GooseNativeSchema,
    stream::{
        goose_fetch_native_message_page, goose_fetch_native_session_page,
        goose_require_canonical_native_order, GooseMessageCellDisposition, GooseNativePageLimits,
        GooseNativeRowKeyset, GooseNativeSessionKeyset,
    },
};
use crate::{
    common::io::{OpenedProviderSourcePath, ProviderSourceDirectory, ProviderSourceRoot},
    provider::{
        normalization::{provider_role, provider_timestamp_seconds},
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
        SqliteLogicalSnapshot, SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot,
    },
    CaptureError, GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
};

mod fingerprint;

use fingerprint::GooseLogicalFingerprint;

const GOOSE_SOURCE_ANCHOR_NAMESPACE: &str = "goose.installed-sessions";
const GOOSE_SOURCE_ANCHOR_KEY: &str = "selected-platform-sessions-db";
const GOOSE_SOURCE_SCHEMA_VARIANT: &str = "goose-sessions-sqlite-v0";
const GOOSE_PARSER_REVISION: &str = "goose-logical-sqlite-v5";
const GOOSE_NATIVE_SESSION_NAMESPACE: &str = "goose.session";
const GOOSE_NATIVE_EVENT_NAMESPACE: &str = "goose.message";
const GOOSE_LOGICAL_SESSION_KIND: &str = "goose-session";
const GOOSE_LOGICAL_EVENT_KIND: &str = "goose-event";
const GOOSE_LOGICAL_RELATION: &str = "goose-messages-native-id-v4";
const GOOSE_MAX_EXPLICIT_RETAINED_ROUTES: usize = 32;
const GOOSE_MISSING_TREE_DOMAIN: &[u8] = b"ctx.goose.missing-logical-tree.v1\0";

#[derive(Debug, Error)]
pub(crate) enum GooseSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("Goose source-backed count overflow")]
    CountOverflow,
    #[error("Goose source-backed projection emitted empty normalized content")]
    EmptyNormalizedContent,
    #[error("Goose source-backed selection has too many explicit retained routes")]
    TooManyRetainedRoutes,
    #[error("Goose source-backed selection contains a duplicate database route")]
    DuplicateDatabaseRoute,
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
    data_root: PathBuf,
    selected: GooseSourceRouteV0,
    retained: Vec<GooseSourceRouteV0>,
}

impl GooseSourceBackedSelectionV0 {
    pub(crate) fn exact(
        data_root: impl Into<PathBuf>,
        selected_database: impl Into<PathBuf>,
        platform_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            data_root: data_root.into(),
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
}

pub(crate) struct GooseSourceBackedAdapterV0 {
    selection: GooseSourceBackedSelectionV0,
    source: SourceKey,
}

impl GooseSourceBackedAdapterV0 {
    pub(crate) fn open(selection: GooseSourceBackedSelectionV0) -> GooseSourceBackedResultV0<Self> {
        Ok(Self {
            source: goose_source_key()?,
            selection,
        })
    }
}

pub(crate) struct GoosePresentAuthority {
    retained: RetainedGooseDirectory,
    snapshot: Mutex<Option<SqliteSourceReadSnapshot>>,
    terminal_revalidate: Box<dyn Fn() -> bool + Send + Sync>,
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
        let retained = RetainedGooseDirectory::open(
            &self.selection.data_root,
            self.selection.selected().selected_database(),
        )
        .map_err(route_error)?;
        let Some(snapshot) = retained.open_snapshot()? else {
            let fingerprint = missing_tree_fingerprint(&self.source);
            return Ok(CompleteDocumentTree::new(
                fingerprint,
                Vec::new(),
                GooseTreeAuthority::Missing(retained),
            ));
        };
        let fingerprint =
            observe_goose_logical_fingerprint(snapshot.connection().map_err(route_error)?)
                .map_err(route_error)?;
        let terminal_revalidate = snapshot.terminal_revalidator();
        Ok(CompleteDocumentTree::new(
            fingerprint,
            vec![ObservedDocumentLeaf::new(
                DocumentLeafFingerprint::new(fingerprint),
                (),
            )],
            GooseTreeAuthority::Present(Box::new(GoosePresentAuthority {
                retained,
                snapshot: Mutex::new(Some(snapshot)),
                terminal_revalidate: Box::new(move || terminal_revalidate().is_ok()),
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
    snapshot.finish().map_err(route_error)?;
    authority.retained.revalidate()?;
    if !(authority.terminal_revalidate)() {
        return Err(source_changed(
            "Goose retained terminal fence changed before publication",
        ));
    }
    Ok(())
}

pub(crate) struct RetainedGooseDirectory {
    directory: ProviderSourceDirectory,
    sqlite: SqliteSourceDirectoryAuthority,
    leaf: OsString,
}

impl RetainedGooseDirectory {
    fn open(data_root: &Path, path: &Path) -> GooseSourceBackedResultV0<Self> {
        let parent = path.parent().ok_or_else(|| {
            CaptureError::InvalidPayload("Goose SQLite source has no parent directory".to_owned())
        })?;
        let leaf = path.file_name().map(OsString::from).ok_or_else(|| {
            CaptureError::InvalidPayload("Goose SQLite source has no leaf name".to_owned())
        })?;
        let root = ProviderSourceRoot::open(parent)?;
        let directory = root.directory()?;
        let authority = directory.try_clone_authority_handle()?;
        let sqlite = retain_sqlite_source_directory_authority(data_root, &authority, parent)
            .map_err(sqlite_access_error)?;
        Ok(Self {
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
        // The selected SQLite family and the named parent identity are the
        // source authority. Unrelated sibling creation must not invalidate a
        // pinned logical snapshot merely because it changes directory
        // timestamps.
        self.sqlite
            .revalidate()
            .map_err(sqlite_access_error)
            .map_err(route_error)
    }
}

fn observe_goose_logical_fingerprint(
    connection: &Connection,
) -> GooseSourceBackedResultV0<[u8; 32]> {
    let schema = GooseNativeSchema::probe(connection)?;
    let limits = GooseNativePageLimits::default();
    goose_require_canonical_native_order(connection)?;
    let mut fingerprint = GooseLogicalFingerprint::new(&schema);
    let mut session_keyset = GooseNativeSessionKeyset::Unstarted;
    loop {
        let rows = goose_fetch_native_session_page(connection, &schema, &session_keyset, limits)?;
        let Some(last) = rows.last().map(|row| row.native_identity.clone()) else {
            break;
        };
        session_keyset = GooseNativeSessionKeyset::After(last);
        for row in &rows {
            fingerprint.record_session(row)?;
        }
    }
    let mut message_keyset = GooseNativeRowKeyset::Unstarted;
    loop {
        let rows = goose_fetch_native_message_page(connection, &schema, message_keyset, limits)?;
        let Some(last) = rows.last().map(|row| row.native_order) else {
            break;
        };
        message_keyset = GooseNativeRowKeyset::After(last);
        for row in &rows {
            fingerprint.record_message(row)?;
        }
    }
    fingerprint.finish()
}

#[derive(Clone)]
struct GooseSessionProjection {
    session_id: StableEntityId,
    cwd: Option<String>,
}

fn scan_goose_logical_snapshot(
    connection: &Connection,
    source: &SourceKey,
    sink: &mut ChangedDocumentSink<'_, '_>,
) -> GooseSourceBackedResultV0<DocumentSourceTerminal> {
    let schema = GooseNativeSchema::probe(connection)?;
    let limits = GooseNativePageLimits::default();
    goose_require_canonical_native_order(connection)?;
    let mut sessions = BTreeMap::new();
    let mut fingerprint = GooseLogicalFingerprint::new(&schema);
    let mut complete_records = 0_u64;
    let mut retained_records = 0_u64;
    let mut rejected_records = 0_u64;
    let mut certified_bytes = 0_u64;
    let mut next_event_sequence = 0_u64;

    let mut session_keyset = GooseNativeSessionKeyset::Unstarted;
    loop {
        let rows = goose_fetch_native_session_page(connection, &schema, &session_keyset, limits)?;
        let Some(last) = rows.last().map(|row| row.native_identity.clone()) else {
            break;
        };
        session_keyset = GooseNativeSessionKeyset::After(last);
        for scanned in rows {
            complete_records = checked_add(complete_records, 1)?;
            certified_bytes = checked_add(certified_bytes, scanned.observed_bytes)?;
            fingerprint.record_session(&scanned)?;
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

    let mut message_keyset = GooseNativeRowKeyset::Unstarted;
    loop {
        let rows = goose_fetch_native_message_page(connection, &schema, message_keyset, limits)?;
        let Some(last) = rows.last().map(|row| row.native_order) else {
            break;
        };
        message_keyset = GooseNativeRowKeyset::After(last);
        for scanned in rows {
            complete_records = checked_add(complete_records, 1)?;
            certified_bytes = checked_add(certified_bytes, scanned.content_bytes)?;
            fingerprint.record_message(&scanned)?;
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
                | GooseMessageCellDisposition::OutputTimeout
                | GooseMessageCellDisposition::OutputSuccess
                | GooseMessageCellDisposition::OutputUnknown => {
                    normalize_goose_native_output(&scanned)?
                }
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
            let event_sequence = goose_event_sequence(&mut next_event_sequence)?;
            sink.emit_core_record(goose_core_record(source, session, event, event_sequence)?)
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
    let content_digest = fingerprint.finish()?;
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

fn goose_core_record(
    source: &SourceKey,
    session: &GooseSessionProjection,
    event: GooseNativeEvent,
    event_sequence: u64,
) -> GooseSourceBackedResultV0<CoreRecord> {
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
    let (relation, primary_key) = goose_message_locator(event.native_order);
    debug_assert_eq!(relation, GOOSE_LOGICAL_RELATION);
    let native_event_id = TypedKey::bytes(primary_key)?;
    let normalized_event_type = event_type(&event);
    let normalized_role = provider_role(Some(&event.role));
    let body = if event.searchable_text.is_empty() {
        format!("{} {}", normalized_event_type.as_str(), event.role)
    } else {
        event.searchable_text
    };
    if body.is_empty() {
        return Err(GooseSourceBackedErrorV0::EmptyNormalizedContent);
    }
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
    let native_file_touches =
        (!event.file_touches.is_empty()).then(|| serde_json::json!(&event.file_touches));
    let mut record = CoreRecord::new_selected(
        event_id,
        session.session_id,
        session.session_id,
        source.clone(),
        event_sequence,
        normalized_event_type.as_str(),
        AgentType::Primary.as_str(),
        true,
        GOOSE_PARSER_REVISION,
        body,
    )?;
    record.provider_session_id = Some(event.session_identity);
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = occurred_at_unix_ms;
    record.role = Some(normalized_role.as_str().to_owned());
    record.cwd = session.cwd.clone();
    if let Some(native_file_touches) = native_file_touches {
        record.metadata.insert(
            "provider_native_file_touches".to_owned(),
            native_file_touches,
        );
    }
    if event.kind == GooseNativeEventKind::ToolOutput {
        record.content.structured_content = Some(serde_json::json!({
            "provider_native_result": event.content,
        }));
    }
    record.validate_contract()?;
    Ok(record)
}

fn goose_event_sequence(next: &mut u64) -> GooseSourceBackedResultV0<u64> {
    let sequence = *next;
    if sequence > i64::MAX as u64 {
        return Err(GooseSourceBackedErrorV0::CountOverflow);
    }
    *next = sequence
        .checked_add(1)
        .ok_or(GooseSourceBackedErrorV0::CountOverflow)?;
    Ok(sequence)
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
