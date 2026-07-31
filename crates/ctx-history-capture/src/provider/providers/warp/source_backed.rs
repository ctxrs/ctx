use std::{
    collections::BTreeMap,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[cfg(test)]
use std::cell::Cell;

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CoreRecord, CoreRecordError,
    EventIdentityInput, NativeItemKey, NativeSessionKey, ProjectionContractError,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey, StableEntityId, TypedKey,
};
use rusqlite::{limits::Limit, Connection};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    nativepath::{
        scan_warp_source_backed_connection, WarpNativeEvent, WarpNativeMessageIdentity,
        WarpNativePage, WarpNativeSession, WarpNativeSink, WarpNativeSourceBackedScan,
    },
    schema::{warp_quote_identifier, WarpSqliteSchema},
};
use crate::{
    common::io::{OpenedProviderSourcePath, ProviderSourceDirectory, ProviderSourceRoot},
    provider::source_backed::{
        family::document::{
            ChangedDocumentSink, CompleteDocumentTree, DocumentLeafFingerprint,
            DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
        },
        route_error, SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
    },
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteLogicalSnapshot, SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot,
    },
    CaptureError, Result as CaptureResult, MAX_PROVIDER_SQLITE_VALUE_BYTES,
    WARP_SQLITE_SOURCE_FORMAT,
};

const WARP_SOURCE_ANCHOR_NAMESPACE: &str = "warp.selected-surface";
const WARP_NATIVE_SESSION_NAMESPACE: &str = "warp.conversation";
const WARP_NATIVE_ITEM_NAMESPACE: &str = "warp.task-message";
const WARP_LOGICAL_SESSION_KIND: &str = "warp-conversation";
const WARP_LOGICAL_ITEM_KIND: &str = "warp-task-message";
const WARP_SOURCE_SCHEMA_VARIANT: &str = "warp-agent-task-protobuf-v1";
const WARP_SOURCE_BACKED_PARSER_REVISION: &str = "warp-source-backed-logical-v1";
const WARP_SCHEMA_EVIDENCE: &[u8] = b"agent_conversations+agent_tasks+unique-task-id-v1";
const WARP_MISSING_TREE_DOMAIN: &[u8] = b"ctx.warp.missing-logical-tree.v1\0";
const WARP_LOGICAL_LEAF_DOMAIN: &[u8] = b"ctx.warp.logical-leaf.v1\0";
const WARP_ORDERING_KEY_MAX_BYTES: usize = 240 * 1024;
const WARP_NATIVE_SQLITE_ROW_OVERHEAD_BYTES: u64 = 64 * 5;
const WARP_NATIVE_SQLITE_CONVERSATION_ROW_OVERHEAD_BYTES: u64 = 64 * 4;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct WarpSourceBackedWork {
    pub(super) snapshot_opens: u64,
    pub(super) source_bytes_copied: u64,
    pub(super) terminal_fences: u64,
    pub(super) terminal_revalidations: u64,
    pub(super) active_snapshots: u64,
    pub(super) max_active_snapshots: u64,
    pub(super) logical_observation_queries: u64,
    pub(super) provider_projections: u64,
    pub(super) projection_queries: u64,
}

#[cfg(test)]
thread_local! {
    static WARP_SOURCE_BACKED_WORK: Cell<WarpSourceBackedWork> =
        Cell::new(WarpSourceBackedWork::default());
}

#[cfg(test)]
pub(super) fn reset_warp_source_backed_work() {
    WARP_SOURCE_BACKED_WORK.set(WarpSourceBackedWork::default());
}

#[cfg(test)]
pub(super) fn warp_source_backed_work() -> WarpSourceBackedWork {
    WARP_SOURCE_BACKED_WORK.get()
}

#[cfg(test)]
fn update_warp_source_backed_work(update: impl FnOnce(&mut WarpSourceBackedWork)) {
    WARP_SOURCE_BACKED_WORK.set({
        let mut work = WARP_SOURCE_BACKED_WORK.get();
        update(&mut work);
        work
    });
}

