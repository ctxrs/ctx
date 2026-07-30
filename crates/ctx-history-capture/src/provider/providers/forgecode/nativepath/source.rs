mod scanner;

use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use chrono::Duration;
use ctx_history_core::EventType;
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::{
    common::io::{ProviderSourceDirectory, ProviderSourceRoot},
    provider::{
        file_touches::{
            event_type_supports_structured_file_touches,
            visit_provider_file_touch_drafts_with_limit, MAX_PACKED_PROVIDER_EVENT_INDEX,
            PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
        },
        normalization::{provider_capped_json_value, provider_line_from_index},
        sqlite::{
            ensure_sqlite_table_columns, optional_column_expr, sqlite_schema_fingerprint,
            sqlite_table_columns, SqliteLengthPreflightGuard,
        },
    },
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceAccessError, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
    },
    CaptureError, OutputAssociations, OutputNativeCoordinate, OutputObservationKind, OutputOutcome,
    OutputOutcomeMetadata, OutputSourceLocator, ProOutputObservation, ProviderAdapterContext,
    ProviderImportFailure, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::super::complete_content::ForgeCodeCompleteContentDigest;
use super::super::event::{
    forgecode_event, forgecode_event_type, forgecode_for_each_metric_file_touch_with_limit,
    forgecode_message_parts, forgecode_message_text, forgecode_normalized_result_content,
    forgecode_timestamp, forgecode_tool_result_call_id, forgecode_tool_result_is_error,
    ForgeCodeFileTouch, ForgeCodeNativeEvent,
};

pub(super) const FORGECODE_NATIVE_PARSER_REVISION: u32 = 1;
pub(super) const FORGECODE_NATIVE_POLICY_REVISION: u32 = 6;
pub(super) const FORGECODE_NATIVE_LOCATOR_KIND: &str = "forgecode-conversation-row-v1";
pub(super) const FORGECODE_NATIVE_PAGE_MAX_BYTES: usize = 6 * 1024 * 1024;
const FORGECODE_NATIVE_PAGE_REJECTION_RESERVE_BYTES: usize = 4 * 1024;
const FORGECODE_NATIVE_PAGE_CONTENT_MAX_BYTES: usize =
    FORGECODE_NATIVE_PAGE_MAX_BYTES - FORGECODE_NATIVE_PAGE_REJECTION_RESERVE_BYTES;
const FORGECODE_NATIVE_MAX_MESSAGES_PER_PAGE: usize = 16;
const FORGECODE_NATIVE_MAX_TOUCHES_PER_MESSAGE: usize = 64;
const FORGECODE_NATIVE_MAX_METRIC_TOUCHES: usize = 64;
const FORGECODE_NATIVE_MAX_EVENT_BYTES: usize = 2 * 1024 * 1024;
const FORGECODE_NATIVE_MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const FORGECODE_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 64 * 8;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::provider::providers::forgecode) struct ForgeCodeFrontier {
    pub(super) rowid: Option<i64>,
    pub(super) next_message: u32,
    pub(super) row_complete: bool,
}

impl ForgeCodeFrontier {
    pub(in crate::provider::providers::forgecode) const fn initial() -> Self {
        Self {
            rowid: None,
            next_message: 0,
            row_complete: true,
        }
    }
}

#[derive(Clone)]
pub(in crate::provider::providers::forgecode) struct ForgeCodeSourceObservation {
    pub(super) canonical_path: PathBuf,
    pub(super) database: Arc<ForgeCodeSqliteDatabase>,
    pub(super) schema_fingerprint: String,
    pub(super) user_version: i64,
    columns: BTreeSet<String>,
}

// Discovery returns the fully certified live source by value; boxing would
// add allocation to the normal path solely to match the missing-path variant.
#[allow(clippy::large_enum_variant)]
pub(in crate::provider::providers::forgecode) enum ForgeCodeDiscovery {
    Live(ForgeCodeSourceObservation),
    Missing,
}

