//! Production NativePath ingestion for Crush's provider-owned SQLite history.
//!
//! Core publication is bounded and atomic in the ctx Store. Successful output
//! bodies are carried only by the independently replayable Pro output lane.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    ContentRef, Event, EventType, Fidelity, FileTouched, Run, RunStatus, RunType, Session,
    SessionEdge, SessionEdgeType, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, EventSearchBulkGuard,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::complete_content::{
    attach_verified_content_locator, verified_content_profile, CompleteContentSourceFamily,
    VerifiedContentLocatorV1, VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use crate::native_source::NativeSqliteValue;
use crate::provider::file_touches::{
    event_type_supports_structured_file_touches, visit_provider_file_touch_drafts_with_limit,
    MAX_PACKED_PROVIDER_EVENT_INDEX, PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
};
use crate::provider::importer::{
    compact_provider_result_payload, provider_event_import_identity_with_exact_legacy_source,
    provider_file_touch_import_id, provider_import_session_uuid, provider_path_identity,
    provider_scoped_source_identity_key, provider_scoped_source_uuid,
    provider_source_cursor_stream_for_path, provider_source_identity, provider_sync_metadata,
    timestamps, CertifiedProviderCursor, ProviderEventImportIdentity,
};
use crate::provider::normalization::provider_line_from_index;
use crate::provider::sqlite::{
    open_provider_sqlite_readonly, sqlite_schema_fingerprint, SqliteLengthPreflightGuard,
};
use crate::{
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputAssociations,
    OutputNativeCoordinate, OutputNativeCursor, OutputSourceIdentity, OutputSourceLocator,
    ProOutputMaterializationPage, ProOutputObservation, ProOutputSink, ProOutputSinkError,
    ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult, Result,
    CRUSH_SQLITE_SOURCE_FORMAT,
};

use super::capture::{
    file_values, message_child_values, message_locator, message_record_digest, read_file_values,
    session_values, CRUSH_SQLITE_VALUE_OVERHEAD_BYTES,
};
use super::projection::{
    crush_normalized_result_content, decode_file, decode_message_child, decode_read_file,
    decode_session, file_touch, project_message, project_session, read_file_touch, CrushEventDraft,
    CrushFileRow, CrushFileTouchDraft, CrushMessageProjection, CrushMessageRow, CrushReadFileRow,
    CrushRecordProjection, CrushSessionDraft, CrushSessionRow,
};
use super::source::{
    file_projection, message_projection, optional_file_columns, optional_read_file_columns,
    optional_session_column, read_file_projection, retained_length_expr, session_columns,
    session_projection, source_revision, source_snapshot,
};
use super::CRUSH_POLICY_REVISION;

const CRUSH_NATIVE_CURSOR_VERSION: u32 = 1;
const CRUSH_NATIVE_PARSER_REVISION: &str = "crush-sqlite-nativepath-v1";
const CRUSH_NATIVE_OUTPUT_CURSOR_VERSION: u32 = 1;
const CRUSH_NATIVE_OUTPUT_PARSER_REVISION: &str = "crush-sqlite-output-v1";
const CRUSH_NATIVE_PUBLICATION_DOMAIN: &[u8] = b"ctx-crush-nativepath-publication-v1\0";
const CRUSH_NATIVE_RETIREMENT_DOMAIN: &[u8] = b"ctx-crush-nativepath-retirement-v1\0";
const CRUSH_NATIVE_SOURCE_REVISION_DOMAIN: &[u8] = b"ctx-crush-nativepath-source-revision-v1\0";
const CRUSH_NATIVE_MAX_ROW_BYTES: u64 = 6 * 1024 * 1024;
const CRUSH_NATIVE_PAGE_OVERHEAD_BYTES: usize = 4 * 1024;
const CRUSH_NATIVE_OUTPUT_OVERHEAD_BYTES: usize = 4 * 1024;
const CRUSH_NATIVE_MAX_EVENT_TOUCHES: usize = 3_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CrushNativePhase {
    Sessions,
    Messages,
    Files,
    ReadFiles,
}

impl CrushNativePhase {
    fn next(self) -> Option<Self> {
        match self {
            Self::Sessions => Some(Self::Messages),
            Self::Messages => Some(Self::Files),
            Self::Files => Some(Self::ReadFiles),
            Self::ReadFiles => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Sessions => "sessions",
            Self::Messages => "messages",
            Self::Files => "files",
            Self::ReadFiles => "read_files",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrushNativeFrontier {
    phase: CrushNativePhase,
    after_rowid: Option<i64>,
    next_ordinal: u64,
}

impl Default for CrushNativeFrontier {
    fn default() -> Self {
        Self {
            phase: CrushNativePhase::Sessions,
            after_rowid: None,
            next_ordinal: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrushNativeCursor {
    version: u32,
    parser_revision: String,
    policy_revision: u32,
    locator_identity: String,
    source_revision: String,
    frontier: CrushNativeFrontier,
    generation: u64,
    terminal: bool,
    rejected_records: u64,
    retained_events: u64,
}

impl CrushNativeCursor {
    fn fresh(locator_identity: String, source_revision: String, generation: u64) -> Self {
        Self {
            version: CRUSH_NATIVE_CURSOR_VERSION,
            parser_revision: CRUSH_NATIVE_PARSER_REVISION.to_owned(),
            policy_revision: CRUSH_POLICY_REVISION,
            locator_identity,
            source_revision,
            frontier: CrushNativeFrontier::default(),
            generation,
            terminal: false,
            rejected_records: 0,
            retained_events: 0,
        }
    }

    fn validate(&self, locator_identity: &str) -> Result<()> {
        if self.version != CRUSH_NATIVE_CURSOR_VERSION
            || self.parser_revision != CRUSH_NATIVE_PARSER_REVISION
            || self.policy_revision != CRUSH_POLICY_REVISION
            || self.locator_identity != locator_identity
            || self.frontier.after_rowid.is_some_and(|rowid| rowid <= 0)
        {
            return Err(CaptureError::InvalidPayload(
                "Crush NativePath cursor is malformed or belongs to another source".to_owned(),
            ));
        }
        Ok(())
    }
}

enum CrushStoredCursor {
    Native(CrushNativeCursor),
    ReleasedLegacy,
}

// Native SQLite row shapes are intentionally decoded into one page-owned
// value; boxing the larger message variant would add allocation per row.
#[allow(clippy::large_enum_variant)]
enum CrushNativeRow {
    Session {
        row: CrushSessionRow,
        retained_bytes: usize,
    },
    Message {
        row: CrushMessageRow,
        session: Option<CrushSessionRow>,
        digest_values: Vec<NativeSqliteValue>,
        retained_bytes: usize,
    },
    File {
        row: CrushFileRow,
        retained_bytes: usize,
    },
    ReadFile {
        row: CrushReadFileRow,
        retained_bytes: usize,
    },
    Rejection {
        line: usize,
        reason: String,
        retained_bytes: usize,
    },
}

impl CrushNativeRow {
    fn retained_bytes(&self) -> usize {
        match self {
            Self::Session { retained_bytes, .. }
            | Self::Message { retained_bytes, .. }
            | Self::File { retained_bytes, .. }
            | Self::ReadFile { retained_bytes, .. }
            | Self::Rejection { retained_bytes, .. } => *retained_bytes,
        }
    }
}

struct CrushNativePage {
    expected: CrushNativeCursor,
    next: CrushNativeCursor,
    row: Option<CrushNativeRow>,
}

struct CrushNativeSchema {
    session_columns: BTreeSet<String>,
    message_columns: BTreeSet<String>,
    file_columns: Option<BTreeSet<String>>,
    read_file_columns: Option<BTreeSet<String>>,
    user_version: i64,
    schema_fingerprint: String,
}

struct CrushNativeSource {
    canonical_path: PathBuf,
    raw_source_path: String,
    source_root: String,
    locator_identity: String,
    cursor_stream: String,
    proposed_source_identity: String,
    source_revision: String,
    snapshot: crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    connection: crate::provider::sqlite::ReadOnlySqliteConnection,
    schema: CrushNativeSchema,
}

pub(super) fn import_crush_native_path(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    context.source_path = Some(path.to_path_buf());
    if !path_exists(path)? {
        if options.import_profile.is_replay_only() {
            if let Some(sink) = options.import_profile.sink() {
                sink.mark_behind(ProOutputSinkError::new(
                    "crush_nativepath_source_missing",
                    "Crush output replay source is missing",
                ));
            }
            return Ok(ProviderImportSummary::default());
        }
        return retire_missing_crush_source(path, store, &context);
    }

    let source = acquire_source(path, &context, &options)?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &source.cursor_stream)?;
    let decoded = stored
        .as_ref()
        .map(|cursor| decode_stored_cursor(&cursor.cursor, &source.locator_identity))
        .transpose()?;

    if options.import_profile.is_replay_only() {
        require_terminal_core(decoded.as_ref(), &source)?;
        replay_crush_outputs(&source, options.import_profile.sink().map(AsRef::as_ref));
        return Ok(ProviderImportSummary::default());
    }

    let mut summary = ProviderImportSummary::default();
    let mut current = core_start_cursor(decoded.as_ref(), &source)?;
    if current.source_revision == source.source_revision {
        summary.failed = usize::try_from(current.rejected_records).unwrap_or(usize::MAX);
    }

    if !current.terminal {
        let committed_store = Store::open_read_only(store.path())?;
        let bulk_guard = store.begin_event_search_bulk_mode()?;
        let operation = (|| {
            let mut changed_groups = 0_usize;
            loop {
                let page = read_core_page(&source, &context, &current)?;
                let next = page.next.clone();
                let changed = publish_core_page(
                    store,
                    &committed_store,
                    &bulk_guard,
                    &source,
                    &context,
                    &options,
                    stored_cursor_for_frontier(store, &context.machine_id, &source.cursor_stream)?,
                    page,
                    &mut summary,
                )?;
                current = next;
                if changed {
                    changed_groups = changed_groups.saturating_add(1);
                }
                if current.terminal {
                    break;
                }
                if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && changed_groups != 0
                {
                    summary.work_remaining = true;
                    break;
                }
            }
            Ok(())
        })();
        let finish = store
            .finish_event_search_bulk_mode(&bulk_guard)
            .map_err(CaptureError::from);
        match (operation, finish) {
            (Ok(()), Ok(())) => {}
            (_, Err(error)) => return Err(error),
            (Err(error), Ok(())) => return Err(error),
        }
    } else {
        summary.skipped_events = usize::try_from(current.retained_events).unwrap_or(usize::MAX);
        summary.skipped = summary.skipped.saturating_add(summary.skipped_events);
        summary.set_work_result(ProviderImportWorkResult::NoOp);
    }

    if !summary.work_remaining && current.terminal {
        replay_crush_outputs(&source, options.import_profile.sink().map(AsRef::as_ref));
    }
    Ok(summary)
}

fn path_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn acquire_source(
    path: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<CrushNativeSource> {
    let snapshot = source_snapshot(path)?;
    let canonical_path = std::fs::canonicalize(path)?;
    let connection = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let user_version = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&connection)?;
    let schema = CrushNativeSchema {
        session_columns: session_columns(&connection)?,
        message_columns: super::source::message_columns(&connection)?,
        file_columns: optional_file_columns(&connection)?,
        read_file_columns: optional_read_file_columns(&connection)?,
        user_version,
        schema_fingerprint: schema_fingerprint.clone(),
    };
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let raw_source_path = canonical_path.display().to_string();
    let source_root = context
        .source_root
        .as_deref()
        .unwrap_or(&canonical_path)
        .display()
        .to_string();
    let locator_identity = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Crush,
        CRUSH_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Crush,
        CRUSH_SQLITE_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Crush NativePath source has no canonical identity",
    ))?;
    let base_revision = source_revision(&snapshot, &schema_fingerprint);
    let source_revision = observed_source_revision(
        &base_revision,
        options.inventory_observation_token.as_deref(),
    );
    Ok(CrushNativeSource {
        canonical_path,
        raw_source_path,
        source_root,
        locator_identity,
        cursor_stream,
        proposed_source_identity,
        source_revision,
        snapshot,
        connection,
        schema,
    })
}

fn observed_source_revision(base: &str, inventory_token: Option<&str>) -> String {
    let Some(token) = inventory_token else {
        return base.to_owned();
    };
    let mut digest = Sha256::new();
    digest.update(CRUSH_NATIVE_SOURCE_REVISION_DOMAIN);
    hash_field(&mut digest, base.as_bytes());
    hash_field(&mut digest, token.as_bytes());
    format!("crush-observed-source-v1:{:x}", digest.finalize())
}

fn decode_stored_cursor(encoded: &str, locator_identity: &str) -> Result<CrushStoredCursor> {
    if let Ok(committed) = decode_native_path_committed_cursor(encoded) {
        let cursor: CrushNativeCursor =
            serde_json::from_str(committed.provider_cursor()).map_err(|error| {
                CaptureError::InvalidPayload(format!(
                    "Crush NativePath committed cursor is malformed: {error}"
                ))
            })?;
        cursor.validate(locator_identity)?;
        return Ok(CrushStoredCursor::Native(cursor));
    }
    if CertifiedProviderCursor::decode_if_certified(encoded)?.is_some() {
        return Ok(CrushStoredCursor::ReleasedLegacy);
    }
    Err(CaptureError::InvalidPayload(
        "Crush cursor is neither NativePath nor a released migration cursor".to_owned(),
    ))
}

fn core_start_cursor(
    stored: Option<&CrushStoredCursor>,
    source: &CrushNativeSource,
) -> Result<CrushNativeCursor> {
    match stored {
        Some(CrushStoredCursor::Native(cursor))
            if cursor.source_revision == source.source_revision =>
        {
            Ok(cursor.clone())
        }
        Some(CrushStoredCursor::Native(cursor)) => Ok(CrushNativeCursor::fresh(
            source.locator_identity.clone(),
            source.source_revision.clone(),
            cursor
                .generation
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Crush NativePath generation exhausted",
                ))?,
        )),
        Some(CrushStoredCursor::ReleasedLegacy) => Ok(CrushNativeCursor::fresh(
            source.locator_identity.clone(),
            source.source_revision.clone(),
            1,
        )),
        None => Ok(CrushNativeCursor::fresh(
            source.locator_identity.clone(),
            source.source_revision.clone(),
            0,
        )),
    }
}

fn require_terminal_core(
    stored: Option<&CrushStoredCursor>,
    source: &CrushNativeSource,
) -> Result<()> {
    match stored {
        Some(CrushStoredCursor::Native(cursor))
            if cursor.source_revision == source.source_revision && cursor.terminal =>
        {
            Ok(())
        }
        _ => Err(CaptureError::InvalidPayload(
            "Crush output replay requires terminal NativePath Core for the exact source revision"
                .to_owned(),
        )),
    }
}

fn stored_cursor_for_frontier(
    store: &Store,
    machine_id: &str,
    stream: &str,
) -> Result<Option<SyncCursor>> {
    store
        .get_sync_cursor(None, machine_id, stream)
        .map_err(CaptureError::from)
}

fn read_core_page(
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    current: &CrushNativeCursor,
) -> Result<CrushNativePage> {
    if current.terminal {
        return Err(CaptureError::SystemInvariant(
            "Crush NativePath attempted to read beyond its terminal frontier",
        ));
    }
    if !source.snapshot.revalidate(&source.canonical_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut frontier = current.frontier.clone();
    let row = loop {
        let candidate = next_candidate(&source.connection, &source.schema, &frontier)?;
        let Some(candidate) = candidate else {
            let Some(next_phase) = frontier.phase.next() else {
                break None;
            };
            frontier.phase = next_phase;
            frontier.after_rowid = None;
            continue;
        };
        let rowid = candidate.rowid;
        let ordinal = frontier.next_ordinal;
        frontier.after_rowid = Some(rowid);
        frontier.next_ordinal =
            frontier
                .next_ordinal
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Crush NativePath ordinal exhausted",
                ))?;
        if candidate.observed_bytes > CRUSH_NATIVE_MAX_ROW_BYTES {
            break Some(CrushNativeRow::Rejection {
                line: provider_line_from_index(ordinal.saturating_add(1)),
                reason: format!(
                    "Crush {} row {rowid} exceeds the NativePath retained-row bound",
                    frontier.phase.label()
                ),
                retained_bytes: CRUSH_NATIVE_PAGE_OVERHEAD_BYTES,
            });
        }
        break Some(
            match hydrate_row(source, frontier.phase, rowid, candidate.observed_bytes) {
                Ok(row) => row,
                Err(error) if row_decode_error_is_local(&error) => CrushNativeRow::Rejection {
                    line: provider_line_from_index(ordinal.saturating_add(1)),
                    reason: format!(
                        "Crush {} row {rowid} could not be decoded: {error}",
                        frontier.phase.label()
                    ),
                    retained_bytes: CRUSH_NATIVE_PAGE_OVERHEAD_BYTES,
                },
                Err(error) => return Err(error),
            },
        );
    };
    if !source.snapshot.revalidate(&source.canonical_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut next = current.clone();
    next.frontier = frontier;
    next.terminal = row.is_none();
    match row.as_ref() {
        Some(CrushNativeRow::Message { row, session, .. }) => {
            match project_message(row, session.as_ref(), context) {
                CrushRecordProjection::Message(message) if message.event.is_some() => {
                    next.retained_events = next.retained_events.saturating_add(1);
                }
                CrushRecordProjection::Rejection { .. } => {
                    next.rejected_records = next.rejected_records.saturating_add(1);
                }
                CrushRecordProjection::Message(_) => {}
            }
        }
        Some(CrushNativeRow::Rejection { .. }) => {
            next.rejected_records = next.rejected_records.saturating_add(1);
        }
        _ => {}
    }
    Ok(CrushNativePage {
        expected: current.clone(),
        next,
        row,
    })
}

fn row_decode_error_is_local(error: &CaptureError) -> bool {
    match error {
        CaptureError::InvalidPayload(_) | CaptureError::Json(_) => true,
        CaptureError::Sqlite(error) => matches!(
            error,
            rusqlite::Error::FromSqlConversionFailure(..)
                | rusqlite::Error::IntegralValueOutOfRange(..)
                | rusqlite::Error::Utf8Error(..)
                | rusqlite::Error::InvalidColumnType(..)
        ),
        _ => false,
    }
}

struct CrushCandidate {
    rowid: i64,
    observed_bytes: u64,
}

fn next_candidate(
    conn: &Connection,
    schema: &CrushNativeSchema,
    frontier: &CrushNativeFrontier,
) -> Result<Option<CrushCandidate>> {
    let (rowid, retained, from) = match frontier.phase {
        CrushNativePhase::Sessions => (
            "s.rowid".to_owned(),
            retained_length_expr(
                &schema.session_columns,
                "s",
                &[
                    "id",
                    "parent_session_id",
                    "title",
                    "created_at",
                    "updated_at",
                    "prompt_tokens",
                    "completion_tokens",
                    "cost",
                    "summary_message_id",
                ],
            ),
            "sessions s".to_owned(),
        ),
        CrushNativePhase::Messages => {
            let local = retained_length_expr(
                &schema.message_columns,
                "m",
                &[
                    "id",
                    "session_id",
                    "role",
                    "parts",
                    "created_at",
                    "updated_at",
                    "provider",
                    "model",
                    "is_summary_message",
                ],
            );
            let parent = retained_length_expr(
                &schema.session_columns,
                "s",
                &["parent_session_id", "created_at", "updated_at"],
            );
            (
                "m.rowid".to_owned(),
                format!("{local} + {parent}"),
                "messages m left join sessions s on s.id = m.session_id".to_owned(),
            )
        }
        CrushNativePhase::Files => {
            let Some(columns) = schema
                .file_columns
                .as_ref()
                .filter(|columns| columns.contains("session_id"))
            else {
                return Ok(None);
            };
            (
                "f.rowid".to_owned(),
                retained_length_expr(
                    columns,
                    "f",
                    &["session_id", "path", "version", "created_at", "updated_at"],
                ),
                "files f".to_owned(),
            )
        }
        CrushNativePhase::ReadFiles => {
            let Some(columns) = schema.read_file_columns.as_ref() else {
                return Ok(None);
            };
            (
                "r.rowid".to_owned(),
                retained_length_expr(columns, "r", &["session_id", "path", "read_at"]),
                "read_files r".to_owned(),
            )
        }
    };
    let after = if frontier.after_rowid.is_some() {
        format!(" where {rowid} > ?1")
    } else {
        String::new()
    };
    let sql = format!("select {rowid}, {retained} from {from}{after} order by {rowid} limit 1");
    let _guard = SqliteLengthPreflightGuard::new(conn);
    let read = |row: &rusqlite::Row<'_>| {
        let rowid = row.get::<_, i64>(0)?;
        let retained = row.get::<_, i64>(1)?;
        Ok((rowid, retained))
    };
    let candidate = match frontier.after_rowid {
        Some(rowid) => conn.query_row(&sql, [rowid], read).optional()?,
        None => conn.query_row(&sql, [], read).optional()?,
    };
    let Some((rowid, retained)) = candidate else {
        return Ok(None);
    };
    if rowid <= 0 || retained < 0 {
        return Err(CaptureError::InvalidPayload(format!(
            "Crush {} keyset metadata is invalid",
            frontier.phase.label()
        )));
    }
    let retained = u64::try_from(retained).map_err(|_| {
        CaptureError::InvalidPayload("Crush retained byte count is invalid".to_owned())
    })?;
    let observed_bytes = CRUSH_SQLITE_VALUE_OVERHEAD_BYTES
        .checked_add(retained)
        .ok_or(CaptureError::SystemInvariant(
            "Crush retained byte count overflowed",
        ))?;
    Ok(Some(CrushCandidate {
        rowid,
        observed_bytes,
    }))
}

fn hydrate_row(
    source: &CrushNativeSource,
    phase: CrushNativePhase,
    rowid: i64,
    observed_bytes: u64,
) -> Result<CrushNativeRow> {
    let retained_bytes = usize::try_from(observed_bytes)
        .unwrap_or(usize::MAX)
        .saturating_add(CRUSH_NATIVE_PAGE_OVERHEAD_BYTES);
    match phase {
        CrushNativePhase::Sessions => {
            let projection = session_projection(&source.schema.session_columns, "s");
            let values = source.connection.query_row(
                &format!("select s.rowid, {projection} from sessions s where s.rowid = ?1"),
                [rowid],
                session_values,
            )?;
            Ok(CrushNativeRow::Session {
                row: decode_session(&values)?,
                retained_bytes,
            })
        }
        CrushNativePhase::Messages => {
            let parent_created_at =
                optional_session_column(&source.schema.session_columns, "created_at");
            let parent_updated_at =
                optional_session_column(&source.schema.session_columns, "updated_at");
            let projection = message_projection(&source.schema.message_columns, "m");
            let values = source.connection.query_row(
                &format!(
                    "select s.rowid, cast({parent_created_at} as integer), \
                     cast({parent_updated_at} as integer), {projection} \
                     from messages m left join sessions s on s.id = m.session_id \
                     where m.rowid = ?1"
                ),
                [rowid],
                message_child_values,
            )?;
            let child = decode_message_child(&values)?;
            let session =
                message_parent_session(&source.connection, &source.schema.session_columns, &child)?;
            Ok(CrushNativeRow::Message {
                row: child.message,
                session,
                digest_values: values,
                retained_bytes,
            })
        }
        CrushNativePhase::Files => {
            let columns =
                source
                    .schema
                    .file_columns
                    .as_ref()
                    .ok_or(CaptureError::SystemInvariant(
                        "Crush file phase has no schema",
                    ))?;
            let projection = file_projection(columns, "f");
            let values = source.connection.query_row(
                &format!("select {projection} from files f where f.rowid = ?1"),
                [rowid],
                file_values,
            )?;
            Ok(CrushNativeRow::File {
                row: decode_file(&values)?,
                retained_bytes,
            })
        }
        CrushNativePhase::ReadFiles => {
            let columns =
                source
                    .schema
                    .read_file_columns
                    .as_ref()
                    .ok_or(CaptureError::SystemInvariant(
                        "Crush read-file phase has no schema",
                    ))?;
            let projection = read_file_projection(columns, "r");
            let values = source.connection.query_row(
                &format!("select {projection} from read_files r where r.rowid = ?1"),
                [rowid],
                read_file_values,
            )?;
            Ok(CrushNativeRow::ReadFile {
                row: decode_read_file(&values)?,
                retained_bytes,
            })
        }
    }
}

fn message_parent_session(
    conn: &Connection,
    columns: &BTreeSet<String>,
    child: &super::projection::CrushChildMessageRow,
) -> Result<Option<CrushSessionRow>> {
    let Some(parent_rowid) = child.parent_rowid else {
        return Ok(None);
    };
    let parent_session_id = if columns.contains("parent_session_id") {
        conn.query_row(
            "select cast(parent_session_id as text) from sessions where rowid = ?1",
            [parent_rowid],
            |row| row.get::<_, Option<String>>(0),
        )?
    } else {
        None
    };
    Ok(Some(CrushSessionRow {
        id: child.message.session_id.clone(),
        parent_session_id,
        title: None,
        created_at: child.parent_created_at,
        updated_at: child.parent_updated_at,
        prompt_tokens: None,
        completion_tokens: None,
        cost: None,
        summary_message_id: None,
    }))
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    stored: Option<SyncCursor>,
    page: CrushNativePage,
    summary: &mut ProviderImportSummary,
) -> Result<bool> {
    if !source.snapshot.revalidate(&source.canonical_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let next_cursor = encode_core_cursor(&page.next)?;
    let next = provider_sync_cursor(
        &context.machine_id,
        source.cursor_stream.clone(),
        next_cursor,
        context.imported_at,
    );
    let transition =
        NativePathCursorTransition::new(stored.as_ref().map(|cursor| cursor.cursor.clone()), next);
    let publication_id = core_publication_id(source, &page, &transition);
    let retained_bytes = page.row.as_ref().map_or(
        CRUSH_NATIVE_PAGE_OVERHEAD_BYTES,
        CrushNativeRow::retained_bytes,
    );
    let accounting = NativePathGroupAccounting::new(1, 1, retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(false);
    }

    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Crush,
            source_format: CRUSH_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: source.locator_identity.clone(),
            cursor_stream: source.cursor_stream.clone(),
            proposed_source_identity: source.proposed_source_identity.clone(),
            raw_source_path: Some(source.raw_source_path.clone()),
            source_revision: source.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;

    if let Some(row) = page.row {
        publish_native_row(
            committed_store,
            &mut group,
            source,
            context,
            options,
            &resolution,
            row,
            summary,
        )?;
    }
    if !source.snapshot.revalidate(&source.canonical_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn publish_native_row(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    resolution: &ctx_history_store::ProviderSourceLocatorResolution,
    row: CrushNativeRow,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    match row {
        CrushNativeRow::Session { row, .. } => {
            let draft = project_session(
                &row,
                &source.raw_source_path,
                source.schema.user_version,
                &source.schema.schema_fingerprint,
                context.imported_at,
            );
            publish_session_draft(
                committed_store,
                group,
                source,
                context,
                options,
                &resolution.canonical_source_identity,
                &resolution.route_binding(),
                &draft,
                summary,
            )?;
            summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
        }
        CrushNativeRow::Message {
            row,
            session,
            digest_values,
            ..
        } => match project_message(&row, session.as_ref(), context) {
            CrushRecordProjection::Message(message) => publish_message(
                committed_store,
                group,
                source,
                context,
                options,
                &resolution.canonical_source_identity,
                &resolution.route_binding(),
                row.rowid,
                digest_values,
                *message,
                summary,
            )?,
            CrushRecordProjection::Rejection {
                line_number,
                reason,
            } => summary.record_failure(ProviderImportFailure {
                line: line_number,
                error: reason,
            }),
        },
        CrushNativeRow::File { row, .. } => {
            let line = provider_line_from_index(
                0x0100_0000_0000_u64.saturating_add(row.rowid.max(0) as u64),
            );
            if let Some(touch) = file_touch(row, context.imported_at) {
                publish_file_touch(
                    committed_store,
                    group,
                    source,
                    context,
                    options,
                    &resolution.canonical_source_identity,
                    &resolution.route_binding(),
                    touch,
                    summary,
                )?;
            } else {
                summary.record_failure(ProviderImportFailure {
                    line,
                    error: "Crush file row has no provider session id".to_owned(),
                });
            }
        }
        CrushNativeRow::ReadFile { row, .. } => {
            let touch = read_file_touch(row, context.imported_at);
            publish_file_touch(
                committed_store,
                group,
                source,
                context,
                options,
                &resolution.canonical_source_identity,
                &resolution.route_binding(),
                touch,
                summary,
            )?;
        }
        CrushNativeRow::Rejection { line, reason, .. } => {
            summary.record_failure(ProviderImportFailure {
                line,
                error: reason,
            });
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn publish_session_draft(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    canonical_source_identity: &str,
    route_binding: &ctx_history_store::ProviderSourceRouteBinding,
    draft: &CrushSessionDraft,
    summary: &mut ProviderImportSummary,
) -> Result<(Uuid, Session)> {
    let provider_session_id = &draft.provider_session_id;
    let source_id = source_id_for_session(
        committed_store,
        source,
        context,
        canonical_source_identity,
        provider_session_id,
    )?;
    group.upsert_capture_source(&canonical_capture_source(
        source,
        context,
        draft,
        source_id,
        canonical_source_identity,
    ))?;
    group.bind_capture_source_provider_route(source_id, route_binding)?;
    let session = canonical_session(
        committed_store,
        source,
        context,
        options,
        draft,
        source_id,
        canonical_source_identity,
    )?;
    let existed = committed_store.get_session(session.id).is_ok();
    if let Some(parent_id) = session.parent_session_id {
        if committed_store.get_session(parent_id).is_err() {
            group.upsert_session(&relationship_placeholder(
                source,
                context,
                options,
                parent_id,
                draft
                    .parent_provider_session_id
                    .as_deref()
                    .unwrap_or("unknown-parent"),
                source_id,
                canonical_source_identity,
            ))?;
        }
    }
    group.upsert_session(&session)?;
    if existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    if let Some(parent_id) = session.parent_session_id {
        let edge = relationship_edge(
            source,
            context,
            &session,
            parent_id,
            source_id,
            canonical_source_identity,
        );
        let existed = committed_store.session_edge_exists(edge.id)?;
        group.upsert_projection_neutral_session_edge(&canonical_actor(&session), &edge)?;
        if existed {
            summary.skipped_edges = summary.skipped_edges.saturating_add(1);
        } else {
            summary.imported_edges = summary.imported_edges.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }
    Ok((source_id, session))
}

fn source_id_for_session(
    committed_store: &Store,
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    canonical_source_identity: &str,
    provider_session_id: &str,
) -> Result<Uuid> {
    Ok(committed_store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::Crush,
            CRUSH_SQLITE_SOURCE_FORMAT,
            &context.machine_id,
            canonical_source_identity,
            provider_session_id,
        )?
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Crush,
                provider_session_id,
                CRUSH_SQLITE_SOURCE_FORMAT,
                Some(&source.raw_source_path),
            )
        }))
}

fn canonical_capture_source(
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    draft: &CrushSessionDraft,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Crush,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(source.raw_source_path.clone()),
            source_format: Some(CRUSH_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(source.source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(draft.provider_session_id.clone()),
        },
        started_at: draft.started_at,
        ended_at: draft.ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": draft.provider_session_id,
                "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": source.source_root,
                "source_revision": source.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Crush,
                    &draft.provider_session_id,
                    CRUSH_SQLITE_SOURCE_FORMAT,
                    Some(&source.raw_source_path),
                ),
                "metadata": draft.source_metadata,
                "nativepath_publication": CRUSH_NATIVE_PARSER_REVISION,
            }),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn canonical_session(
    committed_store: &Store,
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    draft: &CrushSessionDraft,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> Result<Session> {
    let provider_session_id = &draft.provider_session_id;
    let id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Crush,
        provider_session_id,
        source_id,
        Some(canonical_source_identity),
    )?;
    let parent_session_id = draft
        .parent_provider_session_id
        .as_deref()
        .map(|parent| {
            let parent_source_id = source_id_for_session(
                committed_store,
                source,
                context,
                canonical_source_identity,
                parent,
            )?;
            provider_import_session_uuid(
                committed_store,
                CaptureProvider::Crush,
                parent,
                parent_source_id,
                Some(canonical_source_identity),
            )
        })
        .transpose()?;
    Ok(Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id,
        root_session_id: parent_session_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Crush,
        external_session_id: Some(provider_session_id.clone()),
        external_agent_id: None,
        agent_type: if parent_session_id.is_some() {
            AgentType::Subagent
        } else {
            AgentType::Primary
        },
        role_hint: Some(
            if parent_session_id.is_some() {
                "subagent"
            } else {
                "primary"
            }
            .to_owned(),
        ),
        is_primary: parent_session_id.is_none(),
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: draft.started_at,
        ended_at: draft.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "parent_provider_session_id": draft.parent_provider_session_id,
                "root_provider_session_id": draft.parent_provider_session_id,
                "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "session_idempotency_key": format!(
                    "provider-session:{}:{}",
                    CaptureProvider::Crush.as_str(),
                    provider_session_id,
                ),
                "metadata": draft.session_metadata,
                "nativepath_publication": CRUSH_NATIVE_PARSER_REVISION,
            }),
        ),
    })
}

