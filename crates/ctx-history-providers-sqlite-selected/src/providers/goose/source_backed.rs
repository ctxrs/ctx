use std::{
    collections::BTreeMap,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use chrono::{DateTime, Utc};
use ctx_history_capture_model::normalization::{provider_role, provider_timestamp_seconds};
use ctx_history_core::{
    derive_event_id, derive_session_id, ActivityInvocation, ActivityJsonCapture, ActivityResult,
    ActivityTextCapture, AgentScope, CaptureProvider, CoreActivity, CoreRecord, CoreRecordError,
    EventIdentityInput, EventType, LiteralFactKind, NativeItemKey, NativeSessionKey,
    ProjectionContractError, ProviderDeclaredFact, ProviderNativeSessionRelationship,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceAnchorScope, SourceKey,
    StableEntityId, TypedKey, CORE_ACTIVITY_REVISION,
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
    provider::source_backed::{
        route_error, sqlite_source_route_error, ChangedDocumentSink, CompleteDocumentTree,
        DocumentLeafFingerprint, DocumentRecordSpool, DocumentSourceTerminal, ObservedDocumentLeaf,
        ReplacementDocumentTree, SourceBackedRouteError, SourceBackedRouteErrorKind,
        SourceBackedRouteResult,
    },
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteLogicalSnapshot, SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot,
    },
    CaptureError, SelectedSqliteCaptureBinding, GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
};

mod fingerprint;

use fingerprint::GooseLogicalFingerprint;

pub(super) const GOOSE_SOURCE_ANCHOR_NAMESPACE: &str = "goose.installed-sessions";
pub(super) const GOOSE_SOURCE_ANCHOR_KEY: &str = "selected-platform-sessions-db";
pub(super) const GOOSE_SOURCE_SCHEMA_VARIANT: &str = "goose-sessions-sqlite-v0";
const GOOSE_PARSER_REVISION: &str = "goose-logical-sqlite-v9-closed-facts-agent-scope";
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

impl From<ctx_history_source_io::SourceIoError> for GooseSourceBackedErrorV0 {
    fn from(error: ctx_history_source_io::SourceIoError) -> Self {
        Self::Capture(error.into())
    }
}

impl From<ctx_history_source_sqlite::SqliteIoError> for GooseSourceBackedErrorV0 {
    fn from(error: ctx_history_source_sqlite::SqliteIoError) -> Self {
        Self::Capture(error.into())
    }
}

pub(crate) type GooseSourceBackedResultV0<T> = Result<T, GooseSourceBackedErrorV0>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GooseSourceRouteV0 {
    selected_database: PathBuf,
    platform_root: PathBuf,
}

