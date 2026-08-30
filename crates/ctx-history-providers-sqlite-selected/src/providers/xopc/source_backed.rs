use std::{
    collections::BTreeMap,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use chrono::{DateTime, Utc};
use ctx_history_capture_model::normalization::provider_timestamp_value;
use ctx_history_core::{
    admit_provider_declared_fact, derive_event_id, derive_session_id, ActivityInvocation,
    ActivityJsonCapture, ActivityResult, ActivityTextCapture, AgentScope, CaptureProvider,
    CoreActivity, CoreRecord, CoreRecordError, EventIdentityInput, EventRole, EventType,
    LiteralFactKind, NativeItemKey, NativeSessionKey, ProjectionContractError, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceAnchorScope, SourceKey, StableEntityId, TypedKey,
    CORE_ACTIVITY_REVISION,
};
use rusqlite::{params_from_iter, Connection};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    common::io::{OpenedProviderSourcePath, ProviderSourceDirectory, ProviderSourceRoot},
    document_inventory_authority,
    provider::{
        source_backed::{
            route_error, sqlite_source_route_error, ChangedDocumentSink, CompleteDocumentTree,
            DocumentLeafFingerprint, DocumentRecordSpool, DocumentSourceTerminal,
            ObservedDocumentLeaf, ReplacementDocumentTree, SourceBackedRouteDriver,
            SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
        },
        sqlite::{
            ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
            SqliteLengthPreflightGuard,
        },
    },
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteLogicalSnapshot, SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot,
    },
    CaptureError, SelectedSqliteCaptureBinding, XOPC_SESSIONS_SQLITE_SOURCE_FORMAT,
};

const XOPC_SOURCE_ANCHOR_NAMESPACE: &str = "xopc.installed-history";
const XOPC_SOURCE_ANCHOR_KEY: &str = "selected-state-database";
const XOPC_SOURCE_SCHEMA_VARIANT: &str = "xopc-transcripts-sqlite-v1";
const XOPC_PARSER_REVISION: &str = "xopc-logical-sqlite-v1";
const XOPC_NATIVE_SESSION_NAMESPACE: &str = "xopc.transcript";
const XOPC_NATIVE_EVENT_NAMESPACE: &str = "xopc.transcript-entry";
const XOPC_LOGICAL_SESSION_KIND: &str = "xopc-transcript";
const XOPC_LOGICAL_EVENT_KIND: &str = "xopc-transcript-event";
const XOPC_CONTENT_DIGEST_DOMAIN: &[u8] = b"ctx.xopc.logical-content.v1\0";
const XOPC_FINGERPRINT_DOMAIN: &[u8] = b"ctx.xopc.logical-fingerprint.v1\0";
const XOPC_MISSING_TREE_DOMAIN: &[u8] = b"ctx.xopc.missing-tree.v1\0";
const XOPC_SCHEMA_EVIDENCE: &[u8] = b"xopc-required-transcript-schema-v1";
const XOPC_PAGE_ROWS: usize = 64;
const XOPC_PAGE_BYTES: u64 = 8 * 1024 * 1024;
const XOPC_ROW_OVERHEAD_BYTES: u64 = 4 * 1024;
const XOPC_SUBRECORD_STRIDE: u64 = 1 << 16;
const MAX_LINKAGE_BYTES: usize = 16 * 1024;

#[derive(Debug, Error)]
pub(crate) enum XopcSourceBackedError {
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
    #[error("XOPC source-backed count overflow")]
    CountOverflow,
}

impl From<ctx_history_source_io::SourceIoError> for XopcSourceBackedError {
    fn from(error: ctx_history_source_io::SourceIoError) -> Self {
        Self::Capture(error.into())
    }
}

impl From<ctx_history_source_sqlite::SqliteIoError> for XopcSourceBackedError {
    fn from(error: ctx_history_source_sqlite::SqliteIoError) -> Self {
        Self::Capture(error.into())
    }
}

type XopcResult<T> = Result<T, XopcSourceBackedError>;

pub(crate) struct XopcSourceBackedAdapter<B> {
    data_root: PathBuf,
    path: PathBuf,
    source: SourceKey,
    binding: std::marker::PhantomData<fn() -> B>,
}

impl<B> XopcSourceBackedAdapter<B> {
    pub(crate) fn open(
        data_root: &Path,
        path: &Path,
        source_scope: SourceAnchorScope,
    ) -> XopcResult<Self> {
        Ok(Self {
            data_root: data_root.to_path_buf(),
            path: absolute_path(path)?,
            source: xopc_source_key_scoped(source_scope)?,
            binding: std::marker::PhantomData,
        })
    }
}

pub(crate) struct XopcPresentAuthority {
    retained: RetainedXopcDirectory,
    snapshot: Mutex<Option<SqliteSourceReadSnapshot>>,
}

pub(crate) enum XopcTreeAuthority {
    Present(Box<XopcPresentAuthority>),
    Missing(RetainedXopcDirectory),
}