#[allow(clippy::too_many_arguments)]
fn relationship_placeholder(
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    id: Uuid,
    external_session_id: &str,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> Session {
    Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Crush,
        external_session_id: Some(external_session_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Unknown,
        role_hint: Some("relationship_placeholder".to_owned()),
        is_primary: false,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: context.imported_at,
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": external_session_id,
                "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
                "source_identity": canonical_source_identity,
                "source_revision": source.source_revision,
                "relationship_placeholder": true,
            }),
        ),
    }
}

fn relationship_edge(
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    session: &Session,
    parent_id: Uuid,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "provider-source-root:{canonical_source_identity}:session:{}:parent_child",
                session.external_session_id.as_deref().unwrap_or_default()
            ),
            "session-edge",
        ),
        from_session_id: session.id,
        to_session_id: parent_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: ctx_history_core::Confidence::Explicit,
        source_id: Some(source_id),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.external_session_id,
                "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
                "source_revision": source.source_revision,
                "imported_at": context.imported_at,
            }),
        ),
    }
}

fn canonical_actor(session: &Session) -> CanonicalActor {
    CanonicalActor {
        direct_session_id: session.id,
        root_session_id: session.root_session_id.unwrap_or(session.id),
        parent_session_id: session.parent_session_id,
        external_session_id: session.external_session_id.clone(),
        external_agent_id: session.external_agent_id.clone(),
        agent_type: session.agent_type.as_str().to_owned(),
        role_hint: session.role_hint.clone(),
        is_primary: session.is_primary,
    }
}