#[cfg(test)]
pub(super) fn record_warp_projection_query() {
    update_warp_source_backed_work(|work| {
        work.projection_queries = work.projection_queries.saturating_add(1);
    });
}

#[derive(Debug, Error)]
pub(crate) enum WarpSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Warp selected surface key is empty")]
    EmptySurfaceKey,
    #[error("Warp source-backed scan count overflow")]
    CountOverflow,
    #[error("Warp source-backed parser counts do not match its emitted records")]
    ScanCountMismatch,
    #[error("Warp source-backed digest is not canonical lowercase SHA-256")]
    InvalidDigest,
    #[error("Warp source-backed parser emitted empty normalized content")]
    EmptyNormalizedContent,
}

pub(crate) type WarpSourceBackedResultV0<T> = Result<T, WarpSourceBackedErrorV0>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WarpSourceSelectionV0 {
    data_root: PathBuf,
    path: PathBuf,
    surface_key: String,
}

impl WarpSourceSelectionV0 {
    pub(crate) fn new(
        data_root: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
        surface_key: impl Into<String>,
    ) -> WarpSourceBackedResultV0<Self> {
        let surface_key = surface_key.into();
        if surface_key.is_empty() {
            return Err(WarpSourceBackedErrorV0::EmptySurfaceKey);
        }
        Ok(Self {
            data_root: data_root.into(),
            path: path.into(),
            surface_key,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn surface_key(&self) -> &str {
        &self.surface_key
    }
}

pub(crate) fn project_warp_source_backed_v0(
    selection: WarpSourceSelectionV0,
) -> WarpSourceBackedResultV0<WarpReplacementTreeAdapter> {
    Ok(WarpReplacementTreeAdapter {
        source: warp_source_key(&selection)?,
        selection,
    })
}

pub(crate) struct WarpReplacementTreeAdapter {
    selection: WarpSourceSelectionV0,
    source: SourceKey,
}

pub(crate) struct WarpPresentAuthority {
    retained: RetainedWarpDirectory,
    snapshot: Mutex<Option<SqliteSourceReadSnapshot>>,
    terminal_revalidate: Box<dyn Fn() -> bool + Send + Sync>,
}

pub(crate) enum WarpTreeAuthority {
    Present(Box<WarpPresentAuthority>),
    Missing(RetainedWarpDirectory),
}

impl ReplacementDocumentTree for WarpReplacementTreeAdapter {
    type Leaf = ();
    type TreeAuthority = WarpTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        WARP_SOURCE_BACKED_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        self.source.exact_descriptor_eq(source)
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let retained =
            RetainedWarpDirectory::open(&self.selection.data_root, self.selection.path())
                .map_err(route_error)?;
        let Some(snapshot) = retained.open_snapshot()? else {
            let fingerprint = missing_tree_fingerprint(&self.source);
            return Ok(CompleteDocumentTree::new(
                fingerprint,
                Vec::new(),
                WarpTreeAuthority::Missing(retained),
            ));
        };
        let fingerprint = observe_warp_logical_fingerprint(
            snapshot.connection().map_err(route_error)?,
            &self.source,
        )
        .map_err(route_error)?;
        let terminal_revalidate = snapshot.terminal_revalidator();
        Ok(CompleteDocumentTree::new(
            fingerprint,
            vec![ObservedDocumentLeaf::new(
                DocumentLeafFingerprint::new(fingerprint),
                (),
            )],
            WarpTreeAuthority::Present(Box::new(WarpPresentAuthority {
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
        let WarpTreeAuthority::Present(authority) = authority else {
            return Err(internal_route_error(
                "Warp shared lifecycle requested a changed scan for a missing database",
            ));
        };
        let snapshot = take_warp_snapshot(authority)?;
        sink.begin_source(self.source.clone())?;
        let terminal = scan_warp_logical_snapshot(
            snapshot.connection().map_err(route_error)?,
            &self.source,
            self.selection.path(),
            sink,
        )
        .map_err(route_error)?;
        snapshot.revalidate().map_err(route_error)?;
        authority.retained.revalidate()?;
        restore_warp_snapshot(authority, snapshot)?;
        Ok(terminal)
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        let current = match &tree.authority {
            WarpTreeAuthority::Present(authority) => {
                let snapshot = take_warp_snapshot(authority)?;
                finish_warp_authority(authority, snapshot)?;
                tree.tree_fingerprint
            }
            WarpTreeAuthority::Missing(retained) => {
                if retained.open_snapshot()?.is_some() {
                    return Err(source_changed("Warp database appeared"));
                }
                retained.revalidate()?;
                tree.tree_fingerprint
            }
        };
        Ok(current)
    }
}

fn restore_warp_snapshot(
    authority: &WarpPresentAuthority,
    snapshot: SqliteSourceReadSnapshot,
) -> SourceBackedRouteResult<()> {
    let mut slot = authority
        .snapshot
        .lock()
        .map_err(|_| internal_route_error("Warp snapshot lock was poisoned"))?;
    if slot.replace(snapshot).is_some() {
        return Err(internal_route_error(
            "Warp snapshot slot was already occupied",
        ));
    }
    Ok(())
}

fn take_warp_snapshot(
    authority: &WarpPresentAuthority,
) -> SourceBackedRouteResult<SqliteSourceReadSnapshot> {
    authority
        .snapshot
        .lock()
        .map_err(|_| internal_route_error("Warp snapshot lock was poisoned"))?
        .take()
        .ok_or_else(|| internal_route_error("Warp snapshot was already consumed"))
}

fn finish_warp_authority(
    authority: &WarpPresentAuthority,
    snapshot: SqliteSourceReadSnapshot,
) -> SourceBackedRouteResult<()> {
    snapshot.finish().map_err(route_error)?;
    authority.retained.revalidate()?;
    if !(authority.terminal_revalidate)() {
        return Err(source_changed(
            "Warp retained terminal fence changed before publication",
        ));
    }
    #[cfg(test)]
    authority.retained.record_snapshot_work();
    Ok(())
}

pub(crate) struct RetainedWarpDirectory {
    root: ProviderSourceRoot,
    directory: ProviderSourceDirectory,
    sqlite: SqliteSourceDirectoryAuthority,
    leaf: OsString,
}

impl RetainedWarpDirectory {
    fn open(data_root: &Path, path: &Path) -> WarpSourceBackedResultV0<Self> {
        let parent = path.parent().ok_or_else(|| {
            CaptureError::InvalidPayload("Warp SQLite source has no parent directory".to_owned())
        })?;
        let leaf = path.file_name().map(OsString::from).ok_or_else(|| {
            CaptureError::InvalidPayload("Warp SQLite source has no leaf name".to_owned())
        })?;
        let root = ProviderSourceRoot::open(parent)?;
        let directory = root.directory()?;
        let authority_handle = directory.try_clone_authority_handle()?;
        let sqlite = retain_sqlite_source_directory_authority(data_root, &authority_handle, parent)
            .map_err(sqlite_access_error)?;
        let retained = Self {
            root,
            directory,
            sqlite,
            leaf,
        };
        retained.revalidate().map_err(|error| {
            WarpSourceBackedErrorV0::Capture(CaptureError::InvalidPayload(error.detail))
        })?;
        Ok(retained)
    }

    fn open_snapshot(&self) -> SourceBackedRouteResult<Option<SqliteSourceReadSnapshot>> {
        match self.directory.open_child(&self.leaf) {
            Ok(OpenedProviderSourcePath::File(file)) => {
                file.revalidate().map_err(route_error)?;
            }
            Ok(OpenedProviderSourcePath::Directory(_)) => {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::InvalidSource,
                    "Warp SQLite leaf is a directory",
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

    #[cfg(test)]
    fn record_snapshot_work(&self) {
        let counters = self.sqlite.snapshot_counters();
        update_warp_source_backed_work(|work| {
            work.snapshot_opens = work
                .snapshot_opens
                .saturating_add(counters.immutable_snapshot_opens())
                .saturating_add(counters.copied_snapshot_opens());
            work.source_bytes_copied = work
                .source_bytes_copied
                .saturating_add(counters.source_bytes_copied());
            work.terminal_fences = work
                .terminal_fences
                .saturating_add(counters.terminal_fences());
            work.terminal_revalidations = work
                .terminal_revalidations
                .saturating_add(counters.terminal_revalidations());
            work.active_snapshots = work
                .active_snapshots
                .saturating_add(counters.active_snapshots());
            work.max_active_snapshots = work
                .max_active_snapshots
                .max(counters.max_active_snapshots());
        });
    }
}

fn observe_warp_logical_fingerprint(
    connection: &Connection,
    source: &SourceKey,
) -> WarpSourceBackedResultV0<[u8; 32]> {
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| WarpSourceBackedErrorV0::CountOverflow)?;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let schema = WarpSqliteSchema::detect(connection)?;
    let invalid_rowid: bool = connection.query_row(
        "select exists(select 1 from agent_conversations where rowid <= 0)
             or exists(select 1 from agent_tasks where rowid <= 0)",
        [],
        |row| row.get(0),
    )?;
    #[cfg(test)]
    update_warp_source_backed_work(|work| {
        work.logical_observation_queries = work.logical_observation_queries.saturating_add(1);
    });
    if invalid_rowid {
        return Err(WarpSourceBackedErrorV0::Capture(
            CaptureError::InvalidPayload(
                "Warp source-backed paging requires positive 64-bit source rowids".to_owned(),
            ),
        ));
    }
    let mut digest = Sha256::new();
    digest.update(WARP_LOGICAL_LEAF_DOMAIN);
    digest.update(source.exact_descriptor_digest());
    hash_bytes(&mut digest, schema.task_keyset_index.as_bytes())?;
    observe_warp_conversation_rows(connection, &mut digest)?;
    observe_warp_task_rows(connection, &schema, &mut digest)?;
    Ok(digest.finalize().into())
}

fn observe_warp_conversation_rows(
    connection: &Connection,
    digest: &mut Sha256,
) -> WarpSourceBackedResultV0<()> {
    let maximum = u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| WarpSourceBackedErrorV0::CountOverflow)?;
    let hydration_limit = maximum
        .checked_sub(WARP_NATIVE_SQLITE_CONVERSATION_ROW_OVERHEAD_BYTES)
        .ok_or(WarpSourceBackedErrorV0::CountOverflow)?;
    let hydration_limit =
        i64::try_from(hydration_limit).map_err(|_| WarpSourceBackedErrorV0::CountOverflow)?;
    let _guard = crate::provider::sqlite::SqliteLengthPreflightGuard::new(connection);
    let mut statement = connection.prepare(
        "select rowid, \
                typeof(conversation_id), coalesce(octet_length(conversation_id), 0), \
                typeof(conversation_data), coalesce(octet_length(conversation_data), 0), \
                typeof(last_modified_at), coalesce(octet_length(last_modified_at), 0), \
                case when typeof(conversation_id) = 'text' \
                           and typeof(conversation_data) = 'text' \
                           and typeof(last_modified_at) = 'text' \
                           and coalesce(octet_length(conversation_id), 0) \
                             + coalesce(octet_length(conversation_data), 0) \
                             + coalesce(octet_length(last_modified_at), 0) <= ?1 \
                     then conversation_id end, \
                case when typeof(conversation_id) = 'text' \
                           and typeof(conversation_data) = 'text' \
                           and typeof(last_modified_at) = 'text' \
                           and coalesce(octet_length(conversation_id), 0) \
                             + coalesce(octet_length(conversation_data), 0) \
                             + coalesce(octet_length(last_modified_at), 0) <= ?1 \
                     then conversation_data end, \
                case when typeof(conversation_id) = 'text' \
                           and typeof(conversation_data) = 'text' \
                           and typeof(last_modified_at) = 'text' \
                           and coalesce(octet_length(conversation_id), 0) \
                             + coalesce(octet_length(conversation_data), 0) \
                             + coalesce(octet_length(last_modified_at), 0) <= ?1 \
                     then last_modified_at end \
         from agent_conversations where rowid > 0 order by rowid",
    )?;
    let mut rows = statement.query([hydration_limit])?;
    #[cfg(test)]
    update_warp_source_backed_work(|work| {
        work.logical_observation_queries = work.logical_observation_queries.saturating_add(1);
    });
    while let Some(row) = rows.next()? {
        digest.update(b"conversation\0");
        digest.update(row.get::<_, i64>(0)?.to_be_bytes());
        for offset in [1, 3, 5] {
            hash_text(digest, &row.get::<_, String>(offset)?);
            digest.update(row.get::<_, i64>(offset + 1)?.to_be_bytes());
        }
        for offset in [7, 8, 9] {
            hash_optional_text(digest, row.get::<_, Option<String>>(offset)?.as_deref());
        }
    }
    Ok(())
}

fn observe_warp_task_rows(
    connection: &Connection,
    schema: &WarpSqliteSchema,
    digest: &mut Sha256,
) -> WarpSourceBackedResultV0<()> {
    let index = warp_quote_identifier(&schema.task_keyset_index);
    let representable = format!(
        "typeof(t.conversation_id) = 'text' \
         and typeof(t.task_id) = 'text' \
         and typeof(t.task) = 'blob' \
         and typeof(t.last_modified_at) = 'text' \
         and coalesce(octet_length(t.conversation_id), 0) > 0 \
         and coalesce(octet_length(t.task_id), 0) > 0 \
         and coalesce(octet_length(t.task_id), 0) <= {WARP_ORDERING_KEY_MAX_BYTES} \
         and coalesce(octet_length(t.conversation_id), 0) \
             + coalesce(octet_length(t.task_id), 0) \
             + coalesce(octet_length(t.task), 0) \
             + coalesce(octet_length(t.last_modified_at), 0) \
             + {WARP_NATIVE_SQLITE_ROW_OVERHEAD_BYTES} \
             <= {MAX_PROVIDER_SQLITE_VALUE_BYTES}"
    );
    let _guard = crate::provider::sqlite::SqliteLengthPreflightGuard::new(connection);
    let mut statement = connection.prepare(&format!(
        "select t.rowid, \
                typeof(t.conversation_id), coalesce(octet_length(t.conversation_id), 0), \
                typeof(t.task_id), coalesce(octet_length(t.task_id), 0), \
                typeof(t.task), coalesce(octet_length(t.task), 0), \
                typeof(t.last_modified_at), coalesce(octet_length(t.last_modified_at), 0), \
                case when {representable} then t.conversation_id end, \
                case when {representable} then t.task_id end, \
                case when {representable} then t.task end, \
                case when {representable} then t.last_modified_at end \
         from agent_tasks t indexed by {index} \
         order by t.task_id collate binary"
    ))?;
    let mut rows = statement.query([])?;
    #[cfg(test)]
    update_warp_source_backed_work(|work| {
        work.logical_observation_queries = work.logical_observation_queries.saturating_add(1);
    });
    while let Some(row) = rows.next()? {
        digest.update(b"task\0");
        digest.update(row.get::<_, i64>(0)?.to_be_bytes());
        for offset in [1, 3, 5, 7] {
            hash_text(digest, &row.get::<_, String>(offset)?);
            digest.update(row.get::<_, i64>(offset + 1)?.to_be_bytes());
        }
        hash_optional_text(digest, row.get::<_, Option<String>>(9)?.as_deref());
        hash_optional_text(digest, row.get::<_, Option<String>>(10)?.as_deref());
        let task = row.get::<_, Option<Vec<u8>>>(11)?;
        match task {
            Some(task) => {
                digest.update([1]);
                hash_bytes(digest, &task)?;
            }
            None => digest.update([0]),
        }
        hash_optional_text(digest, row.get::<_, Option<String>>(12)?.as_deref());
    }
    Ok(())
}

fn scan_warp_logical_snapshot(
    connection: &Connection,
    source: &SourceKey,
    path: &Path,
    sink: &mut ChangedDocumentSink<'_, '_>,
) -> WarpSourceBackedResultV0<DocumentSourceTerminal> {
    #[cfg(test)]
    update_warp_source_backed_work(|work| {
        work.provider_projections = work.provider_projections.saturating_add(1);
        work.projection_queries = work.projection_queries.saturating_add(1);
    });
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| WarpSourceBackedErrorV0::CountOverflow)?;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let mut projection =
        WarpProjectionSink::new(source.clone(), path.to_string_lossy().into_owned(), sink);
    let native_scan = scan_warp_source_backed_connection(connection, &mut projection)?;
    let counts = scan_counts(&native_scan, &projection)?;
    let content_digest = parse_hex_digest(&native_scan.source_integrity_digest)?;
    let logical = SqliteLogicalSnapshot::new(
        WARP_SOURCE_BACKED_PARSER_REVISION,
        WARP_SCHEMA_EVIDENCE,
        content_digest,
        counts,
    );
    let certificate = logical.certify(source.clone())?;
    Ok(DocumentSourceTerminal {
        source: source.clone(),
        opening: certificate.observation().clone(),
        closing: certificate.observation().clone(),
        parser_revision: WARP_SOURCE_BACKED_PARSER_REVISION,
        content_digest,
        counts,
    })
}

struct WarpProjectionSink<'changed, 'sink, 'writer> {
    source: SourceKey,
    source_path: String,
    session_lineage: BTreeMap<String, WarpSessionLineage>,
    sink: &'changed mut ChangedDocumentSink<'sink, 'writer>,
    indexed_documents: u64,
    rejected_records: u64,
    ignored_records: u64,
}

struct WarpSessionLineage {
    parent_conversation_id: Option<String>,
    root_conversation_id: String,
}

impl<'changed, 'sink, 'writer> WarpProjectionSink<'changed, 'sink, 'writer> {
    fn new(
        source: SourceKey,
        source_path: String,
        sink: &'changed mut ChangedDocumentSink<'sink, 'writer>,
    ) -> Self {
        Self {
            source,
            source_path,
            session_lineage: BTreeMap::new(),
            sink,
            indexed_documents: 0,
            rejected_records: 0,
            ignored_records: 0,
        }
    }
}

impl WarpNativeSink for WarpProjectionSink<'_, '_, '_> {
    fn push_page(&mut self, page: WarpNativePage) -> CaptureResult<()> {
        let WarpNativePage {
            sessions,
            hierarchy_edges,
            events,
            rejections,
            ..
        } = page;
        self.rejected_records = checked_add(
            self.rejected_records,
            u64::try_from(rejections.len())
                .map_err(|_| CaptureError::SystemInvariant("Warp rejection count exceeds u64"))?,
        )
        .map_err(source_backed_capture_error)?;
        let ignored = sessions
            .len()
            .checked_add(hierarchy_edges.len())
            .and_then(|count| u64::try_from(count).ok())
            .ok_or(CaptureError::SystemInvariant(
                "Warp ignored count exceeds u64",
            ))?;
        self.ignored_records =
            self.ignored_records
                .checked_add(ignored)
                .ok_or(CaptureError::SystemInvariant(
                    "Warp ignored count overflowed",
                ))?;
        for session in sessions {
            let conversation_id = session.conversation_id.clone();
            if self
                .session_lineage
                .insert(conversation_id, WarpSessionLineage::from(session))
                .is_some()
            {
                return Err(CaptureError::SystemInvariant(
                    "Warp parser repeated a session",
                ));
            }
        }
        for event in events {
            let lineage = self
                .session_lineage
                .get(&event.identity.conversation_id)
                .ok_or(CaptureError::SystemInvariant(
                    "Warp event has no session lineage",
                ))?;
            let document = core_record(&self.source, &self.source_path, lineage, event)
                .map_err(source_backed_capture_error)?;
            self.sink
                .emit_core_record(document)
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
            self.indexed_documents =
                self.indexed_documents
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Warp indexed document count overflowed",
                    ))?;
        }
        Ok(())
    }
}

impl From<WarpNativeSession> for WarpSessionLineage {
    fn from(session: WarpNativeSession) -> Self {
        Self {
            parent_conversation_id: session.parent_conversation_id,
            root_conversation_id: session.root_conversation_id,
        }
    }
}

fn core_record(
    source: &SourceKey,
    _source_path: &str,
    lineage: &WarpSessionLineage,
    event: WarpNativeEvent,
) -> WarpSourceBackedResultV0<CoreRecord> {
    let session_id = warp_session_id(source, &event.identity.conversation_id)?;
    let parent_session_id = lineage
        .parent_conversation_id
        .as_deref()
        .map(|parent| warp_session_id(source, parent))
        .transpose()?;
    let root_session_id = warp_session_id(source, &lineage.root_conversation_id)?;
    let is_primary = parent_session_id.is_none();
    let message_key = match &event.identity.message {
        WarpNativeMessageIdentity::ProviderId(message_id) => TypedKey::composite(vec![
            TypedKey::utf8("provider-id")?,
            TypedKey::utf8(message_id)?,
        ])?,
        WarpNativeMessageIdentity::MessageOrdinal(ordinal) => TypedKey::composite(vec![
            TypedKey::utf8("ordinal")?,
            TypedKey::U64(u64::from(*ordinal)),
        ])?,
    };
    let item_key = NativeItemKey::composite(
        WARP_NATIVE_ITEM_NAMESPACE,
        vec![TypedKey::utf8(&event.identity.task_id)?, message_key],
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: WARP_LOGICAL_ITEM_KIND,
        native_item_key: &item_key,
        subrecord_selector: None,
    })?;
    let native_event_id = TypedKey::composite(vec![
        TypedKey::utf8(event.identity.task_id.clone())?,
        TypedKey::U64(u64::from(event.native_order.message_ordinal)),
    ])?;
    let body = if event.lexical_body.is_empty() {
        event.kind.to_owned()
    } else {
        event.lexical_body
    };
    if body.is_empty() {
        return Err(WarpSourceBackedErrorV0::EmptyNormalizedContent);
    }
    let is_tool = matches!(
        event.event_type,
        ctx_history_core::EventType::ToolCall
            | ctx_history_core::EventType::ToolOutput
            | ctx_history_core::EventType::CommandOutput
    );
    let native_tool = is_tool.then(|| {
        serde_json::json!({
            "kind": event.kind,
            "request_id": event.request_id,
            "call_id": event.call_id,
            "result_outcome": event.result_outcome,
        })
    });
    let agent_type = if is_primary {
        AgentType::Primary
    } else {
        AgentType::Subagent
    };
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        root_session_id,
        source.clone(),
        event.native_order.provider_event_index,
        event.event_type.as_str(),
        agent_type.as_str(),
        is_primary,
        WARP_SOURCE_BACKED_PARSER_REVISION,
        body,
    )?;
    record.parent_session_id = parent_session_id;
    record.provider_session_id = Some(event.identity.conversation_id);
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = event.occurred_at.map(|value| value.timestamp_millis());
    record.role = event.role.map(|role| role.as_str().to_owned());
    if let Some(native_tool) = native_tool {
        record.content.structured_content = Some(serde_json::json!({
            "provider_native_tool": native_tool,
        }));
    }
    record.validate_contract()?;
    Ok(record)
}

fn warp_session_id(
    source: &SourceKey,
    conversation_id: &str,
) -> WarpSourceBackedResultV0<StableEntityId> {
    let session_key = NativeSessionKey::native_id(
        WARP_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(conversation_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: WARP_LOGICAL_SESSION_KIND,
        native_session_key: &session_key,
    })?)
}

