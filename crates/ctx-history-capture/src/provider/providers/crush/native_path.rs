//! Production NativePath ingestion for Crush's provider-owned SQLite history.
//!
//! Core publication is bounded and atomic in the ctx Store. Successful output
//! bodies are carried only by the independently replayable Pro output lane.

mod event_projection;
mod lifecycle;
mod output;
mod publication;
mod query;
pub(crate) mod source_backed;

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
use rusqlite::{types::ValueRef, Connection, OptionalExtension};
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

use super::capture::{message_locator, message_record_digest, CRUSH_SQLITE_VALUE_OVERHEAD_BYTES};
use super::projection::{
    crush_normalized_result_content, decode_file, decode_message_child, decode_read_file,
    decode_session, file_touch, optional_text, project_message, project_session, read_file_touch,
    CrushEventDraft, CrushFileRow, CrushFileTouchDraft, CrushMessageProjection, CrushMessageRow,
    CrushReadFileRow, CrushRecordProjection, CrushSessionDraft, CrushSessionRow,
};
use super::source::{
    file_projection, message_projection, message_session_join, optional_file_columns,
    optional_read_file_columns, optional_session_column, read_file_projection,
    retained_length_expr, session_columns, session_projection, source_revision, source_snapshot,
};
use super::CRUSH_POLICY_REVISION;

use lifecycle::{hash_field, retire_missing_crush_source};
use output::replay_crush_outputs;
use publication::publish_core_page;
use query::read_core_page;

const CRUSH_NATIVE_CURSOR_VERSION: u32 = 1;
const CRUSH_NATIVE_PARSER_REVISION: &str = "crush-sqlite-nativepath-v2";
const CRUSH_PREVIOUS_NATIVE_PARSER_REVISION: &str = "crush-sqlite-nativepath-v1";
const CRUSH_PREVIOUS_NATIVE_POLICY_REVISION: u32 = 5;
const CRUSH_NATIVE_OUTPUT_CURSOR_VERSION: u32 = 1;
const CRUSH_NATIVE_OUTPUT_PARSER_REVISION: &str = "crush-sqlite-output-v2";
const CRUSH_NATIVE_PUBLICATION_DOMAIN: &[u8] = b"ctx-crush-nativepath-publication-v1\0";
const CRUSH_NATIVE_RETIREMENT_DOMAIN: &[u8] = b"ctx-crush-nativepath-retirement-v1\0";
const CRUSH_NATIVE_SOURCE_REVISION_DOMAIN: &[u8] = b"ctx-crush-nativepath-source-revision-v1\0";
const CRUSH_NATIVE_MAX_ROW_BYTES: u64 = 6 * 1024 * 1024;
const CRUSH_NATIVE_PAGE_OVERHEAD_BYTES: usize = 4 * 1024;
const CRUSH_NATIVE_OUTPUT_OVERHEAD_BYTES: usize = 4 * 1024;
const CRUSH_NATIVE_MAX_EVENT_TOUCHES: usize = 3_000;
const CRUSH_NATIVE_MAX_REJECTION_CHARS: usize = 512;

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
    #[serde(default)]
    rejections: Vec<ProviderImportFailure>,
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
            rejections: Vec::new(),
            retained_events: 0,
        }
    }

    fn validate(&self, locator_identity: &str) -> Result<()> {
        let revision_is_supported = match self.parser_revision.as_str() {
            CRUSH_NATIVE_PARSER_REVISION => self.policy_revision == CRUSH_POLICY_REVISION,
            CRUSH_PREVIOUS_NATIVE_PARSER_REVISION => matches!(
                self.policy_revision,
                CRUSH_PREVIOUS_NATIVE_POLICY_REVISION | CRUSH_POLICY_REVISION
            ),
            _ => false,
        };
        if self.version != CRUSH_NATIVE_CURSOR_VERSION
            || !revision_is_supported
            || self.locator_identity != locator_identity
            || self.frontier.after_rowid.is_some_and(|rowid| rowid <= 0)
            || self.rejections.len() > crate::summaries::MAX_RETAINED_PROVIDER_FAILURES
            || self.rejections.len() as u64 > self.rejected_records
            || self
                .rejections
                .iter()
                .any(|failure| failure.error.chars().count() > CRUSH_NATIVE_MAX_REJECTION_CHARS)
        {
            return Err(CaptureError::InvalidPayload(
                "Crush NativePath cursor is malformed or belongs to another source".to_owned(),
            ));
        }
        Ok(())
    }

    fn record_rejection(&mut self, failure: ProviderImportFailure) -> Result<()> {
        self.rejected_records =
            self.rejected_records
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Crush NativePath rejection count exhausted",
                ))?;
        if self.rejections.len() < crate::summaries::MAX_RETAINED_PROVIDER_FAILURES {
            self.rejections.push(ProviderImportFailure {
                line: failure.line,
                error: failure
                    .error
                    .chars()
                    .take(CRUSH_NATIVE_MAX_REJECTION_CHARS)
                    .collect(),
            });
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
        projection: Box<CrushMessageProjection>,
        touches: Vec<CrushFileTouchDraft>,
        rejections: Vec<ProviderImportFailure>,
        retained_bytes: usize,
    },
    File {
        touch: CrushFileTouchDraft,
        retained_bytes: usize,
    },
    ReadFile {
        touch: CrushFileTouchDraft,
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

    fn rejections(&self) -> Vec<ProviderImportFailure> {
        match self {
            Self::Message { rejections, .. } => rejections.clone(),
            Self::Rejection { line, reason, .. } => {
                vec![ProviderImportFailure {
                    line: *line,
                    error: reason.clone(),
                }]
            }
            _ => Vec::new(),
        }
    }
}

// Hydrated rows are transient query results consumed immediately by projection;
// preserving their inline fields avoids an allocation on every SQLite row.
#[allow(clippy::large_enum_variant)]
enum CrushHydratedRow {
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
    restore_cursor_rejections(&mut summary, &current);

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

fn restore_cursor_rejections(summary: &mut ProviderImportSummary, cursor: &CrushNativeCursor) {
    summary.failed = usize::try_from(cursor.rejected_records).unwrap_or(usize::MAX);
    summary.failures = cursor.rejections.clone();
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
    let schema = read_native_schema(&connection)?;
    let schema_fingerprint = schema.schema_fingerprint.clone();
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

fn read_native_schema(connection: &Connection) -> Result<CrushNativeSchema> {
    Ok(CrushNativeSchema {
        session_columns: session_columns(connection)?,
        message_columns: super::source::message_columns(connection)?,
        file_columns: optional_file_columns(connection)?,
        read_file_columns: optional_read_file_columns(connection)?,
        user_version: connection.pragma_query_value(None, "user_version", |row| row.get(0))?,
        schema_fingerprint: sqlite_schema_fingerprint(connection)?,
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
            if cursor.parser_revision == CRUSH_NATIVE_PARSER_REVISION
                && cursor.source_revision == source.source_revision =>
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
            if cursor.parser_revision == CRUSH_NATIVE_PARSER_REVISION
                && cursor.source_revision == source.source_revision
                && cursor.terminal =>
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
