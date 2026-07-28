use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    EventIdentityInput, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ProjectionContractError, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceKey, SourceObservation, SourceRecordLocator, SourceResolverContractError,
    StableEntityId, TypedKey,
};
use ctx_history_index::{LexicalDocument, MAX_BODY_PREVIEW_CHARS};
use rusqlite::{limits::Limit, Connection, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    nativepath::{
        scan_warp_source_backed_connection, WarpNativeEvent, WarpNativeMessageIdentity,
        WarpNativePage, WarpNativeProOutputPage, WarpNativeProOutputPageReceipt, WarpNativeSession,
        WarpNativeSink, WarpNativeSourceBackedScan,
    },
    schema::WarpSqliteSchema,
    warp_message_content_at,
};
use crate::{
    complete_content::sqlite::sqlite_logical_record_digest, native_source::NativeSqliteValue,
    provider::sqlite::ProviderSqliteSourceSnapshot, CaptureError, Result as CaptureResult,
    MAX_PROVIDER_SQLITE_VALUE_BYTES, WARP_SQLITE_SOURCE_FORMAT,
};

const WARP_SOURCE_ANCHOR_NAMESPACE: &str = "warp.selected-surface";
const WARP_NATIVE_SESSION_NAMESPACE: &str = "warp.conversation";
const WARP_NATIVE_ITEM_NAMESPACE: &str = "warp.task-message";
const WARP_LOGICAL_SESSION_KIND: &str = "warp-conversation";
const WARP_LOGICAL_ITEM_KIND: &str = "warp-task-message";
const WARP_SOURCE_SCHEMA_VARIANT: &str = "warp-agent-task-protobuf-v1";
const WARP_SOURCE_REVISION_KIND: &str = "warp-sqlite-snapshot-observation-v0";
const WARP_SOURCE_BACKED_PARSER_REVISION: &str = "warp-source-backed-v0";
const WARP_TASK_MESSAGE_RELATION: &str = "agent_tasks.task-message.v1";
const WARP_SOURCE_INVALID_REASON: &str = "Warp SQLite source must be a regular non-symlink file";
const WARP_SIDECAR_INVALID_REASON: &str = "Warp SQLite sidecar must be a regular non-symlink file";
const WARP_SOURCE_REVISION_DIGEST_DOMAIN: &[u8] = b"ctx.warp.source-backed.revision.v0\0";

#[derive(Debug, Error)]
pub(crate) enum WarpSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Warp selected surface key is empty")]
    EmptySurfaceKey,
    #[error("Warp selected surface key {0:?} appears more than once")]
    DuplicateSurfaceKey(String),
    #[error("Warp SQLite source changed while its read snapshot was projected")]
    SourceChanged,
    #[error("Warp source-backed scan count overflow")]
    CountOverflow,
    #[error("Warp source-backed parser counts do not match its emitted records")]
    ScanCountMismatch,
    #[error("Warp source-backed digest is not canonical lowercase SHA-256")]
    InvalidDigest,
    #[error("Warp source-backed parser emitted an empty lexical record")]
    EmptyLexicalRecord,
    #[error("locator is not a Warp task-message row")]
    InvalidLocator,
    #[error("Warp locator source revision no longer matches the selected database")]
    StaleSourceRevision,
    #[error("Warp locator task row no longer exists")]
    MissingTaskRow,
    #[error("Warp locator task row digest no longer matches")]
    StaleTaskRow,
    #[error("Warp locator message no longer exists in its task row")]
    MissingTaskMessage,
}

pub(crate) type WarpSourceBackedResultV0<T> = Result<T, WarpSourceBackedErrorV0>;

/// One source selected by the installed-surface discovery contract.
///
/// `surface_key` is stable catalog lineage such as `linux:stable:gui` or
/// `windows:preview:tui`; it is not a physical path or mutable file identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WarpSourceSelectionV0 {
    path: PathBuf,
    surface_key: String,
}