fn attach_crush_complete_content_locator(
    event: &mut CrushEventDraft,
    rowid: i64,
    digest_values: &[NativeSqliteValue],
    complete_text: &str,
) -> Result<()> {
    if event.event_type != EventType::Message
        || event
            .payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Ok(());
    }
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("SQLite content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::Crush,
        CRUSH_SQLITE_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "supported SQLite message route must have a verified-content profile",
    ))?;
    let locator = message_locator(rowid)?;
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        event.provider_event_hash.clone(),
        message_record_digest(digest_values)?,
    )
    .ok_or(CaptureError::SystemInvariant(
        "SQLite complete-content locator exceeds the bounded canonical schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn crush_core_event(
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    event: &CrushEventDraft,
    event_hash: &str,
    identity: &ProviderEventImportIdentity,
    run_id: Option<Uuid>,
) -> Event {
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut event_metadata = event.metadata.clone();
    let verified_content_locators = event_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": event.provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": ProviderEventHashAuthority::ProviderSupplied.as_str(),
        "cursor": event.cursor,
        "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::Crush.as_str(),
            provider_session_id,
            event.provider_event_index,
        ),
        "source_record_ordinal": Value::Null,
        "source_record_subrecord_index": Value::Null,
        "metadata": event_metadata,
    });
    if let (Some(metadata), Some(locators)) =
        (sync_metadata.as_object_mut(), verified_content_locators)
    {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session_id),
        run_id,
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::Crush.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": event.provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": event.cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(event.event_type, &event.payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    }
}