pub(in crate::provider::providers::forgecode) fn discover_forgecode_source(
    path: &Path,
) -> Result<ForgeCodeDiscovery> {
    let candidate = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            path.join(".forge.db")
        }
        Ok(_) => path.to_path_buf(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let exact = absolute_path(path)?;
            let child = exact.join(".forge.db");
            let exact_is_preferred = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("db"));
            let preferred_path = if exact_is_preferred { exact } else { child };
            let _ = preferred_path;
            return Ok(ForgeCodeDiscovery::Missing);
        }
        Err(error) => return Err(error.into()),
    };
    match fs::symlink_metadata(&candidate) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let candidate = absolute_path(&candidate)?;
            let _ = candidate;
            return Ok(ForgeCodeDiscovery::Missing);
        }
        Err(error) => return Err(error.into()),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.file_type().is_file() => {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: candidate,
                reason: "ForgeCode SQLite source must be a regular non-symlink file",
            });
        }
        Ok(_) => {}
    }
    let canonical_path = absolute_path(&candidate)?;
    let (database, (columns, schema_fingerprint, user_version)) =
        ForgeCodeSqliteDatabase::open(&canonical_path, |conn| {
            let columns = sqlite_table_columns(conn, "conversations")?;
            ensure_sqlite_table_columns(
                &columns,
                "ForgeCode conversations table",
                &["conversation_id", "workspace_id", "created_at"],
            )?;
            Ok((
                columns,
                sqlite_schema_fingerprint(conn)?,
                conn.pragma_query_value(None, "user_version", |row| row.get(0))?,
            ))
        })?;
    Ok(ForgeCodeDiscovery::Live(ForgeCodeSourceObservation {
        canonical_path,
        database: Arc::new(database),
        schema_fingerprint,
        user_version,
        columns,
    }))
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

#[derive(Debug)]
pub(in crate::provider::providers::forgecode) struct ForgeCodeSqliteDatabase {
    parent: ProviderSourceDirectory,
    authority: SqliteSourceDirectoryAuthority,
    database_name: OsString,
    evidence: SqliteSourceEvidence,
}