impl WarpSourceSelectionV0 {
    pub(crate) fn new(
        path: impl Into<PathBuf>,
        surface_key: impl Into<String>,
    ) -> WarpSourceBackedResultV0<Self> {
        let surface_key = surface_key.into();
        if surface_key.is_empty() {
            return Err(WarpSourceBackedErrorV0::EmptySurfaceKey);
        }
        Ok(Self {
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

#[derive(Debug)]
pub(crate) struct WarpSourceBackedSnapshotV0 {
    pub(crate) selection: WarpSourceSelectionV0,
    pub(crate) source: SourceKey,
    pub(crate) certified_source: CertifiedSource,
    pub(crate) documents: Vec<LexicalDocument>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WarpHydratedRecordV0 {
    pub(crate) provider_bytes: Vec<u8>,
    pub(crate) event_type: String,
    pub(crate) native_record_id: String,
}

pub(crate) fn project_selected_warp_sources_v0(
    selections: &[WarpSourceSelectionV0],
) -> WarpSourceBackedResultV0<Vec<WarpSourceBackedSnapshotV0>> {
    let mut selected = BTreeSet::new();
    let mut snapshots = Vec::with_capacity(selections.len());
    for selection in selections {
        if !selected.insert(selection.surface_key.clone()) {
            return Err(WarpSourceBackedErrorV0::DuplicateSurfaceKey(
                selection.surface_key.clone(),
            ));
        }
        snapshots.push(project_warp_source_backed_v0(selection.clone())?);
    }
    Ok(snapshots)
}

pub(crate) fn project_warp_source_backed_v0(
    selection: WarpSourceSelectionV0,
) -> WarpSourceBackedResultV0<WarpSourceBackedSnapshotV0> {
    let source = warp_source_key(&selection)?;
    let canonical_path = fs::canonicalize(selection.path())?;
    let opening_snapshot = read_source_snapshot(&canonical_path)?;
    let opening_revision = opening_snapshot.revision_component();
    let revision_digest = source_revision_digest(&source, &opening_revision);
    let connection = open_direct_read_only(&canonical_path)?;
    let mut sink = WarpProjectionSink::new(
        source.clone(),
        revision_digest,
        canonical_path.to_string_lossy().into_owned(),
    );
    let native_scan = scan_warp_source_backed_connection(&connection, &mut sink)?;
    drop(connection);

    let closing_snapshot = read_source_snapshot(&canonical_path)?;
    if opening_snapshot != closing_snapshot || fs::canonicalize(selection.path())? != canonical_path
    {
        return Err(WarpSourceBackedErrorV0::SourceChanged);
    }
    let closing_revision = closing_snapshot.revision_component();
    let counts = scan_counts(&native_scan, &sink)?;
    let content_digest = parse_hex_digest(&native_scan.source_integrity_digest)?;
    let certified_source = CertifiedSource::certify(
        SourceObservation::new(
            source.clone(),
            WARP_SOURCE_REVISION_KIND,
            opening_revision.into_bytes(),
        )?,
        SourceObservation::new(
            source.clone(),
            WARP_SOURCE_REVISION_KIND,
            closing_revision.into_bytes(),
        )?,
        WARP_SOURCE_BACKED_PARSER_REVISION,
        content_digest,
        counts,
    )?;
    Ok(WarpSourceBackedSnapshotV0 {
        selection,
        source,
        certified_source,
        documents: sink.documents,
    })
}

pub(crate) fn resolve_warp_locator_v0(
    selection: &WarpSourceSelectionV0,
    locator: &SourceRecordLocator,
) -> WarpSourceBackedResultV0<WarpHydratedRecordV0> {
    locator.validate_contract()?;
    let source = warp_source_key(selection)?;
    if !source.exact_descriptor_eq(locator.source())
        || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
    {
        return Err(WarpSourceBackedErrorV0::InvalidLocator);
    }
    let (rowid, message_ordinal) = decode_task_message_coordinate(locator)?;
    let expected_revision = locator
        .certified_source_revision_digest()
        .ok_or(WarpSourceBackedErrorV0::InvalidLocator)?;
    let canonical_path = fs::canonicalize(selection.path())?;
    let opening_snapshot = read_source_snapshot(&canonical_path)?;
    if &source_revision_digest(&source, &opening_snapshot.revision_component()) != expected_revision
    {
        return Err(WarpSourceBackedErrorV0::StaleSourceRevision);
    }

    let connection = open_direct_read_only(&canonical_path)?;
    WarpSqliteSchema::detect(&connection)?;
    let resolved = with_read_transaction(&connection, || {
        let values =
            load_task_values(&connection, rowid)?.ok_or(WarpSourceBackedErrorV0::MissingTaskRow)?;
        let actual_digest = digest_bytes(sqlite_logical_record_digest(&values).as_str())?;
        if actual_digest != *locator.record_digest() {
            return Err(WarpSourceBackedErrorV0::StaleTaskRow);
        }
        let [_, NativeSqliteValue::Text(conversation_id), NativeSqliteValue::Text(task_id), NativeSqliteValue::Blob(task), _] =
            values.as_slice()
        else {
            return Err(WarpSourceBackedErrorV0::StaleTaskRow);
        };
        let content = warp_message_content_at(
            task,
            conversation_id,
            task_id,
            usize::try_from(message_ordinal)
                .map_err(|_| WarpSourceBackedErrorV0::InvalidLocator)?,
        )?
        .ok_or(WarpSourceBackedErrorV0::MissingTaskMessage)?;
        Ok(WarpHydratedRecordV0 {
            provider_bytes: content.text.into_bytes(),
            event_type: content.event_type.as_str().to_owned(),
            native_record_id: content.native_record_id,
        })
    })?;
    drop(connection);

    let closing_snapshot = read_source_snapshot(&canonical_path)?;
    if opening_snapshot != closing_snapshot || fs::canonicalize(selection.path())? != canonical_path
    {
        return Err(WarpSourceBackedErrorV0::SourceChanged);
    }
    Ok(resolved)
}

struct WarpProjectionSink {
    source: SourceKey,
    source_revision_digest: [u8; 32],
    source_path: String,
    session_lineage: BTreeMap<String, WarpSessionLineage>,
    documents: Vec<LexicalDocument>,
    rejected_records: u64,
    ignored_records: u64,
}

struct WarpSessionLineage {
    parent_conversation_id: Option<String>,
    root_conversation_id: String,
}

impl WarpProjectionSink {
    fn new(source: SourceKey, source_revision_digest: [u8; 32], source_path: String) -> Self {
        Self {
            source,
            source_revision_digest,
            source_path,
            session_lineage: BTreeMap::new(),
            documents: Vec::new(),
            rejected_records: 0,
            ignored_records: 0,
        }
    }
}

impl WarpNativeSink for WarpProjectionSink {
    fn push_page(&mut self, page: WarpNativePage) -> CaptureResult<()> {
        let WarpNativePage {
            sessions,
            hierarchy_edges,
            events,
            rejections,
            ..
        } = page;
        self.rejected_records = self
            .rejected_records
            .checked_add(u64::try_from(rejections.len()).map_err(|_| {
                CaptureError::SystemInvariant("Warp source-backed rejection count exceeds u64")
            })?)
            .ok_or(CaptureError::SystemInvariant(
                "Warp source-backed rejection count overflowed",
            ))?;
        let ignored_records = sessions
            .len()
            .checked_add(hierarchy_edges.len())
            .and_then(|count| u64::try_from(count).ok())
            .ok_or(CaptureError::SystemInvariant(
                "Warp source-backed ignored count exceeds u64",
            ))?;
        self.ignored_records = self.ignored_records.checked_add(ignored_records).ok_or(
            CaptureError::SystemInvariant("Warp source-backed ignored count overflowed"),
        )?;
        for session in sessions {
            let conversation_id = session.conversation_id.clone();
            if self
                .session_lineage
                .insert(conversation_id, WarpSessionLineage::from(session))
                .is_some()
            {
                return Err(CaptureError::SystemInvariant(
                    "Warp source-backed parser repeated a session",
                ));
            }
        }
        for event in events {
            let lineage = self
                .session_lineage
                .get(&event.identity.conversation_id)
                .ok_or(CaptureError::SystemInvariant(
                    "Warp source-backed event has no session lineage",
                ))?;
            let document = lexical_document(
                &self.source,
                self.source_revision_digest,
                &self.source_path,
                lineage,
                event,
            )
            .map_err(source_backed_capture_error)?;
            self.documents.push(document);
        }
        Ok(())
    }

    fn push_pro_output_page(
        &mut self,
        page: WarpNativeProOutputPage,
    ) -> WarpNativeProOutputPageReceipt {
        page.receipt()
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

fn lexical_document(
    source: &SourceKey,
    source_revision_digest: [u8; 32],
    source_path: &str,
    lineage: &WarpSessionLineage,
    event: WarpNativeEvent,
) -> WarpSourceBackedResultV0<LexicalDocument> {
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
    let record_digest = digest_bytes(event.source_record_digest.as_str())?;
    let coordinate_key = TypedKey::composite(vec![
        TypedKey::I64(event.native_order.task_rowid),
        TypedKey::U64(u64::from(event.native_order.message_ordinal)),
    ])?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: WARP_TASK_MESSAGE_RELATION.to_owned(),
            primary_key: coordinate_key,
            row_version: Some(TypedKey::bytes(record_digest.to_vec())?),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(source_revision_digest),
        record_digest,
    )?;
    let body = bounded_lexical_body(&event.body, event.kind);
    if body.is_empty() {
        return Err(WarpSourceBackedErrorV0::EmptyLexicalRecord);
    }
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id,
        root_session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(event.identity.conversation_id),
        // Warp's certified task-message model does not expose a VCS branch.
        branch: None,
        source_path: Some(source_path.to_owned()),
        agent_type: if is_primary {
            AgentType::Primary
        } else {
            AgentType::Subagent
        }
        .as_str()
        .to_owned(),
        is_primary,
        event_sequence: event.native_order.provider_event_index,
        occurred_at_unix_ms: event.occurred_at.map(|value| value.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body,
        workspace: None,
        cwd: None,
        touched_files: Vec::new(),
    })
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
    sink: &WarpProjectionSink,
) -> WarpSourceBackedResultV0<ScannedSourceCounts> {
    let retained_records =
        u64::try_from(sink.documents.len()).map_err(|_| WarpSourceBackedErrorV0::CountOverflow)?;
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

fn decode_task_message_coordinate(
    locator: &SourceRecordLocator,
) -> WarpSourceBackedResultV0<(i64, u64)> {
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(WarpSourceBackedErrorV0::InvalidLocator);
    };
    let TypedKey::Composite(parts) = primary_key else {
        return Err(WarpSourceBackedErrorV0::InvalidLocator);
    };
    let [TypedKey::I64(rowid), TypedKey::U64(message_ordinal)] = parts.as_slice() else {
        return Err(WarpSourceBackedErrorV0::InvalidLocator);
    };
    if logical_relation != WARP_TASK_MESSAGE_RELATION
        || *rowid <= 0
        || row_version.as_ref() != Some(&TypedKey::Bytes(locator.record_digest().to_vec()))
    {
        return Err(WarpSourceBackedErrorV0::InvalidLocator);
    }
    Ok((*rowid, *message_ordinal))
}

fn load_task_values(
    connection: &Connection,
    rowid: i64,
) -> WarpSourceBackedResultV0<Option<Vec<NativeSqliteValue>>> {
    connection
        .query_row(
            "select rowid, cast(conversation_id as text), cast(task_id as text), task, \
                    cast(last_modified_at as text) \
             from agent_tasks where rowid = ?1",
            [rowid],
            |row| {
                Ok(vec![
                    NativeSqliteValue::Integer(row.get(0)?),
                    NativeSqliteValue::Text(row.get(1)?),
                    NativeSqliteValue::Text(row.get(2)?),
                    NativeSqliteValue::Blob(row.get(3)?),
                    NativeSqliteValue::Text(row.get(4)?),
                ])
            },
        )
        .optional()
        .map_err(WarpSourceBackedErrorV0::from)
}

fn open_direct_read_only(path: &Path) -> WarpSourceBackedResultV0<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| WarpSourceBackedErrorV0::CountOverflow)?;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "query_only", true)?;
    Ok(connection)
}

fn with_read_transaction<T>(
    connection: &Connection,
    read: impl FnOnce() -> WarpSourceBackedResultV0<T>,
) -> WarpSourceBackedResultV0<T> {
    connection.execute_batch("begin")?;
    let result = read();
    let rollback = connection.execute_batch("rollback");
    match (result, rollback) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (_, Err(error)) => Err(error.into()),
    }
}

fn read_source_snapshot(path: &Path) -> WarpSourceBackedResultV0<ProviderSqliteSourceSnapshot> {
    Ok(ProviderSqliteSourceSnapshot::read(
        path,
        WARP_SOURCE_INVALID_REASON,
        WARP_SIDECAR_INVALID_REASON,
    )?)
}

fn source_revision_digest(source: &SourceKey, revision: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(WARP_SOURCE_REVISION_DIGEST_DOMAIN);
    digest.update(source.exact_descriptor_digest());
    digest.update((revision.len() as u64).to_be_bytes());
    digest.update(revision.as_bytes());
    digest.finalize().into()
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

fn bounded_lexical_body(body: &str, fallback: &str) -> String {
    let bounded = body
        .chars()
        .take(MAX_BODY_PREVIEW_CHARS)
        .collect::<String>();
    if bounded.is_empty() {
        fallback.chars().take(MAX_BODY_PREVIEW_CHARS).collect()
    } else {
        bounded
    }
}

fn source_backed_capture_error(error: WarpSourceBackedErrorV0) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}