#[allow(clippy::too_many_arguments)]
fn crush_command_run(
    provider_session_id: &str,
    history_record_id: Option<Uuid>,
    source_id: Uuid,
    session_id: Uuid,
    event: &CrushEventDraft,
    event_hash: &str,
    run_source_id: Option<Uuid>,
) -> Option<Run> {
    if event.event_type != EventType::CommandOutput {
        return None;
    }
    let run_id = run_source_id.map_or_else(
        || {
            stable_capture_uuid(
                &format!(
                    "provider:{}:{provider_session_id}:run:{event_hash}",
                    CaptureProvider::Crush.as_str(),
                ),
                "run",
            )
        },
        |run_source_id| {
            stable_capture_uuid(
                &format!("provider-source:{run_source_id}:run:{event_hash}"),
                "run",
            )
        },
    );
    Some(Run {
        id: run_id,
        history_record_id,
        session_id: Some(session_id),
        run_type: RunType::Command,
        status: match event.payload.get("result_outcome").and_then(Value::as_str) {
            Some("failure") => RunStatus::Failed,
            Some("success") => RunStatus::Succeeded,
            _ => RunStatus::Partial,
        },
        started_at: event.occurred_at,
        ended_at: Some(event.occurred_at),
        exit_code: None,
        cwd: None,
        command_preview: None,
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(event.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": event.provider_event_index,
                "provider_event_hash": event_hash,
                "call_id": Value::Null,
                "source": "provider_command_output",
            }),
        ),
    })
}