impl GooseSourceRouteV0 {
    pub fn exact(selected_database: impl Into<PathBuf>, platform_root: impl Into<PathBuf>) -> Self {
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

pub(crate) struct GooseSourceBackedAdapterV0<B> {
    selection: GooseSourceBackedSelectionV0,
    source: SourceKey,
    binding: std::marker::PhantomData<fn() -> B>,
}

impl<B> GooseSourceBackedAdapterV0<B> {
    pub(crate) fn open_scoped(
        selection: GooseSourceBackedSelectionV0,
        source_scope: SourceAnchorScope,
    ) -> GooseSourceBackedResultV0<Self> {
        Ok(Self {
            source: goose_source_key_scoped(source_scope)?,
            selection,
            binding: std::marker::PhantomData,
        })
    }
}

pub(crate) struct GoosePresentAuthority {
    retained: RetainedGooseDirectory,
    snapshot: Mutex<Option<SqliteSourceReadSnapshot>>,
}

pub(crate) enum GooseTreeAuthority {
    Present(Box<GoosePresentAuthority>),
    Missing(RetainedGooseDirectory),
}

impl<B: SelectedSqliteCaptureBinding> ReplacementDocumentTree for GooseSourceBackedAdapterV0<B> {
    type Lifecycle = B::Lifecycle;
    type Spool = B::Spool;
    type RouteControl = B::RouteControl;
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
        Ok(CompleteDocumentTree::new(
            fingerprint,
            vec![ObservedDocumentLeaf::new(
                DocumentLeafFingerprint::new(fingerprint),
                (),
            )],
            GooseTreeAuthority::Present(Box::new(GoosePresentAuthority {
                retained,
                snapshot: Mutex::new(Some(snapshot)),
            })),
        ))
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        _leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_, B::Lifecycle, B::Spool>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let GooseTreeAuthority::Present(authority) = authority else {
            return Err(internal_route_error(
                "Goose lifecycle requested a scan for a missing database",
            ));
        };
        let snapshot = take_snapshot(&authority.snapshot, "Goose")?;
        sink.begin_source(self.source.clone())?;
        let mut sink_failure = None;
        let terminal = scan_goose_logical_snapshot(
            snapshot.connection().map_err(route_error)?,
            &self.source,
            sink,
            &mut sink_failure,
        );
        if let Some(error) = sink_failure {
            return Err(error);
        }
        let terminal = terminal.map_err(route_error)?;
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
    let terminal_fence = snapshot.seal().map_err(sqlite_source_route_error)?;
    authority.retained.revalidate()?;
    terminal_fence
        .revalidate()
        .map_err(sqlite_source_route_error)
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
            Err(ctx_history_source_io::SourceIoError::Io(error))
                if error.kind() == io::ErrorKind::NotFound =>
            {
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
    parent_session_id: Option<StableEntityId>,
    parent_provider_session_id: Option<String>,
    cwd: Option<String>,
}

fn scan_goose_logical_snapshot<L, S>(
    connection: &Connection,
    source: &SourceKey,
    sink: &mut ChangedDocumentSink<'_, '_, L, S>,
    sink_failure: &mut Option<SourceBackedRouteError>,
) -> GooseSourceBackedResultV0<DocumentSourceTerminal>
where
    L: ctx_history_capture_runtime::CaptureLifecycleSink,
    S: DocumentRecordSpool,
{
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
                    parent_session_id: None,
                    parent_provider_session_id: row.parent_session_id,
                    cwd: row.working_dir,
                },
            );
        }
    }
    resolve_goose_session_lineage(source, &mut sessions)?;

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
                GooseMessageCellDisposition::ToolOutput => normalize_goose_native_output(&scanned)?,
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
                .map_err(|error| {
                    let detail = error.to_string();
                    *sink_failure = Some(error);
                    CaptureError::InvalidPayload(detail)
                })?;
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

#[cfg(test)]
pub(super) fn goose_source_key() -> GooseSourceBackedResultV0<SourceKey> {
    goose_source_key_scoped(SourceAnchorScope::Unqualified)
}

pub(super) fn goose_source_key_scoped(
    source_scope: SourceAnchorScope,
) -> GooseSourceBackedResultV0<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        GOOSE_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(GOOSE_SOURCE_ANCHOR_KEY)?,
    )?;
    Ok(SourceKey::derive_scoped(
        CaptureProvider::Goose.as_str(),
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        GOOSE_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
        source_scope,
    )?)
}