impl<B: SelectedSqliteCaptureBinding> ReplacementDocumentTree for XopcSourceBackedAdapter<B> {
    type Lifecycle = B::Lifecycle;
    type Spool = B::Spool;
    type RouteControl = B::RouteControl;
    type Leaf = ();
    type TreeAuthority = XopcTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        XOPC_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        self.source.exact_descriptor_eq(source)
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let retained =
            RetainedXopcDirectory::open(&self.data_root, &self.path).map_err(route_error)?;
        let Some(snapshot) = retained.open_snapshot()? else {
            let fingerprint = missing_tree_fingerprint(&self.source);
            return Ok(CompleteDocumentTree::new(
                fingerprint,
                Vec::new(),
                XopcTreeAuthority::Missing(retained),
            ));
        };
        let fingerprint =
            observe_xopc_logical_fingerprint(snapshot.connection().map_err(route_error)?)
                .map_err(route_error)?;
        Ok(CompleteDocumentTree::new(
            fingerprint,
            vec![ObservedDocumentLeaf::new(
                DocumentLeafFingerprint::new(fingerprint),
                (),
            )],
            XopcTreeAuthority::Present(Box::new(XopcPresentAuthority {
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
        let XopcTreeAuthority::Present(authority) = authority else {
            return Err(internal_route_error(
                "XOPC lifecycle requested a scan for a missing database",
            ));
        };
        let snapshot = take_snapshot(&authority.snapshot)?;
        sink.begin_source(self.source.clone())?;
        let mut sink_failure = None;
        let terminal = scan_xopc_logical_snapshot(
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
        restore_snapshot(&authority.snapshot, snapshot)?;
        Ok(terminal)
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        match &tree.authority {
            XopcTreeAuthority::Present(authority) => {
                let snapshot = take_snapshot(&authority.snapshot)?;
                let terminal_fence = snapshot.seal().map_err(sqlite_source_route_error)?;
                authority.retained.revalidate()?;
                terminal_fence
                    .revalidate()
                    .map_err(sqlite_source_route_error)?;
            }
            XopcTreeAuthority::Missing(retained) => {
                if retained.open_snapshot()?.is_some() {
                    return Err(source_changed("XOPC database appeared"));
                }
                retained.revalidate()?;
            }
        }
        Ok(tree.tree_fingerprint)
    }
}

pub(crate) fn source_backed_driver_scoped<B: SelectedSqliteCaptureBinding>(
    source_path: &Path,
    data_root: &Path,
    source_scope: SourceAnchorScope,
) -> XopcResult<SourceBackedRouteDriver<B::Lifecycle, B::RouteControl>> {
    let adapter = XopcSourceBackedAdapter::<B>::open(data_root, source_path, source_scope)?;
    Ok(
        ctx_history_capture_runtime::replacement_document_tree_driver(
            document_inventory_authority(
                CaptureProvider::Xopc.as_str(),
                XOPC_SESSIONS_SQLITE_SOURCE_FORMAT,
                source_path,
            ),
            adapter,
        ),
    )
}

fn take_snapshot(
    snapshot: &Mutex<Option<SqliteSourceReadSnapshot>>,
) -> SourceBackedRouteResult<SqliteSourceReadSnapshot> {
    snapshot
        .lock()
        .map_err(|_| internal_route_error("XOPC snapshot lock was poisoned"))?
        .take()
        .ok_or_else(|| internal_route_error("XOPC snapshot was already consumed"))
}

fn restore_snapshot(
    slot: &Mutex<Option<SqliteSourceReadSnapshot>>,
    snapshot: SqliteSourceReadSnapshot,
) -> SourceBackedRouteResult<()> {
    let mut slot = slot
        .lock()
        .map_err(|_| internal_route_error("XOPC snapshot lock was poisoned"))?;
    if slot.replace(snapshot).is_some() {
        return Err(internal_route_error(
            "XOPC snapshot slot was already occupied",
        ));
    }
    Ok(())
}

pub(crate) struct RetainedXopcDirectory {
    directory: ProviderSourceDirectory,
    sqlite: SqliteSourceDirectoryAuthority,
    leaf: OsString,
}

impl RetainedXopcDirectory {
    fn open(data_root: &Path, path: &Path) -> XopcResult<Self> {
        let parent = path.parent().ok_or_else(|| {
            CaptureError::InvalidPayload("XOPC SQLite source has no parent directory".to_owned())
        })?;
        let leaf = path.file_name().map(OsString::from).ok_or_else(|| {
            CaptureError::InvalidPayload("XOPC SQLite source has no leaf name".to_owned())
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
                    "XOPC SQLite leaf is a directory",
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
            .map_err(sqlite_source_route_error)?;
        self.revalidate()?;
        Ok(Some(snapshot))
    }

    fn revalidate(&self) -> SourceBackedRouteResult<()> {
        self.sqlite.revalidate().map_err(sqlite_source_route_error)
    }
}

fn validate_schema(connection: &Connection) -> XopcResult<()> {
    for (table, required) in [
        ("sessions", &["session_key", "session_id", "agent_id"][..]),
        (
            "transcripts",
            &[
                "session_id",
                "session_key",
                "status",
                "archive_reason",
                "created_at",
                "archived_at",
                "cwd",
            ][..],
        ),
        (
            "transcript_entries",
            &[
                "entry_id",
                "session_id",
                "seq",
                "entry_kind",
                "role",
                "payload_json",
                "created_at",
            ][..],
        ),
    ] {
        if !sqlite_table_exists(connection, table)? {
            return Err(CaptureError::UnsupportedSchema(format!(
                "XOPC database is missing required table {table}"
            ))
            .into());
        }
        let columns = sqlite_table_columns(connection, table)?;
        ensure_sqlite_table_columns(&columns, table, required).map_err(|_| {
            CaptureError::UnsupportedSchema(format!(
                "XOPC {table} table is missing required columns"
            ))
        })?;
    }
    Ok(())
}

fn absolute_path(path: &Path) -> XopcResult<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

#[derive(Clone)]
struct XopcTranscript {
    session_id: String,
    session_key: String,
    status: String,
    archive_reason: Option<String>,
    created_at: i64,
    archived_at: Option<i64>,
    cwd: String,
}

impl XopcTranscript {
    fn canonical_bytes(&self) -> XopcResult<u64> {
        let string_bytes = [
            self.session_id.len(),
            self.session_key.len(),
            self.status.len(),
            self.archive_reason.as_ref().map_or(0, String::len),
            self.cwd.len(),
        ]
        .into_iter()
        .try_fold(0_u64, |total, value| {
            checked_add(
                total,
                u64::try_from(value).map_err(|_| XopcSourceBackedError::CountOverflow)?,
            )
        })?;
        checked_add(string_bytes, 32)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct EntryKeyset {
    started: bool,
    rowid: i64,
}

struct XopcEntryCell {
    rowid: i64,
    entry_id: String,
    session_id: String,
    seq: i64,
    entry_kind: String,
    role: Option<String>,
    payload_json: Option<String>,
    created_at: i64,
    payload_bytes: u64,
}

impl XopcEntryCell {
    fn canonical_bytes(&self) -> XopcResult<u64> {
        let strings = [
            self.entry_id.len(),
            self.session_id.len(),
            self.entry_kind.len(),
            self.role.as_ref().map_or(0, String::len),
        ]
        .into_iter()
        .try_fold(0_u64, |total, value| {
            checked_add(
                total,
                u64::try_from(value).map_err(|_| XopcSourceBackedError::CountOverflow)?,
            )
        })?;
        checked_add(checked_add(strings, self.payload_bytes)?, 32)
    }
}

struct EntryCandidate {
    rowid: i64,
    payload_bytes: u64,
}

fn read_transcripts(connection: &Connection) -> XopcResult<BTreeMap<String, XopcTranscript>> {
    let mut statement = connection.prepare(
        "select cast(session_id as text), cast(session_key as text), cast(status as text),
                cast(archive_reason as text), cast(created_at as integer),
                cast(archived_at as integer), cast(cwd as text)
         from transcripts
         order by cast(session_id as text)",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(XopcTranscript {
            session_id: row.get(0)?,
            session_key: row.get(1)?,
            status: row.get(2)?,
            archive_reason: row.get(3)?,
            created_at: row.get(4)?,
            archived_at: row.get(5)?,
            cwd: row.get(6)?,
        })
    })?;
    let mut transcripts = BTreeMap::new();
    for transcript in rows {
        let transcript = transcript?;
        if transcript.session_id.is_empty()
            || transcript.session_id.len() > MAX_LINKAGE_BYTES
            || transcript.session_key.len() > MAX_LINKAGE_BYTES
        {
            continue;
        }
        transcripts.insert(transcript.session_id.clone(), transcript);
    }
    Ok(transcripts)
}

fn read_entry_page(connection: &Connection, keyset: EntryKeyset) -> XopcResult<Vec<XopcEntryCell>> {
    let _guard = SqliteLengthPreflightGuard::new(connection);
    let mut statement = connection.prepare(
        "select rowid, length(cast(payload_json as blob))
         from transcript_entries
         where (?1 = 0 or rowid > ?2)
         order by rowid
         limit ?3",
    )?;
    let candidates = statement
        .query_map(
            [
                i64::from(keyset.started),
                keyset.rowid,
                XOPC_PAGE_ROWS as i64,
            ],
            |row| {
                let length = row.get::<_, i64>(1)?;
                Ok(EntryCandidate {
                    rowid: row.get(0)?,
                    payload_bytes: u64::try_from(length).unwrap_or(u64::MAX),
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    drop(_guard);

    let mut selected = Vec::new();
    let mut loaded_bytes = 0_u64;
    for candidate in candidates {
        let retained = checked_add(candidate.payload_bytes, XOPC_ROW_OVERHEAD_BYTES)?;
        let next = checked_add(loaded_bytes, retained)?;
        if !selected.is_empty() && next > XOPC_PAGE_BYTES {
            break;
        }
        loaded_bytes = next;
        selected.push(candidate);
    }
    let rowids = selected
        .iter()
        .filter(|candidate| {
            candidate.payload_bytes <= crate::MAX_PROVIDER_SQLITE_VALUE_BYTES as u64
        })
        .map(|candidate| candidate.rowid)
        .collect::<Vec<_>>();
    let mut values = BTreeMap::new();
    if !rowids.is_empty() {
        let placeholders = (1..=rowids.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "select rowid, cast(entry_id as text), cast(session_id as text),
                    cast(seq as integer), cast(entry_kind as text), cast(role as text),
                    cast(payload_json as text), cast(created_at as integer)
             from transcript_entries
             where rowid in ({placeholders})"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(&rowids), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                (
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                ),
            ))
        })?;
        values = rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    }

    selected
        .into_iter()
        .map(|candidate| {
            let payload_bytes = candidate.payload_bytes;
            let value = values.remove(&candidate.rowid);
            let retained = value.is_some();
            let (entry_id, session_id, seq, entry_kind, role, payload_json, created_at) = value
                .unwrap_or_else(|| {
                    (
                        String::new(),
                        String::new(),
                        0,
                        String::new(),
                        None,
                        String::new(),
                        0,
                    )
                });
            Ok(XopcEntryCell {
                rowid: candidate.rowid,
                entry_id,
                session_id,
                seq,
                entry_kind,
                role,
                payload_json: retained.then_some(payload_json),
                created_at,
                payload_bytes,
            })
        })
        .collect()
}

fn hash_transcript(hasher: &mut Sha256, transcript: &XopcTranscript) {
    hash_text(hasher, &transcript.session_id);
    hash_text(hasher, &transcript.session_key);
    hash_text(hasher, &transcript.status);
    hash_optional_text(hasher, transcript.archive_reason.as_deref());
    hasher.update(transcript.created_at.to_le_bytes());
    match transcript.archived_at {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
    hash_text(hasher, &transcript.cwd);
}

fn hash_entry(hasher: &mut Sha256, entry: &XopcEntryCell) {
    hasher.update(entry.rowid.to_le_bytes());
    hash_text(hasher, &entry.entry_id);
    hash_text(hasher, &entry.session_id);
    hasher.update(entry.seq.to_le_bytes());
    hash_text(hasher, &entry.entry_kind);
    hash_optional_text(hasher, entry.role.as_deref());
    hasher.update(entry.created_at.to_le_bytes());
    hasher.update(entry.payload_bytes.to_le_bytes());
    if let Some(payload) = entry.payload_json.as_deref() {
        hash_text(hasher, payload);
    }
}

fn hash_text(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

fn hash_optional_text(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_text(hasher, value);
        }
        None => hasher.update([0]),
    }
}

fn observe_xopc_logical_fingerprint(connection: &Connection) -> XopcResult<[u8; 32]> {
    validate_schema(connection)?;
    let transcripts = read_transcripts(connection)?;
    let mut hasher = Sha256::new();
    hasher.update(XOPC_FINGERPRINT_DOMAIN);
    hasher.update(XOPC_SCHEMA_EVIDENCE);
    hasher.update((transcripts.len() as u64).to_be_bytes());
    for transcript in transcripts.values() {
        hash_transcript(&mut hasher, transcript);
    }
    let mut keyset = EntryKeyset::default();
    let mut entries = 0_u64;
    loop {
        let page = read_entry_page(connection, keyset)?;
        let Some(last) = page.last() else {
            break;
        };
        keyset = EntryKeyset {
            started: true,
            rowid: last.rowid,
        };
        for entry in page {
            entries = checked_add(entries, 1)?;
            hash_entry(&mut hasher, &entry);
        }
    }
    hasher.update(entries.to_be_bytes());
    Ok(hasher.finalize().into())
}

fn scan_xopc_logical_snapshot<L, S>(
    connection: &Connection,
    source: &SourceKey,
    sink: &mut ChangedDocumentSink<'_, '_, L, S>,
    sink_failure: &mut Option<SourceBackedRouteError>,
) -> XopcResult<DocumentSourceTerminal>
where
    L: ctx_history_capture_runtime::CaptureLifecycleSink,
    S: DocumentRecordSpool,
{
    validate_schema(connection)?;
    let transcripts = read_transcripts(connection)?;
    let mut sessions = BTreeMap::new();
    let mut content_digest = Sha256::new();
    content_digest.update(XOPC_CONTENT_DIGEST_DOMAIN);
    content_digest.update((transcripts.len() as u64).to_be_bytes());
    let mut certified_bytes = 0_u64;
    for transcript in transcripts.values() {
        hash_transcript(&mut content_digest, transcript);
        certified_bytes = checked_add(certified_bytes, transcript.canonical_bytes()?)?;
        sessions.insert(
            transcript.session_id.clone(),
            XopcSessionProjection {
                stable_id: xopc_session_id(source, &transcript.session_id)?,
                transcript: transcript.clone(),
            },
        );
    }

    let mut complete_records = 0_u64;
    let mut retained_records = 0_u64;
    let mut rejected_records = 0_u64;
    let mut ignored_records = 0_u64;
    let mut keyset = EntryKeyset::default();
    loop {
        let page = read_entry_page(connection, keyset)?;
        let Some(last) = page.last() else {
            break;
        };
        keyset = EntryKeyset {
            started: true,
            rowid: last.rowid,
        };
        for entry in page {
            hash_entry(&mut content_digest, &entry);
            certified_bytes = checked_add(certified_bytes, entry.canonical_bytes()?)?;
            let Some(payload_json) = entry.payload_json.as_deref() else {
                complete_records = checked_add(complete_records, 1)?;
                rejected_records = checked_add(rejected_records, 1)?;
                continue;
            };
            if entry.entry_id.is_empty()
                || entry.entry_id.len() > MAX_LINKAGE_BYTES
                || entry.session_id.is_empty()
                || entry.session_id.len() > MAX_LINKAGE_BYTES
                || entry.seq < 0
                || (entry.seq as u64) > u64::MAX / XOPC_SUBRECORD_STRIDE
            {
                complete_records = checked_add(complete_records, 1)?;
                rejected_records = checked_add(rejected_records, 1)?;
                continue;
            }
            let Some(session) = sessions.get(&entry.session_id) else {
                complete_records = checked_add(complete_records, 1)?;
                rejected_records = checked_add(rejected_records, 1)?;
                continue;
            };
            let payload: Value = match serde_json::from_str(payload_json) {
                Ok(payload) => payload,
                Err(_) => {
                    complete_records = checked_add(complete_records, 1)?;
                    rejected_records = checked_add(rejected_records, 1)?;
                    continue;
                }
            };
            let events = normalize_xopc_entry(&entry, &payload)?;
            if events.is_empty() {
                complete_records = checked_add(complete_records, 1)?;
                ignored_records = checked_add(ignored_records, 1)?;
                continue;
            }
            let event_count =
                u64::try_from(events.len()).map_err(|_| XopcSourceBackedError::CountOverflow)?;
            complete_records = checked_add(complete_records, event_count)?;
            for (subindex, event) in events.into_iter().enumerate() {
                let record = xopc_core_record(source, session, &entry, subindex, event)?;
                sink.emit_core_record(record).map_err(|error| {
                    let detail = error.to_string();
                    *sink_failure = Some(error);
                    CaptureError::InvalidPayload(detail)
                })?;
                retained_records = checked_add(retained_records, 1)?;
            }
        }
    }

    let counts = ScannedSourceCounts {
        complete_records,
        retained_records,
        rejected_records,
        ignored_records,
        indexed_documents: retained_records,
        certified_bytes,
    };
    let content_digest: [u8; 32] = content_digest.finalize().into();
    let certificate = SqliteLogicalSnapshot::new(
        XOPC_PARSER_REVISION,
        XOPC_SCHEMA_EVIDENCE,
        content_digest,
        counts,
    )
    .certify(source.clone())?;
    Ok(DocumentSourceTerminal {
        source: source.clone(),
        opening: certificate.observation().clone(),
        closing: certificate.observation().clone(),
        parser_revision: XOPC_PARSER_REVISION,
        content_digest,
        counts,
    })
}

#[derive(Clone)]
struct XopcSessionProjection {
    stable_id: StableEntityId,
    transcript: XopcTranscript,
}

struct XopcNativeEvent {
    selector: String,
    event_type: EventType,
    role: EventRole,
    body: String,
    occurred_at_unix_ms: i64,
    structured_content: Option<Value>,
    provider_call_id: Option<String>,
    invocation: Option<XopcInvocation>,
    result: Option<XopcResultCapture>,
    facts: Vec<(LiteralFactKind, String)>,
}

struct XopcInvocation {
    tool: String,
    arguments: ActivityJsonCapture,
}

struct XopcResultCapture {
    status: Option<String>,
    structured_content: ActivityJsonCapture,
}

fn normalize_xopc_entry(
    entry: &XopcEntryCell,
    payload: &Value,
) -> XopcResult<Vec<XopcNativeEvent>> {
    let occurred_at_unix_ms = xopc_timestamp(payload.get("timestamp"), entry.created_at);
    let role = payload
        .get("role")
        .and_then(Value::as_str)
        .or(entry.role.as_deref());

    if entry.entry_kind == "context"
        || payload.get("kind").and_then(Value::as_str) == Some("context")
    {
        return Ok(payload
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(|text| {
                vec![simple_event(
                    "context",
                    EventType::Notice,
                    EventRole::System,
                    text.to_owned(),
                    occurred_at_unix_ms,
                    sanitized_structured(payload),
                )]
            })
            .unwrap_or_default());
    }

    if entry.entry_kind == "compaction"
        || payload.get("type").and_then(Value::as_str) == Some("compaction")
    {
        return Ok(payload
            .get("summary")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(|summary| {
                vec![simple_event(
                    "compaction-summary",
                    EventType::Summary,
                    EventRole::Assistant,
                    summary.to_owned(),
                    occurred_at_unix_ms,
                    compaction_structured(payload),
                )]
            })
            .unwrap_or_default());
    }

    match role {
        Some("assistant") => normalize_assistant(payload, occurred_at_unix_ms),
        Some("tool" | "toolResult") => normalize_tool_result(payload, occurred_at_unix_ms),
        Some("bashExecution") => normalize_bash(entry, payload, occurred_at_unix_ms),
        Some("branchSummary" | "compactionSummary") => Ok(payload
            .get("summary")
            .and_then(Value::as_str)
            .filter(|summary| !summary.is_empty())
            .map(|summary| {
                vec![simple_event(
                    "summary",
                    EventType::Summary,
                    EventRole::Assistant,
                    summary.to_owned(),
                    occurred_at_unix_ms,
                    sanitized_structured(payload),
                )]
            })
            .unwrap_or_default()),
        Some("custom") => normalize_custom(payload, occurred_at_unix_ms),
        Some("user" | "system") => {
            let event_role = if role == Some("user") {
                EventRole::User
            } else {
                EventRole::System
            };
            Ok(normalize_message_content(
                payload,
                event_role,
                occurred_at_unix_ms,
            ))
        }
        _ if payload.get("type").and_then(Value::as_str) == Some("custom_message") => {
            normalize_custom(payload, occurred_at_unix_ms)
        }
        _ => Ok(Vec::new()),
    }
}

fn normalize_message_content(
    payload: &Value,
    role: EventRole,
    occurred_at_unix_ms: i64,
) -> Vec<XopcNativeEvent> {
    let content = payload.get("content").unwrap_or(payload);
    let body = provider_text(content);
    if body.is_empty() {
        return Vec::new();
    }
    vec![simple_event(
        "message",
        EventType::Message,
        role,
        body,
        occurred_at_unix_ms,
        sanitized_structured(content),
    )]
}

fn normalize_assistant(
    payload: &Value,
    occurred_at_unix_ms: i64,
) -> XopcResult<Vec<XopcNativeEvent>> {
    let Some(content) = payload.get("content") else {
        return Ok(Vec::new());
    };
    let blocks = content
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(content));
    let mut events = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        let block_type = block.get("type").and_then(Value::as_str);
        match block_type {
            Some("toolCall" | "tool_call" | "toolUse" | "tool_use") => {
                let call_id = first_string(block, &["id", "toolCallId", "tool_call_id"]);
                let tool = first_string(block, &["name", "toolName", "tool"]);
                let arguments = first_value(block, &["arguments", "args", "input", "parameters"]);
                let mut body = tool.unwrap_or("tool call").to_owned();
                if let Some(arguments) = arguments {
                    if let Ok(encoded) = serde_json::to_string(arguments) {
                        if !encoded.is_empty() {
                            body.push('\n');
                            body.push_str(&encoded);
                        }
                    }
                }
                let invocation = tool
                    .filter(|tool| !tool.is_empty() && tool.len() <= MAX_LINKAGE_BYTES)
                    .map(|tool| XopcInvocation {
                        tool: tool.to_owned(),
                        arguments: arguments.map_or(ActivityJsonCapture::Absent, |value| {
                            ActivityJsonCapture::Present {
                                value: value.clone(),
                            }
                        }),
                    });
                events.push(XopcNativeEvent {
                    selector: format!("tool-call-{index}"),
                    event_type: EventType::ToolCall,
                    role: EventRole::Assistant,
                    body,
                    occurred_at_unix_ms,
                    structured_content: sanitized_structured(block),
                    provider_call_id: bounded_string(call_id),
                    invocation,
                    result: None,
                    facts: Vec::new(),
                });
            }
            Some("thinking" | "reasoning") => {
                let body =
                    first_string(block, &["thinking", "text", "content"]).unwrap_or_default();
                if !body.is_empty() {
                    events.push(simple_event(
                        format!("thinking-{index}"),
                        EventType::Summary,
                        EventRole::Assistant,
                        body.to_owned(),
                        occurred_at_unix_ms,
                        None,
                    ));
                }
            }
            _ => {
                let body = provider_text(block);
                if !body.is_empty() {
                    events.push(simple_event(
                        format!("text-{index}"),
                        EventType::Message,
                        EventRole::Assistant,
                        body,
                        occurred_at_unix_ms,
                        sanitized_structured(block),
                    ));
                }
            }
        }
    }
    Ok(events)
}

fn normalize_tool_result(
    payload: &Value,
    occurred_at_unix_ms: i64,
) -> XopcResult<Vec<XopcNativeEvent>> {
    let content = payload.get("content").unwrap_or(payload);
    let body = provider_text(content);
    if body.is_empty() {
        return Ok(Vec::new());
    }
    let call_id = bounded_string(first_string(
        payload,
        &["toolCallId", "tool_call_id", "toolUseId", "tool_use_id"],
    ));
    let status = payload
        .get("isError")
        .and_then(Value::as_bool)
        .map(|is_error| if is_error { "failed" } else { "succeeded" }.to_owned())
        .or_else(|| first_string(payload, &["status", "state", "outcome"]).map(str::to_owned));
    Ok(vec![XopcNativeEvent {
        selector: "tool-result".to_owned(),
        event_type: EventType::ToolOutput,
        role: EventRole::Tool,
        body,
        occurred_at_unix_ms,
        structured_content: None,
        provider_call_id: call_id,
        invocation: None,
        result: Some(XopcResultCapture {
            status,
            structured_content: ActivityJsonCapture::Present {
                value: payload.clone(),
            },
        }),
        facts: Vec::new(),
    }])
}

fn normalize_bash(
    entry: &XopcEntryCell,
    payload: &Value,
    occurred_at_unix_ms: i64,
) -> XopcResult<Vec<XopcNativeEvent>> {
    let Some(command) = payload
        .get("command")
        .and_then(Value::as_str)
        .filter(|command| !command.is_empty())
    else {
        return Ok(Vec::new());
    };
    let call_id = format!("{}:bash", entry.entry_id);
    let facts = vec![(LiteralFactKind::Command, command.to_owned())];
    let mut events = vec![XopcNativeEvent {
        selector: "command-started".to_owned(),
        event_type: EventType::CommandStarted,
        role: EventRole::User,
        body: command.to_owned(),
        occurred_at_unix_ms,
        structured_content: None,
        provider_call_id: Some(call_id.clone()),
        invocation: Some(XopcInvocation {
            tool: "local_shell".to_owned(),
            arguments: ActivityJsonCapture::Present {
                value: serde_json::json!({"command": command}),
            },
        }),
        result: None,
        facts: facts.clone(),
    }];
    if let Some(output) = payload.get("output") {
        let body = provider_text(output);
        let status = payload
            .get("exitCode")
            .and_then(Value::as_i64)
            .map(|code| format!("exit_{code}"))
            .or_else(|| {
                payload
                    .get("signal")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        events.push(XopcNativeEvent {
            selector: "command-output".to_owned(),
            event_type: EventType::CommandOutput,
            role: EventRole::Tool,
            body: if body.is_empty() {
                "command completed without output".to_owned()
            } else {
                body
            },
            occurred_at_unix_ms,
            structured_content: None,
            provider_call_id: Some(call_id),
            invocation: None,
            result: Some(XopcResultCapture {
                status,
                structured_content: ActivityJsonCapture::Present {
                    value: payload.clone(),
                },
            }),
            facts,
        });
    }
    Ok(events)
}

fn normalize_custom(payload: &Value, occurred_at_unix_ms: i64) -> XopcResult<Vec<XopcNativeEvent>> {
    if payload.get("display").and_then(Value::as_bool) == Some(false) {
        return Ok(Vec::new());
    }
    let Some(content) = payload.get("content") else {
        return Ok(Vec::new());
    };
    let body = provider_text(content);
    if body.is_empty() {
        return Ok(Vec::new());
    }
    Ok(vec![simple_event(
        "custom-message",
        EventType::Message,
        EventRole::User,
        body,
        occurred_at_unix_ms,
        sanitized_structured(content),
    )])
}

fn simple_event(
    selector: impl Into<String>,
    event_type: EventType,
    role: EventRole,
    body: String,
    occurred_at_unix_ms: i64,
    structured_content: Option<Value>,
) -> XopcNativeEvent {
    XopcNativeEvent {
        selector: selector.into(),
        event_type,
        role,
        body,
        occurred_at_unix_ms,
        structured_content,
        provider_call_id: None,
        invocation: None,
        result: None,
        facts: Vec::new(),
    }
}

fn xopc_core_record(
    source: &SourceKey,
    session: &XopcSessionProjection,
    entry: &XopcEntryCell,
    subindex: usize,
    event: XopcNativeEvent,
) -> XopcResult<CoreRecord> {
    let subindex = u64::try_from(subindex).map_err(|_| XopcSourceBackedError::CountOverflow)?;
    if subindex >= XOPC_SUBRECORD_STRIDE {
        return Err(XopcSourceBackedError::CountOverflow);
    }
    let native_key = NativeItemKey::native_id(
        XOPC_NATIVE_EVENT_NAMESPACE,
        TypedKey::composite(vec![
            TypedKey::utf8(entry.entry_id.clone())?,
            TypedKey::utf8(event.selector.clone())?,
        ])?,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: session.stable_id,
        logical_item_kind: XOPC_LOGICAL_EVENT_KIND,
        native_item_key: &native_key,
        subrecord_selector: None,
    })?;
    let event_sequence = (entry.seq as u64)
        .checked_mul(XOPC_SUBRECORD_STRIDE)
        .and_then(|value| value.checked_add(subindex))
        .ok_or(XopcSourceBackedError::CountOverflow)?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session.stable_id,
        source.clone(),
        event_sequence,
        event.event_type.as_str(),
        XOPC_PARSER_REVISION,
        event.body,
    )?;
    record.agent_scope = Some(AgentScope::Primary);
    record.provider_session_id = Some(session.transcript.session_id.clone());
    record.native_event_id = Some(TypedKey::composite(vec![
        TypedKey::utf8(entry.entry_id.clone())?,
        TypedKey::utf8(event.selector)?,
    ])?);
    record.occurred_at_unix_ms = Some(event.occurred_at_unix_ms);
    record.role = Some(event.role.as_str().to_owned());
    record.content.structured_content = event.structured_content;

    let mut facts = Vec::new();
    if let Some(fact) = admit_provider_declared_fact(
        LiteralFactKind::SessionCwd,
        session.transcript.cwd.clone(),
        facts.len(),
    ) {
        facts.push(fact);
    }
    for (kind, value) in event.facts {
        if let Some(fact) = admit_provider_declared_fact(kind, value, facts.len()) {
            facts.push(fact);
        }
    }
    let provider_call_id = event
        .provider_call_id
        .filter(|value| !value.is_empty() && value.len() <= MAX_LINKAGE_BYTES)
        .map(TypedKey::utf8)
        .transpose()?;
    let invocation = event.invocation.map(|invocation| ActivityInvocation {
        protocol: None,
        server: None,
        tool: invocation.tool,
        arguments: invocation.arguments,
        started_at_unix_ms: Some(event.occurred_at_unix_ms),
    });
    let result = event.result.map(|result| ActivityResult {
        status: result.status,
        completed_at_unix_ms: Some(event.occurred_at_unix_ms),
        duration_ns: None,
        text: ActivityTextCapture::NormalizedBody,
        structured_content: result.structured_content,
    });
    if provider_call_id.is_some() || invocation.is_some() || result.is_some() || !facts.is_empty() {
        record.content.activity = Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id,
            invocation,
            result,
            facts,
        });
    }
    fit_xopc_content(&mut record)?;
    record.validate_contract()?;
    Ok(record)
}

fn fit_xopc_content(record: &mut CoreRecord) -> XopcResult<()> {
    if record.content.encoded_content_bytes()? > ctx_history_core::MAX_CORE_CONTENT_BYTES {
        if let Some(activity) = record.content.activity.as_mut() {
            if let Some(invocation) = activity.invocation.as_mut() {
                omit_json_capture(&mut invocation.arguments);
            }
            if let Some(result) = activity.result.as_mut() {
                omit_json_capture(&mut result.structured_content);
            }
        }
    }
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    Ok(())
}

fn omit_json_capture(capture: &mut ActivityJsonCapture) {
    let ActivityJsonCapture::Present { value } = capture else {
        return;
    };
    let observed_encoded_bytes = serde_json::to_vec(value)
        .ok()
        .and_then(|encoded| u64::try_from(encoded.len()).ok());
    *capture = ActivityJsonCapture::Omitted {
        reason: "size_limit".to_owned(),
        observed_encoded_bytes,
    };
}

fn xopc_session_id(source: &SourceKey, native_session_id: &str) -> XopcResult<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        XOPC_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: XOPC_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn xopc_source_key_scoped(source_scope: SourceAnchorScope) -> XopcResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        XOPC_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(XOPC_SOURCE_ANCHOR_KEY)?,
    )?;
    Ok(SourceKey::derive_scoped(
        CaptureProvider::Xopc.as_str(),
        XOPC_SESSIONS_SQLITE_SOURCE_FORMAT,
        XOPC_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
        source_scope,
    )?)
}