#[allow(clippy::too_many_arguments)]
fn publish_message(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    canonical_source_identity: &str,
    route_binding: &ctx_history_store::ProviderSourceRouteBinding,
    rowid: i64,
    digest_values: Vec<NativeSqliteValue>,
    mut projection: CrushMessageProjection,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let existing_source = committed_store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::Crush,
            CRUSH_SQLITE_SOURCE_FORMAT,
            &context.machine_id,
            canonical_source_identity,
            &projection.provider_session_id,
        )?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(format!(
                "Crush message {} references session {} not already persisted for its exact source",
                projection.native_record_id, projection.provider_session_id
            ))
        });
    let existing_source = match existing_source {
        Ok(source) => source,
        Err(error) => {
            summary.record_failure(ProviderImportFailure {
                line: projection.line_number,
                error: error.to_string(),
            });
            return Ok(());
        }
    };
    group.bind_capture_source_provider_route(existing_source.id, route_binding)?;
    let session = committed_store
        .session_by_capture_source_and_external_session(
            existing_source.id,
            CaptureProvider::Crush,
            &projection.provider_session_id,
        )?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(format!(
                "Crush message {} has no canonical parent session",
                projection.native_record_id
            ))
        });
    let session = match session {
        Ok(session) => session,
        Err(error) => {
            summary.record_failure(ProviderImportFailure {
                line: projection.line_number,
                error: error.to_string(),
            });
            return Ok(());
        }
    };

    if projection.output.is_none() {
        let event = projection
            .event
            .as_mut()
            .ok_or(CaptureError::SystemInvariant(
                "Crush non-output projection has no event",
            ))?;
        let complete_text =
            projection
                .complete_text
                .clone()
                .ok_or(CaptureError::SystemInvariant(
                    "Crush non-output projection has no complete text",
                ))?;
        attach_crush_complete_content_locator(event, rowid, &digest_values, &complete_text)?;
    }

    if let Some(event) = projection.event.take() {
        let event_hash = event.provider_event_hash.clone();
        let identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            CaptureProvider::Crush,
            &projection.provider_session_id,
            existing_source.id,
            projection.provider_event_index,
            projection.provider_event_index,
            &event_hash,
            None,
            None,
            session.id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::Crush,
                    &projection.provider_session_id,
                ),
        )?;
        let run = crush_command_run(
            &projection.provider_session_id,
            options.history_record_id,
            existing_source.id,
            session.id,
            &event,
            &event_hash,
            identity.run_source_id,
        );
        if let Some(run) = &run {
            group.upsert_run(run)?;
        }
        let normalized = crush_core_event(
            context,
            options,
            &projection.provider_session_id,
            existing_source.id,
            session.id,
            projection.line_number,
            &event,
            &event_hash,
            &identity,
            run.as_ref().map(|run| run.id),
        );
        if group
            .reconcile_provider_event(&normalized, ProviderEventHashAuthority::ProviderSupplied)?
        {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    }

    let mut touches = Vec::new();
    let outcome = visit_provider_file_touch_drafts_with_limit(
        &projection.raw_parts,
        event_type_supports_structured_file_touches(projection.event_type),
        CRUSH_NATIVE_MAX_EVENT_TOUCHES,
        |(touch_ordinal, touch)| {
            let provider_touch_index =
                if projection.provider_event_index > MAX_PACKED_PROVIDER_EVENT_INDEX {
                    touch_ordinal
                } else {
                    (projection.provider_event_index << 16) | touch_ordinal
                };
            touches.push(CrushFileTouchDraft {
                provider_session_id: projection.provider_session_id.clone(),
                provider_touch_index,
                provider_event_index: Some(projection.provider_event_index),
                path: touch.path,
                change_kind: touch.change_kind,
                old_path: touch.old_path,
                line_count_delta: None,
                confidence: touch.confidence,
                occurred_at: projection.occurred_at,
                metadata: touch.metadata,
            });
            Ok::<(), CaptureError>(())
        },
    )?;
    for touch in touches {
        publish_file_touch(
            committed_store,
            group,
            source,
            context,
            options,
            canonical_source_identity,
            route_binding,
            touch,
            summary,
        )?;
    }
    if outcome.limit_exceeded() {
        summary.record_failure(ProviderImportFailure {
            line: projection.line_number,
            error: PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn publish_file_touch(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    _source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    canonical_source_identity: &str,
    route_binding: &ctx_history_store::ProviderSourceRouteBinding,
    touch: CrushFileTouchDraft,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let Some(existing_source) = committed_store.capture_source_by_canonical_identity_session(
        CaptureProvider::Crush,
        CRUSH_SQLITE_SOURCE_FORMAT,
        &context.machine_id,
        canonical_source_identity,
        &touch.provider_session_id,
    )?
    else {
        summary.record_failure(ProviderImportFailure {
            line: provider_line_from_index(touch.provider_touch_index),
            error: format!(
                "Crush file touch references session {} not already persisted for its exact source",
                touch.provider_session_id
            ),
        });
        return Ok(());
    };
    group.bind_capture_source_provider_route(existing_source.id, route_binding)?;
    let session = committed_store
        .session_by_capture_source_and_external_session(
            existing_source.id,
            CaptureProvider::Crush,
            &touch.provider_session_id,
        )?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Crush file touch has no canonical parent session".to_owned(),
            )
        })?;
    let event_id = touch
        .provider_event_index
        .map(|index| {
            crate::provider::importer::provider_file_touch_event_id(
                committed_store,
                CaptureProvider::Crush,
                &touch.provider_session_id,
                existing_source.id,
                index,
                session.id
                    == crate::provider::importer::provider_session_uuid(
                        CaptureProvider::Crush,
                        &touch.provider_session_id,
                    ),
            )
        })
        .transpose()?
        .flatten();
    let id = provider_file_touch_import_id(
        committed_store,
        CaptureProvider::Crush,
        &touch.provider_session_id,
        existing_source.id,
        touch.provider_event_index,
        touch.provider_touch_index,
        session.id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::Crush,
                &touch.provider_session_id,
            ),
    )?;
    group.upsert_file_touched(&FileTouched {
        id,
        history_record_id: options.history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: touch.path,
        change_kind: touch.change_kind,
        old_path: touch.old_path,
        line_count_delta: touch.line_count_delta,
        confidence: touch.confidence,
        timestamps: timestamps(touch.occurred_at),
        source_id: Some(existing_source.id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": CaptureProvider::Crush.as_str(),
                "provider_session_id": touch.provider_session_id,
                "provider_touch_index": touch.provider_touch_index,
                "provider_event_index": touch.provider_event_index,
                "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
                "metadata": touch.metadata,
            }),
        ),
    })?;
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok(())
}