pub(super) fn goose_session_id(
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

fn resolve_goose_session_lineage(
    source: &SourceKey,
    sessions: &mut BTreeMap<String, GooseSessionProjection>,
) -> GooseSourceBackedResultV0<()> {
    let session_ids = sessions.keys().cloned().collect::<Vec<_>>();
    for session_identity in session_ids {
        let parent_identity = sessions
            .get(&session_identity)
            .and_then(|session| session.parent_provider_session_id.clone());
        let parent_session_id = parent_identity
            .as_deref()
            .map(|parent| goose_session_id(source, parent))
            .transpose()?;
        let session = sessions
            .get_mut(&session_identity)
            .ok_or(CaptureError::SystemInvariant(
                "Goose session lineage lost a discovered session",
            ))?;
        session.parent_session_id = parent_session_id;
    }
    Ok(())
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
    let (relation, _) = goose_message_locator(event.native_order);
    debug_assert_eq!(relation, GOOSE_LOGICAL_RELATION);
    let native_event_id = event
        .provider_message_identity
        .as_deref()
        .map(TypedKey::utf8)
        .transpose()?;
    let normalized_event_type = event_type(&event);
    let normalized_role = provider_role(Some(&event.role));
    let body = if event.searchable_text.is_empty() {
        format!("{} {}", normalized_event_type.as_str(), event.role)
    } else {
        event.searchable_text.clone()
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
    let mut record = CoreRecord::new_selected(
        event_id,
        session.session_id,
        source.clone(),
        event_sequence,
        normalized_event_type.as_str(),
        GOOSE_PARSER_REVISION,
        body,
    )?;
    record.agent_scope = Some(if session.parent_session_id.is_some() {
        AgentScope::Subagent
    } else {
        AgentScope::Primary
    });
    if let Some(parent_session_id) = session.parent_session_id {
        record.parent_session_id = Some(parent_session_id);
        record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    }
    record.provider_session_id = Some(event.session_identity.clone());
    record.native_event_id = native_event_id;
    record.occurred_at_unix_ms = occurred_at_unix_ms;
    record.role = Some(normalized_role.as_str().to_owned());
    let facts = session
        .cwd
        .as_ref()
        .map(|cwd| ProviderDeclaredFact {
            kind: LiteralFactKind::SessionCwd,
            value: cwd.clone(),
        })
        .into_iter()
        .collect::<Vec<_>>();
    let (provider_call_id, invocation, result) = goose_activity(&event, occurred_at_unix_ms)?;
    if !event.semantic_capture_ambiguous {
        record.content.structured_content = Some(event.content);
    }
    if invocation.is_some() || result.is_some() || !facts.is_empty() {
        record.content.activity = Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id,
            invocation,
            result,
            facts,
        });
    }
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    record.validate_contract()?;
    Ok(record)
}

pub(super) fn goose_activity(
    event: &GooseNativeEvent,
    occurred_at_unix_ms: Option<i64>,
) -> GooseSourceBackedResultV0<(
    Option<TypedKey>,
    Option<ActivityInvocation>,
    Option<ActivityResult>,
)> {
    if event.semantic_capture_ambiguous {
        return Ok((None, None, None));
    }
    let mut requests = Vec::new();
    let mut responses = Vec::new();
    visit_goose_objects(&event.content, &mut |object| match object
        .get("type")
        .and_then(serde_json::Value::as_str)
    {
        Some("toolRequest" | "frontendToolRequest") => requests.push(object.clone()),
        Some("toolResponse") => responses.push(object.clone()),
        _ => {}
    });
    let request = (requests.len() == 1).then(|| &requests[0]);
    let response = (responses.len() == 1).then(|| &responses[0]);
    let request_call_id = request.map_or(GooseAlias::Absent, goose_call_id);
    let response_call_id = response.map_or(GooseAlias::Absent, goose_call_id);
    let call_id = match (request_call_id, response_call_id) {
        (GooseAlias::Conflict, _) | (_, GooseAlias::Conflict) => None,
        (GooseAlias::Unique(left), GooseAlias::Unique(right)) if left != right => None,
        (GooseAlias::Unique(value), _) | (_, GooseAlias::Unique(value)) => Some(value),
        (GooseAlias::Absent, GooseAlias::Absent) => None,
    };
    let Some(call_id) = call_id else {
        return Ok((None, None, None));
    };
    let invocation = request.and_then(|object| {
        let call = object
            .get("toolCall")
            .and_then(serde_json::Value::as_object)
            .unwrap_or(object);
        let GooseAlias::Unique(tool) =
            unique_goose_string_alias(call, &["name", "tool", "toolName"])
        else {
            return None;
        };
        if tool.is_empty() {
            return None;
        }
        let arguments =
            match unique_goose_json_alias(call, &["arguments", "args", "input", "parameters"]) {
                GooseAlias::Absent => ActivityJsonCapture::Absent,
                GooseAlias::Unique(value) => ActivityJsonCapture::Present {
                    value: value.clone(),
                },
                GooseAlias::Conflict => ActivityJsonCapture::Unavailable,
            };
        Some(ActivityInvocation {
            protocol: None,
            server: None,
            tool: tool.to_owned(),
            arguments,
            started_at_unix_ms: occurred_at_unix_ms,
        })
    });
    let result = response.map(|object| ActivityResult {
        status: unique_goose_string(object, &["status", "state", "outcome"]),
        completed_at_unix_ms: occurred_at_unix_ms,
        duration_ns: None,
        text: ActivityTextCapture::NormalizedBody,
        structured_content: ActivityJsonCapture::Omitted {
            reason: "normalized_body_authoritative".to_owned(),
            observed_encoded_bytes: serde_json::to_vec(object)
                .ok()
                .and_then(|encoded| u64::try_from(encoded.len()).ok()),
        },
    });
    if invocation.is_none() && result.is_none() {
        return Ok((None, None, None));
    }
    Ok((Some(TypedKey::utf8(call_id)?), invocation, result))
}