fn xopc_timestamp(value: Option<&Value>, fallback: i64) -> i64 {
    let fallback =
        DateTime::<Utc>::from_timestamp_millis(fallback).unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let provider_millis = match value {
        Some(Value::Number(number)) => number
            .as_i64()
            .filter(|value| value.unsigned_abs() >= 10_000_000_000),
        Some(Value::String(raw)) => raw
            .parse::<i64>()
            .ok()
            .filter(|value| value.unsigned_abs() >= 10_000_000_000),
        _ => None,
    };
    provider_millis
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .unwrap_or_else(|| provider_timestamp_value(value, fallback))
        .timestamp_millis()
}

fn provider_text(value: &Value) -> String {
    let mut parts = Vec::new();
    collect_provider_text(value, &mut parts);
    parts.join("\n")
}

fn collect_provider_text(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::String(text) if !text.is_empty() => parts.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                collect_provider_text(item, parts);
            }
        }
        Value::Object(object) => {
            let kind = object.get("type").and_then(Value::as_str);
            if matches!(
                kind,
                Some("toolCall" | "tool_call" | "toolUse" | "tool_use")
            ) {
                return;
            }
            for key in ["text", "thinking"] {
                if let Some(text) = object
                    .get(key)
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    parts.push(text.to_owned());
                    return;
                }
            }
            if let Some(content) = object.get("content") {
                collect_provider_text(content, parts);
            }
        }
        _ => {}
    }
}