fn encode_core_cursor(cursor: &CrushNativeCursor) -> Result<String> {
    serde_json::to_string(cursor).map_err(CaptureError::from)
}

fn provider_sync_cursor(
    machine_id: &str,
    stream: String,
    cursor: String,
    observed_at: DateTime<Utc>,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Crush.as_str(),
                machine_id,
                stream
            ),
            "cursor",
        ),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream,
        cursor,
        last_synced_at: Some(observed_at),
        timestamps: timestamps(observed_at),
    }
}

fn core_publication_id(
    source: &CrushNativeSource,
    page: &CrushNativePage,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(CRUSH_NATIVE_PUBLICATION_DOMAIN);
    hash_field(&mut digest, source.locator_identity.as_bytes());
    hash_field(&mut digest, source.source_revision.as_bytes());
    hash_field(
        &mut digest,
        &serde_json::to_vec(&page.expected.frontier).unwrap_or_default(),
    );
    hash_field(
        &mut digest,
        &serde_json::to_vec(&page.next.frontier).unwrap_or_default(),
    );
    hash_field(&mut digest, transition.next().cursor.as_bytes());
    format!("crush-nativepath-v1:{:x}", digest.finalize())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrushOutputCursor {
    version: u32,
    source_revision: String,
    after_rowid: Option<i64>,
    next_ordinal: u64,
    terminal: bool,
}

impl CrushOutputCursor {
    fn initial(source_revision: String) -> Self {
        Self {
            version: CRUSH_NATIVE_OUTPUT_CURSOR_VERSION,
            source_revision,
            after_rowid: None,
            next_ordinal: 0,
            terminal: false,
        }
    }

    fn native_cursor(&self) -> Result<OutputNativeCursor> {
        Ok(OutputNativeCursor {
            version: CRUSH_NATIVE_OUTPUT_CURSOR_VERSION,
            payload: serde_json::to_vec(self)?,
        })
    }
}

fn replay_crush_outputs(source: &CrushNativeSource, sink: Option<&dyn ProOutputSink>) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_crush_outputs_inner(source, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "crush_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_crush_outputs_inner(source: &CrushNativeSource, sink: &dyn ProOutputSink) -> Result<()> {
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::Crush.as_str().to_owned(),
        namespace_id: source.source_root.clone(),
        source_id: source.locator_identity.clone(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let materializer_revision = sink.materializer_revision().to_owned();
    let progress_cursor = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == CRUSH_NATIVE_OUTPUT_CURSOR_VERSION)
        .and_then(|cursor| serde_json::from_slice::<CrushOutputCursor>(&cursor.payload).ok())
        .filter(|cursor| {
            cursor.version == CRUSH_NATIVE_OUTPUT_CURSOR_VERSION
                && cursor.after_rowid.is_none_or(|rowid| rowid > 0)
        });
    let can_resume = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == CRUSH_NATIVE_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == materializer_revision
            && progress.observed_revision == source.source_revision
            && progress_cursor.is_some()
    });
    if can_resume
        && progress_cursor
            .as_ref()
            .is_some_and(|cursor| cursor.terminal)
    {
        return Ok(());
    }
    let (mut cursor, source_epoch, mut expected_epoch, mut expected_cursor, mut disposition) =
        match progress {
            None => (
                CrushOutputCursor::initial(source.source_revision.clone()),
                0,
                None,
                None,
                ProOutputSourceDisposition::NewSource,
            ),
            Some(progress) if can_resume => (
                progress_cursor.ok_or(CaptureError::SystemInvariant(
                    "Crush resumable output progress lost its cursor",
                ))?,
                progress.source_epoch,
                Some(progress.source_epoch),
                progress.cursor,
                ProOutputSourceDisposition::AppendOrResume,
            ),
            Some(progress) => (
                CrushOutputCursor::initial(source.source_revision.clone()),
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Crush output source epoch exhausted",
                    ))?,
                Some(progress.source_epoch),
                progress.cursor,
                ProOutputSourceDisposition::Rewrite,
            ),
        };

    loop {
        if !source.snapshot.revalidate(&source.canonical_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let candidate =
            next_message_candidate(&source.connection, &source.schema, cursor.after_rowid)?;
        let mut next = cursor.clone();
        let observations = match candidate {
            Some(candidate) => {
                next.after_rowid = Some(candidate.rowid);
                let ordinal = next.next_ordinal;
                next.next_ordinal =
                    next.next_ordinal
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "Crush output ordinal exhausted",
                        ))?;
                if candidate.observed_bytes > CRUSH_NATIVE_MAX_ROW_BYTES {
                    return Err(CaptureError::InvalidPayload(format!(
                        "Crush output row {} exceeds the NativePath retained-row bound",
                        candidate.rowid
                    )));
                } else {
                    let row = hydrate_row(
                        source,
                        CrushNativePhase::Messages,
                        candidate.rowid,
                        candidate.observed_bytes,
                    )?;
                    let observation = match row {
                        CrushNativeRow::Message { row, session, .. } => {
                            output_observation(source, ordinal, row, session.as_ref())?
                        }
                        _ => None,
                    };
                    observation.into_iter().collect()
                }
            }
            None => {
                next.terminal = true;
                Vec::new()
            }
        };
        let next_native = next.native_cursor()?;
        let page = ProOutputMaterializationPage {
            inventory_generation: sink.inventory_generation(),
            source: output_source.clone(),
            source_epoch,
            observed_revision: source.source_revision.clone(),
            parser_revision: CRUSH_NATIVE_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: materializer_revision.clone(),
            disposition,
            expected_prior_source_epoch: expected_epoch,
            expected_prior_cursor: expected_cursor.clone(),
            next_safe_cursor: next_native.clone(),
            terminal: next.terminal,
            observations,
        };
        let result = match sink.materialize_page(page) {
            Ok(result)
                if result.source_epoch == source_epoch
                    && result.committed_cursor == next_native =>
            {
                result
            }
            Ok(_) => {
                sink.mark_behind(ProOutputSinkError::new(
                    "crush_nativepath_output_receipt_mismatch",
                    "Crush output sink acknowledged another source epoch or cursor",
                ));
                return Ok(());
            }
            Err(error) => {
                sink.mark_behind(error);
                return Ok(());
            }
        };
        expected_epoch = Some(result.source_epoch);
        expected_cursor = Some(result.committed_cursor);
        disposition = ProOutputSourceDisposition::AppendOrResume;
        cursor = next;
        if cursor.terminal {
            return Ok(());
        }
    }
}