fn warp_source_key(selection: &WarpSourceSelectionV0) -> WarpSourceBackedResultV0<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        WARP_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(selection.surface_key())?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::Warp.as_str(),
        WARP_SQLITE_SOURCE_FORMAT,
        WARP_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn scan_counts(
    native_scan: &WarpNativeSourceBackedScan,
    sink: &WarpProjectionSink<'_, '_, '_>,
) -> WarpSourceBackedResultV0<ScannedSourceCounts> {
    let retained_records = sink.indexed_documents;
    if retained_records != native_scan.counters.retained_events
        || u64::try_from(sink.session_lineage.len())
            .map_err(|_| WarpSourceBackedErrorV0::CountOverflow)?
            != native_scan.counters.sessions_retained
        || sink.ignored_records
            != native_scan
                .counters
                .sessions_retained
                .checked_add(native_scan.counters.hierarchy_edges)
                .ok_or(WarpSourceBackedErrorV0::CountOverflow)?
    {
        return Err(WarpSourceBackedErrorV0::ScanCountMismatch);
    }
    let complete_records = retained_records
        .checked_add(sink.rejected_records)
        .and_then(|count| count.checked_add(sink.ignored_records))
        .ok_or(WarpSourceBackedErrorV0::CountOverflow)?;
    let conversation_bytes = native_scan
        .counters
        .conversation_rows
        .checked_mul((b"conversation\0".len() + 32) as u64)
        .ok_or(WarpSourceBackedErrorV0::CountOverflow)?;
    let task_bytes = native_scan
        .counters
        .task_rows
        .checked_mul((b"task\0".len() + 32) as u64)
        .ok_or(WarpSourceBackedErrorV0::CountOverflow)?;
    Ok(ScannedSourceCounts {
        complete_records,
        retained_records,
        rejected_records: sink.rejected_records,
        ignored_records: sink.ignored_records,
        indexed_documents: retained_records,
        certified_bytes: conversation_bytes
            .checked_add(task_bytes)
            .ok_or(WarpSourceBackedErrorV0::CountOverflow)?,
    })
}