fn first_string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
}

fn first_value<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .find_map(|key| value.get(*key).filter(|candidate| !candidate.is_null()))
}

fn bounded_string(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| !value.is_empty() && value.len() <= MAX_LINKAGE_BYTES)
        .map(str::to_owned)
}

fn sanitized_structured(value: &Value) -> Option<Value> {
    match value {
        Value::Object(object) => {
            let mut sanitized = object.clone();
            sanitized.remove("thinkingSignature");
            sanitized.remove("thinking_signature");
            sanitized.remove("signature");
            Some(Value::Object(sanitized))
        }
        Value::Array(items) => Some(Value::Array(
            items
                .iter()
                .filter_map(sanitized_structured)
                .collect::<Vec<_>>(),
        )),
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => Some(value.clone()),
    }
}

fn compaction_structured(value: &Value) -> Option<Value> {
    let Value::Object(object) = value else {
        return None;
    };
    let mut sanitized = object.clone();
    sanitized.remove("messages");
    sanitized.remove("handover");
    sanitized.remove("audit");
    sanitized.remove("thinkingSignature");
    Some(Value::Object(sanitized))
}

fn checked_add(left: u64, right: u64) -> XopcResult<u64> {
    left.checked_add(right)
        .ok_or(XopcSourceBackedError::CountOverflow)
}