fn next_message_candidate(
    conn: &Connection,
    schema: &CrushNativeSchema,
    after_rowid: Option<i64>,
) -> Result<Option<CrushCandidate>> {
    next_candidate(
        conn,
        schema,
        &CrushNativeFrontier {
            phase: CrushNativePhase::Messages,
            after_rowid,
            next_ordinal: 0,
        },
    )
}

fn output_observation(
    source: &CrushNativeSource,
    ordinal: u64,
    row: CrushMessageRow,
    session: Option<&CrushSessionRow>,
) -> Result<Option<ProOutputObservation>> {
    let CrushRecordProjection::Message(projected) = project_message(
        &row,
        session,
        &ProviderAdapterContext {
            machine_id: String::new(),
            source_path: Some(source.canonical_path.clone()),
            source_root: Some(PathBuf::from(&source.source_root)),
            imported_at: DateTime::<Utc>::UNIX_EPOCH,
        },
    ) else {
        return Ok(None);
    };
    let Some(output) = projected.output else {
        return Ok(None);
    };
    let content = crush_normalized_result_content(&projected.raw_parts)
        .unwrap_or_default()
        .into_bytes();
    if content
        .len()
        .saturating_add(CRUSH_NATIVE_OUTPUT_OVERHEAD_BYTES)
        > ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES
    {
        return Err(CaptureError::InvalidPayload(format!(
            "Crush output row {} exceeds the NativePath output-page bound",
            row.rowid
        )));
    }
    Ok(Some(ProOutputObservation {
        kind: output.kind,
        coordinate: OutputNativeCoordinate {
            unit_key: format!(
                "crush:{}:{}:output",
                projected.provider_session_id, projected.native_record_id
            ),
            native_sequence: ordinal,
            native_record_id: Some(projected.native_record_id.clone()),
            source_record_ordinal: Some(ordinal),
            source_record_subrecord_index: Some(0),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: Some(projected.occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: projected.provider_session_id.clone(),
            root_session_id: projected
                .parent_session_id
                .clone()
                .unwrap_or_else(|| projected.provider_session_id.clone()),
            parent_session_id: projected.parent_session_id,
            provider_session_id: Some(projected.provider_session_id),
            agent_id: None,
            repository: None,
        },
        call_id: output.call_id,
        command: output.command,
        outcome: output.outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: super::capture::CRUSH_LOCATOR_KIND.to_owned(),
            payload: message_locator(row.rowid)?.value().to_vec(),
        },
        content,
    }))
}