impl ForgeCodeSqliteDatabase {
    pub(super) fn open<T>(
        path: &Path,
        query: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<(Self, T)> {
        let parent_path =
            path.parent()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: "ForgeCode SQLite source must have a parent directory",
                })?;
        let database_name = path
            .file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "ForgeCode SQLite source must have a database leaf name",
            })?
            .to_os_string();
        let parent = ProviderSourceRoot::open(parent_path)?.directory()?;
        let authority_handle = parent.try_clone_authority_handle()?;
        let authority = retain_sqlite_source_directory_authority(&authority_handle, parent_path)
            .map_err(|error| forgecode_sqlite_source_error(path, error))?;
        let snapshot = open_root_handle_sqlite_source_snapshot(&authority, &database_name)
            .map_err(|error| forgecode_sqlite_source_error(path, error))?;
        let evidence = snapshot.evidence().clone();
        let result = snapshot
            .connection()
            .map_err(|error| forgecode_sqlite_source_error(path, error))
            .and_then(query);
        let finished = snapshot
            .finish()
            .map_err(|error| forgecode_sqlite_source_error(path, error))?;
        if finished != evidence {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let database = Self {
            parent,
            authority,
            database_name,
            evidence,
        };
        database.revalidate()?;
        Ok((database, result?))
    }

    pub(super) fn read<T>(
        &self,
        path: &Path,
        query: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<T> {
        self.revalidate()?;
        let snapshot =
            open_root_handle_sqlite_source_snapshot(&self.authority, &self.database_name)
                .map_err(|error| forgecode_sqlite_source_error(path, error))?;
        let result = if snapshot.evidence() == &self.evidence {
            snapshot
                .connection()
                .map_err(|error| forgecode_sqlite_source_error(path, error))
                .and_then(query)
        } else {
            Err(CaptureError::SourceChangedDuringCapture)
        };
        let finished = snapshot
            .finish()
            .map_err(|error| forgecode_sqlite_source_error(path, error))?;
        self.revalidate()?;
        if finished != self.evidence {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        result
    }

    pub(super) fn revalidate(&self) -> Result<()> {
        self.parent.revalidate()?;
        self.parent.authority_root().revalidate()
    }

    pub(super) fn evidence(&self) -> &SqliteSourceEvidence {
        &self.evidence
    }
}

fn forgecode_sqlite_source_error(path: &Path, error: SqliteSourceAccessError) -> CaptureError {
    match error {
        SqliteSourceAccessError::SourceChanged
        | SqliteSourceAccessError::ConnectionIdentityMismatch => {
            CaptureError::SourceChangedDuringCapture
        }
        error => CaptureError::ProviderSource {
            provider: ctx_history_core::CaptureProvider::ForgeCode.as_str(),
            path: path.to_path_buf(),
            kind: crate::ProviderSourceFailureKind::SourceDatabase,
            detail: error.to_string(),
        },
    }
}

pub(in crate::provider::providers::forgecode) struct ForgeCodeScanner {
    source: ForgeCodeSourceObservation,
    frontier: ForgeCodeFrontier,
    context: ProviderAdapterContext,
    source_root: Option<String>,
    wants_outputs: bool,
    exhausted: bool,
    active_decoded: Option<ForgeCodeDecodedRow>,
    decoded_rows: u64,
}

#[derive(Debug)]
pub(in crate::provider::providers::forgecode) struct ForgeCodePage {
    // Frontier and output-byte accounting remain part of the bounded page
    // contract even when the Core coordinator consumes only next_frontier.
    #[allow(dead_code)]
    pub(in crate::provider::providers::forgecode) expected_frontier: ForgeCodeFrontier,
    pub(in crate::provider::providers::forgecode) next_frontier: ForgeCodeFrontier,
    pub(in crate::provider::providers::forgecode) terminal: bool,
    pub(in crate::provider::providers::forgecode) row: Option<ForgeCodeConversationRow>,
    pub(in crate::provider::providers::forgecode) events: Vec<ForgeCodeRetainedEvent>,
    pub(in crate::provider::providers::forgecode) outputs: Vec<ProOutputObservation>,
    pub(in crate::provider::providers::forgecode) touches: Vec<ForgeCodeFileTouch>,
    pub(in crate::provider::providers::forgecode) rejections: Vec<ProviderImportFailure>,
    pub(in crate::provider::providers::forgecode) retained_bytes: usize,
    #[allow(dead_code)]
    pub(in crate::provider::providers::forgecode) retained_output_bytes: usize,
}

#[derive(Debug)]
pub(in crate::provider::providers::forgecode) struct ForgeCodeRetainedEvent {
    pub(in crate::provider::providers::forgecode) event: ForgeCodeNativeEvent,
    pub(in crate::provider::providers::forgecode) provider_event_index: u64,
}

#[derive(Debug, Clone)]
pub(in crate::provider::providers::forgecode) struct ForgeCodeConversationRow {
    pub(in crate::provider::providers::forgecode) rowid: i64,
    pub(in crate::provider::providers::forgecode) source_record_digest: [u8; 32],
    pub(in crate::provider::providers::forgecode) canonical_record_bytes: u64,
    pub(in crate::provider::providers::forgecode) conversation_id: String,
    pub(in crate::provider::providers::forgecode) title: Option<String>,
    pub(in crate::provider::providers::forgecode) workspace_id: i64,
    pub(in crate::provider::providers::forgecode) created_at: String,
    pub(in crate::provider::providers::forgecode) updated_at: Option<String>,
    pub(in crate::provider::providers::forgecode) context_metadata: Value,
    pub(in crate::provider::providers::forgecode) metrics_metadata: Option<Value>,
    // Exact context cardinality remains provider evidence for staging Pro.
    #[allow(dead_code)]
    pub(in crate::provider::providers::forgecode) context_message_count: usize,
    pub(in crate::provider::providers::forgecode) initiator: Option<String>,
}

struct ForgeCodeHydratedRow {
    rowid: i64,
    conversation_id: Vec<u8>,
    title: Option<Vec<u8>>,
    workspace_id: i64,
    context: Option<Vec<u8>>,
    created_at: Vec<u8>,
    updated_at: Option<Vec<u8>>,
    metrics: Option<Vec<u8>>,
}

#[derive(Clone)]
struct ForgeCodeDecodedRow {
    rowid: i64,
    conversation_id: String,
    title: Option<String>,
    workspace_id: i64,
    context: Option<String>,
    created_at: String,
    updated_at: Option<String>,
    metrics: Option<String>,
}

struct ForgeCodeRowDecodeError {
    field: &'static str,
}

impl ForgeCodeRowDecodeError {
    fn reason(&self) -> String {
        format!("ForgeCode conversations.{} is not valid UTF-8", self.field)
    }
}

impl ForgeCodeHydratedRow {
    fn decode(self) -> std::result::Result<ForgeCodeDecodedRow, ForgeCodeRowDecodeError> {
        Ok(ForgeCodeDecodedRow {
            rowid: self.rowid,
            conversation_id: decode_utf8(self.conversation_id, "conversation_id")?,
            title: decode_optional_utf8(self.title, "title")?,
            workspace_id: self.workspace_id,
            context: decode_optional_utf8(self.context, "context")?,
            created_at: decode_utf8(self.created_at, "created_at")?,
            updated_at: decode_optional_utf8(self.updated_at, "updated_at")?,
            metrics: decode_optional_utf8(self.metrics, "metrics")?,
        })
    }
}

struct ForgeCodeRowCandidate {
    rowid: i64,
    retained_bytes: i64,
    storage_classes: [String; 7],
}

impl ForgeCodeRowCandidate {
    fn observed_bytes(&self) -> Result<u64> {
        let retained = u64::try_from(self.retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload(
                "ForgeCode SQLite retained byte count must be nonnegative".to_owned(),
            )
        })?;
        FORGECODE_SQLITE_VALUE_OVERHEAD_BYTES
            .checked_add(retained)
            .ok_or(CaptureError::SystemInvariant(
                "ForgeCode SQLite retained byte count overflowed",
            ))
    }

    fn rejection_reason(&self) -> Option<&'static str> {
        let [conversation_id, title, workspace_id, context, created_at, updated_at, metrics] =
            self.storage_classes.each_ref();
        let castable_required = |kind: &str| matches!(kind, "integer" | "real" | "text");
        let castable_optional = |kind: &str| kind == "null" || castable_required(kind);
        let optional_text = |kind: &str| matches!(kind, "null" | "text");
        if !castable_required(conversation_id) {
            Some("ForgeCode conversations.conversation_id has an unsupported SQLite storage class")
        } else if !optional_text(title) {
            Some("ForgeCode conversations.title has an unsupported SQLite storage class")
        } else if workspace_id != "integer" {
            Some("ForgeCode conversations.workspace_id has an unsupported SQLite storage class")
        } else if !optional_text(context) {
            Some("ForgeCode conversations.context has an unsupported SQLite storage class")
        } else if !castable_required(created_at) {
            Some("ForgeCode conversations.created_at has an unsupported SQLite storage class")
        } else if !castable_optional(updated_at) {
            Some("ForgeCode conversations.updated_at has an unsupported SQLite storage class")
        } else if !optional_text(metrics) {
            Some("ForgeCode conversations.metrics has an unsupported SQLite storage class")
        } else {
            None
        }
    }
}