fn missing_tree_fingerprint(source: &SourceKey) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(XOPC_MISSING_TREE_DOMAIN);
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
        operation: "accessing a retained XOPC SQLite source",
        source: io::Error::other(error),
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, fs, path::Path};

    use rusqlite::config::DbConfig;

    use super::*;

    fn fixture_connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "create table sessions (
                    session_key text primary key,
                    session_id text not null,
                    agent_id text not null
                );
                create table transcripts (
                    session_id text primary key,
                    session_key text not null,
                    status text not null,
                    archive_reason text,
                    created_at integer not null,
                    archived_at integer,
                    cwd text not null
                );
                create table transcript_entries (
                    entry_id text primary key,
                    session_id text not null,
                    seq integer not null,
                    entry_kind text not null,
                    role text,
                    payload_json text not null,
                    created_at integer not null
                );",
            )
            .unwrap();
        connection
    }

    fn insert_transcript(connection: &Connection, session_id: &str, session_key: &str) {
        connection
            .execute(
                "insert into transcripts
                 (session_id, session_key, status, archive_reason, created_at, archived_at, cwd)
                 values (?1, ?2, 'active', null, 1700000000000, null, '/workspace')",
                [session_id, session_key],
            )
            .unwrap();
    }

    fn insert_entry(
        connection: &Connection,
        entry_id: &str,
        session_id: &str,
        seq: i64,
        role: &str,
        payload: Value,
    ) {
        connection
            .execute(
                "insert into transcript_entries
                 (entry_id, session_id, seq, entry_kind, role, payload_json, created_at)
                 values (?1, ?2, ?3, 'message', ?4, ?5, 1700000000000)",
                rusqlite::params![entry_id, session_id, seq, role, payload.to_string()],
            )
            .unwrap();
    }

    #[test]
    fn source_snapshot_reads_active_wal_without_persistent_source_writes() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source = temp.path().join("xopc.db");
        create_file_fixture(&source);
        persist_wal_entry(&source);
        let before_read = persistent_directory_snapshot(temp.path());

        let retained =
            RetainedXopcDirectory::open(crate::test_provider_sqlite_data_root(), &source).unwrap();
        let snapshot = retained.open_snapshot().unwrap().unwrap();
        let payload: String = snapshot
            .connection()
            .unwrap()
            .query_row(
                "select payload_json from transcript_entries where entry_id = 'entry-wal'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(payload.contains("from active wal"));
        let terminal_fence = snapshot.seal().unwrap();
        retained.revalidate().unwrap();
        terminal_fence.revalidate().unwrap();

        assert_eq!(persistent_directory_snapshot(temp.path()), before_read);
    }

    fn create_file_fixture(path: &Path) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "create table sessions (
                    session_key text primary key,
                    session_id text not null,
                    agent_id text not null
                );
                create table transcripts (
                    session_id text primary key,
                    session_key text not null,
                    status text not null,
                    archive_reason text,
                    created_at integer not null,
                    archived_at integer,
                    cwd text not null
                );
                create table transcript_entries (
                    entry_id text primary key,
                    session_id text not null,
                    seq integer not null,
                    entry_kind text not null,
                    role text,
                    payload_json text not null,
                    created_at integer not null
                );
                insert into transcripts
                    (session_id, session_key, status, archive_reason, created_at, archived_at, cwd)
                values ('transcript-wal', 'agent:main:cli', 'active', null,
                        1700000000000, null, '/workspace');",
            )
            .unwrap();
    }

    fn persist_wal_entry(path: &Path) {
        let writer = Connection::open(path).unwrap();
        let mode: String = writer
            .query_row("pragma journal_mode=wal", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        writer
            .execute(
                "insert into transcript_entries
                 (entry_id, session_id, seq, entry_kind, role, payload_json, created_at)
                 values ('entry-wal', 'transcript-wal', 0, 'message', 'user',
                         '{\"role\":\"user\",\"content\":\"from active wal\"}',
                         1700000000000)",
                [],
            )
            .unwrap();
        writer
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .unwrap();
        drop(writer);
        assert!(path.with_file_name("xopc.db-wal").exists());
        assert!(path.with_file_name("xopc.db-shm").exists());
    }

    fn persistent_directory_snapshot(directory: &Path) -> Vec<(OsString, Vec<u8>)> {
        let mut paths = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                !path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .ends_with("-shm")
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                (
                    path.file_name().unwrap().to_os_string(),
                    fs::read(path).unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn mixed_assistant_rows_split_into_stable_native_events() {
        let entry = XopcEntryCell {
            rowid: 1,
            entry_id: "entry-1".to_owned(),
            session_id: "transcript-1".to_owned(),
            seq: 7,
            entry_kind: "message".to_owned(),
            role: Some("assistant".to_owned()),
            payload_json: None,
            created_at: 1_700_000_000_000,
            payload_bytes: 0,
        };
        let payload = serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "answer"},
                {"type": "thinking", "thinking": "reasoning", "thinkingSignature": "private"},
                {"type": "toolCall", "id": "call-1", "name": "read", "arguments": {"path": "src/lib.rs"}},
                {"type": "toolCall", "id": "call-2", "name": "exec", "arguments": {"command": "pwd"}}
            ]
        });
        let events = normalize_xopc_entry(&entry, &payload).unwrap();
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].selector, "text-0");
        assert_eq!(events[1].event_type, EventType::Summary);
        assert_eq!(events[2].provider_call_id.as_deref(), Some("call-1"));
        assert_eq!(events[3].selector, "tool-call-3");
        assert!(events[1].structured_content.is_none());
    }

    #[test]
    fn numeric_provider_timestamps_accept_xopc_milliseconds_and_seconds() {
        assert_eq!(
            xopc_timestamp(Some(&serde_json::json!(1_782_259_201_234_i64)), 0),
            1_782_259_201_234
        );
        assert_eq!(
            xopc_timestamp(Some(&serde_json::json!(1_782_259_201_i64)), 0),
            1_782_259_201_000
        );
    }

    #[test]
    fn tool_results_and_bash_rows_keep_activity_linkage() {
        let entry = XopcEntryCell {
            rowid: 1,
            entry_id: "entry-1".to_owned(),
            session_id: "transcript-1".to_owned(),
            seq: 1,
            entry_kind: "message".to_owned(),
            role: Some("toolResult".to_owned()),
            payload_json: None,
            created_at: 1_700_000_000_000,
            payload_bytes: 0,
        };
        let result = normalize_xopc_entry(
            &entry,
            &serde_json::json!({
                "role": "toolResult",
                "toolCallId": "call-1",
                "toolName": "read",
                "content": [{"type": "text", "text": "file body"}],
                "isError": false
            }),
        )
        .unwrap();
        assert_eq!(result[0].event_type, EventType::ToolOutput);
        assert_eq!(result[0].provider_call_id.as_deref(), Some("call-1"));
        assert_eq!(result[0].body, "file body");

        let bash = normalize_xopc_entry(
            &entry,
            &serde_json::json!({
                "role": "bashExecution",
                "command": "pwd",
                "output": "/workspace\n",
                "exitCode": 0
            }),
        )
        .unwrap();
        assert_eq!(bash.len(), 2);
        assert_eq!(bash[0].event_type, EventType::CommandStarted);
        assert_eq!(bash[1].event_type, EventType::CommandOutput);
        assert_eq!(bash[0].provider_call_id, bash[1].provider_call_id);
    }

    #[test]
    fn transcript_id_not_session_key_owns_reset_generations() {
        let connection = fixture_connection();
        insert_transcript(&connection, "generation-1", "agent:main:cli");
        insert_transcript(&connection, "generation-2", "agent:main:cli");
        insert_entry(
            &connection,
            "entry-1",
            "generation-1",
            0,
            "user",
            serde_json::json!({"role": "user", "content": [{"type": "text", "text": "before reset"}]}),
        );
        insert_entry(
            &connection,
            "entry-2",
            "generation-2",
            0,
            "user",
            serde_json::json!({"role": "user", "content": [{"type": "text", "text": "after reset"}]}),
        );
        validate_schema(&connection).unwrap();
        let transcripts = read_transcripts(&connection).unwrap();
        let source = xopc_source_key_scoped(SourceAnchorScope::Unqualified).unwrap();
        assert_ne!(
            xopc_session_id(&source, &transcripts["generation-1"].session_id).unwrap(),
            xopc_session_id(&source, &transcripts["generation-2"].session_id).unwrap()
        );
    }

    #[test]
    fn schema_probe_accepts_future_unrelated_columns_and_rejects_missing_authority() {
        let connection = fixture_connection();
        connection
            .execute(
                "alter table transcript_entries add column future_value text",
                [],
            )
            .unwrap();
        validate_schema(&connection).unwrap();

        let invalid = Connection::open_in_memory().unwrap();
        invalid
            .execute("create table sessions (session_key text)", [])
            .unwrap();
        assert!(validate_schema(&invalid).is_err());
    }

    #[test]
    fn compaction_does_not_reindex_nested_messages_or_private_audit() {
        let entry = XopcEntryCell {
            rowid: 1,
            entry_id: "entry-compaction".to_owned(),
            session_id: "transcript-1".to_owned(),
            seq: 9,
            entry_kind: "compaction".to_owned(),
            role: None,
            payload_json: None,
            created_at: 1_700_000_000_000,
            payload_bytes: 0,
        };
        let events = normalize_xopc_entry(
            &entry,
            &serde_json::json!({
                "type": "compaction",
                "summary": "bounded summary",
                "messages": [{"role": "user", "content": "duplicate secret"}],
                "handover": {"private": true},
                "audit": {"private": true}
            }),
        )
        .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].body, "bounded summary");
        let structured = events[0].structured_content.as_ref().unwrap();
        assert!(structured.get("messages").is_none());
        assert!(structured.get("handover").is_none());
        assert!(structured.get("audit").is_none());
    }
}