#[derive(Clone)]
struct KnownCrushRoute {
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    current_cursor: SyncCursor,
}

fn retire_missing_crush_source(
    requested_path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
) -> Result<ProviderImportSummary> {
    let routes = known_crush_routes(store, requested_path, context)?;
    if routes.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: requested_path.to_path_buf(),
            reason: "Crush SQLite source does not exist",
        });
    }
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        for route in routes.values() {
            if retire_crush_route(store, &bulk_guard, context, route)? {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
                summary.set_work_result(ProviderImportWorkResult::Changed);
            }
        }
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

fn known_crush_routes(
    store: &Store,
    requested_path: &Path,
    context: &ProviderAdapterContext,
) -> Result<BTreeMap<String, KnownCrushRoute>> {
    let requested_absolute = lexical_absolute_path(requested_path)?;
    let requested_is_source_root = context
        .source_root
        .as_deref()
        .map(lexical_absolute_path)
        .transpose()?
        .is_some_and(|root| root == requested_absolute);
    let mut routes = BTreeMap::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Crush
            || source.descriptor.machine_id != context.machine_id
            || source.descriptor.source_format.as_deref() != Some(CRUSH_SQLITE_SOURCE_FORMAT)
        {
            continue;
        }
        let Some(raw_path) = source.descriptor.raw_source_path.as_deref() else {
            continue;
        };
        let raw_path = PathBuf::from(raw_path);
        let raw_matches =
            raw_path == requested_path || lexical_absolute_path(&raw_path)? == requested_absolute;
        let root_matches = requested_is_source_root
            && source
                .descriptor
                .source_root
                .as_deref()
                .map(Path::new)
                .map(lexical_absolute_path)
                .transpose()?
                .is_some_and(|root| root == requested_absolute);
        if !raw_matches && !root_matches {
            continue;
        }
        let locator_identity = provider_path_identity(&raw_path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Crush,
            CRUSH_SQLITE_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, &context.machine_id, &stream)?
        else {
            continue;
        };
        let Some(canonical_source_identity) = source.descriptor.source_identity.clone() else {
            continue;
        };
        let Some(source_revision) = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        routes
            .entry(locator_identity.clone())
            .or_insert(KnownCrushRoute {
                locator_identity,
                canonical_source_identity,
                source_revision,
                current_cursor,
            });
    }
    Ok(routes)
}

fn lexical_absolute_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

fn retire_crush_route(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    route: &KnownCrushRoute,
) -> Result<bool> {
    let provider_cursor = match decode_native_path_committed_cursor(&route.current_cursor.cursor) {
        Ok(committed) => committed.provider_cursor().to_owned(),
        Err(_) => encode_core_cursor(&CrushNativeCursor {
            version: CRUSH_NATIVE_CURSOR_VERSION,
            parser_revision: CRUSH_NATIVE_PARSER_REVISION.to_owned(),
            policy_revision: CRUSH_POLICY_REVISION,
            locator_identity: route.locator_identity.clone(),
            source_revision: route.source_revision.clone(),
            frontier: CrushNativeFrontier::default(),
            generation: 1,
            terminal: true,
            rejected_records: 0,
            retained_events: 0,
        })?,
    };
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            route.current_cursor.stream.clone(),
            provider_cursor,
            context.imported_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Crush,
        source_format: CRUSH_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.current_cursor.stream.clone(),
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason: ProviderSourceRouteRetirementReason::RootMissing,
    };
    let mut digest = Sha256::new();
    digest.update(CRUSH_NATIVE_RETIREMENT_DOMAIN);
    hash_field(&mut digest, route.locator_identity.as_bytes());
    hash_field(&mut digest, route.canonical_source_identity.as_bytes());
    hash_field(&mut digest, route.source_revision.as_bytes());
    hash_field(&mut digest, route.current_cursor.cursor.as_bytes());
    let publication_id = format!("crush-nativepath-retire-v1:{:x}", digest.finalize());
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    let changed =
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllExpected => {
                let disposition = group.retire_provider_source_route(&retirement)?;
                if disposition == ProviderSourceRouteRetirementDisposition::Retired {
                    group.prepare_journal_checkpoint()?;
                    group.publish_cursor_set()?;
                    true
                } else {
                    group.rollback()?;
                    return Ok(false);
                }
            }
            NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
        };
    group.commit()?;
    Ok(changed)
}

fn hash_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}