fn row_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<ForgeCodeRowCandidate> {
    Ok(ForgeCodeRowCandidate {
        rowid: row.get(0)?,
        retained_bytes: row.get(1)?,
        storage_classes: [
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
        ],
    })
}

fn with_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

fn retained_length_expr(expressions: &[&str]) -> String {
    let terms = expressions
        .iter()
        .map(|expression| {
            format!(
                "CASE WHEN {expression} IS NULL THEN 0 \
                 ELSE coalesce(octet_length(CAST({expression} AS BLOB)), 0) END"
            )
        })
        .collect::<Vec<_>>();
    format!("({})", terms.join(" + "))
}

fn decode_utf8(
    value: Vec<u8>,
    field: &'static str,
) -> std::result::Result<String, ForgeCodeRowDecodeError> {
    String::from_utf8(value).map_err(|_| ForgeCodeRowDecodeError { field })
}

fn decode_optional_utf8(
    value: Option<Vec<u8>>,
    field: &'static str,
) -> std::result::Result<Option<String>, ForgeCodeRowDecodeError> {
    value.map(|value| decode_utf8(value, field)).transpose()
}

fn context_without_messages(context: &Value) -> Value {
    let Some(object) = context.as_object() else {
        return Value::Null;
    };
    let metadata = object
        .iter()
        .filter(|(key, _)| key.as_str() != "messages")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    provider_capped_json_value(&Value::Object(metadata), PROVIDER_MAX_PREVIEW_CHARS)
}