enum GooseAlias<T> {
    Absent,
    Unique(T),
    Conflict,
}

fn goose_call_id(object: &serde_json::Map<String, serde_json::Value>) -> GooseAlias<String> {
    let mut candidates = Vec::new();
    if let Some(call) = object
        .get("toolCall")
        .and_then(serde_json::Value::as_object)
    {
        if let Some(value) = call.get("id") {
            candidates.push(value);
        }
    }
    for key in ["toolCallId", "tool_call_id", "id"] {
        if let Some(value) = object.get(key) {
            candidates.push(value);
        }
    }
    unique_goose_string_values(candidates)
}

fn unique_goose_string_alias(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> GooseAlias<String> {
    unique_goose_string_values(keys.iter().filter_map(|key| object.get(*key)).collect())
}

fn unique_goose_string_values(values: Vec<&serde_json::Value>) -> GooseAlias<String> {
    let mut selected = None;
    for value in values.into_iter().filter(|value| !value.is_null()) {
        let Some(candidate) = value.as_str() else {
            return GooseAlias::Conflict;
        };
        if selected
            .as_ref()
            .is_some_and(|selected| selected != candidate)
        {
            return GooseAlias::Conflict;
        }
        selected = Some(candidate.to_owned());
    }
    selected.map_or(GooseAlias::Absent, GooseAlias::Unique)
}

fn unique_goose_json_alias<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> GooseAlias<&'a serde_json::Value> {
    let mut selected = None;
    for key in keys {
        let Some(candidate) = object.get(*key).filter(|value| !value.is_null()) else {
            continue;
        };
        if selected.is_some_and(|selected| selected != candidate) {
            return GooseAlias::Conflict;
        }
        selected = Some(candidate);
    }
    selected.map_or(GooseAlias::Absent, GooseAlias::Unique)
}

fn unique_goose_string(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<String> {
    let mut selected = None::<&str>;
    for key in keys {
        let Some(candidate) = object.get(*key).and_then(serde_json::Value::as_str) else {
            continue;
        };
        if selected.is_some_and(|selected| selected != candidate) {
            return None;
        }
        selected = Some(candidate);
    }
    selected.map(str::to_owned)
}

fn visit_goose_objects(
    value: &serde_json::Value,
    visitor: &mut impl FnMut(&serde_json::Map<String, serde_json::Value>),
) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                visit_goose_objects(value, visitor);
            }
        }
        serde_json::Value::Object(object) => visitor(object),
        _ => {}
    }
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

#[cfg(test)]
mod timestamp_tests;