fn missing_tree_fingerprint(source: &SourceKey) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(WARP_MISSING_TREE_DOMAIN);
    digest.update(source.exact_descriptor_digest());
    digest.finalize().into()
}

fn checked_add(left: u64, right: u64) -> WarpSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(WarpSourceBackedErrorV0::CountOverflow)
}

fn hash_bytes(digest: &mut Sha256, value: &[u8]) -> WarpSourceBackedResultV0<()> {
    digest.update(
        u64::try_from(value.len())
            .map_err(|_| WarpSourceBackedErrorV0::CountOverflow)?
            .to_be_bytes(),
    );
    digest.update(value);
    Ok(())
}

fn hash_text(digest: &mut Sha256, value: &str) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(value.as_bytes());
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

fn parse_hex_digest(value: &str) -> WarpSourceBackedResultV0<[u8; 32]> {
    digest_bytes(value)
}

fn digest_bytes(value: &str) -> WarpSourceBackedResultV0<[u8; 32]> {
    if value.len() != 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
    {
        return Err(WarpSourceBackedErrorV0::InvalidDigest);
    }
    let mut digest = [0_u8; 32];
    for (index, slot) in digest.iter_mut().enumerate() {
        let offset = index * 2;
        *slot = u8::from_str_radix(&value[offset..offset + 2], 16)
            .map_err(|_| WarpSourceBackedErrorV0::InvalidDigest)?;
    }
    Ok(digest)
}

fn source_changed(detail: &str) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn internal_route_error(detail: &str) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

fn sqlite_access_error(error: crate::provider_sources::SqliteSourceAccessError) -> CaptureError {
    CaptureError::SystemIo {
        operation: "accessing a retained Warp SQLite source",
        source: io::Error::other(error),
    }
}

fn source_backed_capture_error(error: WarpSourceBackedErrorV0) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}