fn estimated_row_bytes(row: &ForgeCodeConversationRow) -> usize {
    row.conversation_id
        .len()
        .saturating_add(row.title.as_deref().map(str::len).unwrap_or_default())
        .saturating_add(row.created_at.len())
        .saturating_add(row.updated_at.as_deref().map(str::len).unwrap_or_default())
        .saturating_add(row.initiator.as_deref().map(str::len).unwrap_or_default())
        .saturating_add(
            serde_json::to_vec(&row.context_metadata)
                .map(|bytes| bytes.len())
                .unwrap_or(usize::MAX),
        )
        .saturating_add(
            row.metrics_metadata
                .as_ref()
                .and_then(|value| serde_json::to_vec(value).ok())
                .map(|bytes| bytes.len())
                .unwrap_or_default(),
        )
        .saturating_add(512)
}

fn estimated_retained_event_bytes(event: &ForgeCodeRetainedEvent) -> usize {
    event
        .event
        .provider_event_hash
        .as_deref()
        .map(str::len)
        .unwrap_or_default()
        .saturating_add(event.event.cursor.len())
        .saturating_add(
            serde_json::to_vec(&event.event.payload)
                .map(|bytes| bytes.len())
                .unwrap_or(usize::MAX),
        )
        .saturating_add(
            serde_json::to_vec(&event.event.metadata)
                .map(|bytes| bytes.len())
                .unwrap_or(usize::MAX),
        )
        .saturating_add(512)
}

fn estimated_touch_bytes(touch: &ForgeCodeFileTouch) -> usize {
    serde_json::to_vec(touch)
        .map(|bytes| bytes.len().saturating_add(128))
        .unwrap_or(usize::MAX)
}

fn estimated_rejection_bytes(rejection: &ProviderImportFailure) -> usize {
    rejection.error.len().saturating_add(64)
}

fn estimated_output_bytes(output: &ProOutputObservation) -> usize {
    let optional = |value: Option<&str>| value.map(str::len).unwrap_or_default();
    let repository_bytes = output
        .associations
        .repository
        .as_ref()
        .map(|repository| {
            repository
                .repository_id
                .len()
                .saturating_add(optional(repository.checkout_id.as_deref()))
                .saturating_add(optional(repository.worktree_id.as_deref()))
                .saturating_add(optional(repository.object_format.as_deref()))
        })
        .unwrap_or_default();
    let command_bytes = output
        .command
        .as_ref()
        .map(|command| {
            command
                .tool_name
                .len()
                .saturating_add(command.command.len())
                .saturating_add(optional(command.working_directory.as_deref()))
        })
        .unwrap_or_default();
    output
        .coordinate
        .unit_key
        .len()
        .saturating_add(optional(output.coordinate.native_record_id.as_deref()))
        .saturating_add(output.associations.direct_session_id.len())
        .saturating_add(output.associations.root_session_id.len())
        .saturating_add(optional(output.associations.parent_session_id.as_deref()))
        .saturating_add(optional(output.associations.provider_session_id.as_deref()))
        .saturating_add(optional(output.associations.agent_id.as_deref()))
        .saturating_add(repository_bytes)
        .saturating_add(optional(output.call_id.as_deref()))
        .saturating_add(command_bytes)
        .saturating_add(output.locator.kind.len())
        .saturating_add(output.locator.payload.len())
        .saturating_add(output.content.len())
        .saturating_add(512)
}

fn output_outcome(parts: super::super::event::ForgeCodeMessageParts<'_>) -> OutputOutcomeMetadata {
    OutputOutcomeMetadata {
        outcome: match forgecode_tool_result_is_error(parts) {
            Some(true) => OutputOutcome::Failure,
            Some(false) => OutputOutcome::Success,
            None => OutputOutcome::Unknown,
        },
        exit_code: None,
        duration_ms: None,
    }
}

pub(super) fn ordered_rowid(rowid: i64) -> u64 {
    (rowid as u64) ^ (1_u64 << 63)
}

#[cfg(test)]
mod stock_sqlite_snapshot_tests {
    use std::{cell::Cell, ffi::OsString, fs, path::Path};

    use rusqlite::{config::DbConfig, params, Connection};

    use super::ForgeCodeSqliteDatabase;

    #[test]
    fn stock_snapshot_queries_active_wal_without_persistent_writes_and_rejects_swap() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source = temp.path().join("forgecode.sqlite");
        let attacker = temp.path().join("attacker.sqlite");
        let admitted = temp.path().join("admitted.sqlite");
        create_database(&source, "main");
        create_database(&attacker, "attacker");
        persist_wal_row(&source, "from-wal");
        let before_read = persistent_directory_snapshot(temp.path());

        let (database, opened_value) = ForgeCodeSqliteDatabase::open(&source, read_latest).unwrap();
        assert_eq!(opened_value, "from-wal");
        assert!(database.evidence().wal_length().is_some());
        assert!(database.evidence().shared_memory_length().is_some());
        assert_eq!(database.read(&source, read_latest).unwrap(), "from-wal");
        assert_eq!(persistent_directory_snapshot(temp.path()), before_read);

        fs::rename(&source, &admitted).unwrap();
        fs::rename(&attacker, &source).unwrap();
        let before_rejected_read = persistent_directory_snapshot(temp.path());
        let queried = Cell::new(false);
        let result = database.read(&source, |_| -> crate::Result<()> {
            queried.set(true);
            Ok(())
        });
        assert!(result.is_err());
        assert!(!queried.get());
        assert_eq!(
            persistent_directory_snapshot(temp.path()),
            before_rejected_read
        );
    }

    fn create_database(path: &Path, value: &str) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        connection
            .execute("INSERT INTO messages (body) VALUES (?1)", params![value])
            .unwrap();
    }

    fn persist_wal_row(path: &Path, value: &str) {
        let writer = Connection::open(path).unwrap();
        let mode: String = writer
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        writer
            .execute("INSERT INTO messages (body) VALUES (?1)", params![value])
            .unwrap();
        writer
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .unwrap();
        drop(writer);
        assert!(path.with_file_name("forgecode.sqlite-wal").exists());
        assert!(path.with_file_name("forgecode.sqlite-shm").exists());
    }

    fn read_latest(connection: &Connection) -> crate::Result<String> {
        Ok(connection.query_row(
            "SELECT body FROM messages ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )?)
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
}
