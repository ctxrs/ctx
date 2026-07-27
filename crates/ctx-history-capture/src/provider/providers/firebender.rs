use std::{
    cell::Cell,
    collections::BTreeSet,
    fs::{self},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use ctx_history_store::Store;
use rusqlite::{params, Connection, OptionalExtension, Statement};
use serde_json::{json, Value};

use crate::captured_batch::sqlite_logical_rows::{
    SqliteLogicalRow, SqliteLogicalRowBatchProducer, SqliteLogicalRowsBatchError,
};
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, CapturedSqliteValue, NativeLocator,
    NativePosition, ProviderRecordKind, SourceObservation, CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, BoundedParserCheckpoint, CapturedBatchCursorFinish,
    CapturedBatchCursorMode, CapturedBatchProjector, CapturedSourceAdmission,
    CertifiedProviderCursor, ProviderProjectionFatal, ProviderProjectionOutput,
    ProviderProjectionResult,
};
use crate::provider::normalization::{
    native_event, native_provider_capture, provider_capped_json, provider_json_text,
    provider_line_from_index, provider_role, provider_timestamp_millis, provider_timestamp_value,
    NativeEventDraft, NativeSessionDraft,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, open_provider_sqlite_readonly, sqlite_schema_fingerprint,
    sqlite_table_columns, sqlite_table_exists, with_sqlite_read_snapshot,
    ProviderSqliteSourceSnapshot,
};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    ProviderNormalizationResult, Result, FIREBENDER_SQLITE_SOURCE_FORMAT,
    PROVIDER_MAX_PREVIEW_CHARS,
};

mod message_text;
pub(crate) use message_text::{firebender_message_text, firebender_result_content};

const FIREBENDER_CAPTURE_REVISION: u32 = 2;
const FIREBENDER_POLICY_REVISION: u32 = 6;
const FIREBENDER_POSITION_KIND: &str = "firebender-chat-session-keyset-v1";
const FIREBENDER_LOCATOR_KIND: &str = "firebender-chat-session-row-v1";
const FIREBENDER_RECORD_KIND: &str = "firebender-chat-session-v1";
const FIREBENDER_POSITION_ROW_BYTES: usize = 1 + 8 + 8 + 8;
const FIREBENDER_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 4 * 5 + 2 * 9;

pub(crate) struct FirebenderChatSessionRow {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) messages_json: String,
    pub(crate) metadata_json: String,
}

fn firebender_source_snapshot(path: &Path) -> Result<ProviderSqliteSourceSnapshot> {
    ProviderSqliteSourceSnapshot::read(
        path,
        "Firebender SQLite source must be a regular non-symlink file",
        "Firebender SQLite sidecar must be a regular non-symlink file",
    )
}

fn firebender_source_revision(snapshot: &ProviderSqliteSourceSnapshot) -> String {
    format!(
        "firebender-sqlite-snapshot-v1:capture={FIREBENDER_CAPTURE_REVISION};policy={FIREBENDER_POLICY_REVISION};{}",
        snapshot.revision_component(),
    )
}

struct FirebenderCapturedBatchProjector {
    context: ProviderAdapterContext,
    database_path: PathBuf,
    schema_fingerprint: String,
}

impl FirebenderCapturedBatchProjector {
    fn new(
        context: ProviderAdapterContext,
        database_path: PathBuf,
        schema_fingerprint: String,
    ) -> Self {
        Self {
            context,
            database_path,
            schema_fingerprint,
        }
    }

    fn emit_capture(
        &self,
        line_number: usize,
        capture: ProviderCaptureEnvelope,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        output.emit_normalization(ProviderNormalizationResult {
            captures: vec![(line_number, capture)],
            ..ProviderNormalizationResult::default()
        })
    }
}

impl CapturedBatchProjector for FirebenderCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != FIREBENDER_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "Firebender projector received an unexpected record kind",
            ));
        }
        let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Firebender projector requires SQLite logical values",
            ));
        };
        let [CapturedSqliteValue::Text(id), CapturedSqliteValue::Text(name), CapturedSqliteValue::Integer(created_at), CapturedSqliteValue::Integer(updated_at), CapturedSqliteValue::Text(messages_json), CapturedSqliteValue::Text(metadata_json)] =
            values.as_slice()
        else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Firebender logical row has an invalid value shape",
            ));
        };
        let row_number = record
            .ordinal()
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Firebender row ordinal overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        let line_number = provider_line_from_index(row_number);
        let row = FirebenderChatSessionRow {
            id: id.clone(),
            name: name.clone(),
            created_at: *created_at,
            updated_at: *updated_at,
            messages_json: messages_json.clone(),
            metadata_json: metadata_json.clone(),
        };
        let started_at = provider_timestamp_millis(Some(row.created_at), self.context.imported_at);
        let ended_at = Some(provider_timestamp_millis(Some(row.updated_at), started_at));
        let metadata = provider_json_text(&row.metadata_json);
        let messages = match serde_json::from_str::<Value>(&row.messages_json) {
            Ok(Value::Array(messages)) => messages,
            Ok(_) => {
                output.reject_record(
                    line_number,
                    format!(
                        "Firebender session {} messages_json is not an array",
                        row.id
                    ),
                );
                Vec::new()
            }
            Err(error) => {
                output.reject_record(
                    line_number,
                    format!(
                        "Firebender session {} messages_json is invalid JSON: {error}",
                        row.id
                    ),
                );
                Vec::new()
            }
        };

        if messages.is_empty() {
            return self.emit_capture(
                line_number,
                firebender_capture(
                    &row,
                    &metadata,
                    &self.database_path,
                    started_at,
                    ended_at,
                    &self.schema_fingerprint,
                    &self.context,
                    None,
                ),
                output,
            );
        }

        for (message_index, message) in messages.into_iter().enumerate() {
            let provider_event_index = u64::try_from(message_index).map_err(|_| {
                ProviderProjectionFatal::system_invariant("Firebender message index exceeds u64")
            })?;
            let fallback_offset = i64::try_from(message_index).map_err(|_| {
                ProviderProjectionFatal::system_invariant("Firebender message index exceeds i64")
            })?;
            let occurred_at = firebender_message_time(
                &message,
                started_at + Duration::milliseconds(fallback_offset),
            );
            let mut event = firebender_event(&row.id, provider_event_index, &message, occurred_at);
            crate::complete_content::sqlite::attach_sqlite_complete_content_locator(
                &mut event,
                CaptureProvider::Firebender,
                FIREBENDER_SQLITE_SOURCE_FORMAT,
                record.locator(),
                values,
                || {
                    firebender_message_text(&message).unwrap_or_else(|| {
                        format!(
                            "Firebender {}",
                            message
                                .get("role")
                                .and_then(Value::as_str)
                                .unwrap_or("message")
                        )
                    })
                },
            )
            .map_err(ProviderProjectionFatal::new)?;
            crate::complete_content::sqlite::attach_sqlite_result_content_locator(
                &mut event,
                CaptureProvider::Firebender,
                FIREBENDER_SQLITE_SOURCE_FORMAT,
                record.locator(),
                values,
                firebender_result_content(&message),
            )
            .map_err(ProviderProjectionFatal::new)?;
            self.emit_capture(
                line_number,
                firebender_capture(
                    &row,
                    &metadata,
                    &self.database_path,
                    started_at,
                    ended_at,
                    &self.schema_fingerprint,
                    &self.context,
                    Some(event),
                ),
                output,
            )?;
        }
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if decode_firebender_position(position)?.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Firebender initial cursor candidate is not at the SQLite source start".to_owned(),
            ));
        }
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&())?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&())?,
            )?,
        ))
    }
}

struct FirebenderRowFetcher<'connection> {
    candidate: Statement<'connection>,
    hydration: Statement<'connection>,
    record_kind: ProviderRecordKind,
}

impl<'connection> FirebenderRowFetcher<'connection> {
    fn new(
        conn: &'connection Connection,
        columns: &BTreeSet<String>,
        record_kind: ProviderRecordKind,
    ) -> Result<Self> {
        let deleted_filter = if columns.contains("deleted_at") {
            "deleted_at is null and"
        } else {
            ""
        };
        let candidate_sql = format!(
            "select rowid, cast(updated_at as integer), \
                    length(cast(id as blob)), length(cast(name as blob)), \
                    length(cast(messages_json as blob)), length(cast(metadata_json as blob)) \
             from chat_sessions \
             where {deleted_filter} \
                   (?1 = 0 or cast(updated_at as integer) > ?2 \
                    or (cast(updated_at as integer) = ?2 and rowid > ?3)) \
             order by cast(updated_at as integer), rowid limit 1"
        );
        Ok(Self {
            candidate: conn.prepare(&candidate_sql)?,
            hydration: conn.prepare(
                "select id, name, cast(created_at as integer), cast(updated_at as integer), \
                        messages_json, metadata_json \
                 from chat_sessions where rowid = ?1 and cast(updated_at as integer) = ?2",
            )?,
            record_kind,
        })
    }

    fn fetch(&mut self, after: NativePosition) -> Result<Option<SqliteLogicalRow>> {
        let keyset = decode_firebender_position(&after)?;
        let (has_after, after_updated_at, after_rowid, ordinal) = match keyset {
            Some(keyset) => (1_i64, keyset.updated_at, keyset.rowid, keyset.next_ordinal),
            None => (0_i64, 0_i64, 0_i64, 0_u64),
        };
        let candidate = self
            .candidate
            .query_row(params![has_after, after_updated_at, after_rowid], |row| {
                Ok(FirebenderRowCandidate {
                    rowid: row.get(0)?,
                    updated_at: row.get(1)?,
                    id_bytes: row.get(2)?,
                    name_bytes: row.get(3)?,
                    messages_bytes: row.get(4)?,
                    metadata_bytes: row.get(5)?,
                })
            })
            .optional()?;
        let Some(candidate) = candidate else {
            return Ok(None);
        };
        let next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Firebender captured row ordinal overflowed",
        ))?;
        let next_position = encode_firebender_position(FirebenderKeyset {
            next_ordinal,
            updated_at: candidate.updated_at,
            rowid: candidate.rowid,
        })?;
        let locator = NativeLocator::new(
            FIREBENDER_LOCATOR_KIND,
            candidate.rowid.to_be_bytes().to_vec(),
        )
        .map_err(firebender_captured_error)?;
        let observed_bytes = candidate.retained_bytes()?;
        if observed_bytes
            > u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
                .map_err(|_| CaptureError::SystemInvariant("Firebender byte limit exceeds u64"))?
        {
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                locator,
                self.record_kind.clone(),
                observed_bytes,
            )
            .map(Some)
            .map_err(firebender_captured_error);
        }
        let row =
            self.hydration
                .query_row(params![candidate.rowid, candidate.updated_at], |row| {
                    Ok(vec![
                        CapturedSqliteValue::Text(row.get(0)?),
                        CapturedSqliteValue::Text(row.get(1)?),
                        CapturedSqliteValue::Integer(row.get(2)?),
                        CapturedSqliteValue::Integer(row.get(3)?),
                        CapturedSqliteValue::Text(row.get(4)?),
                        CapturedSqliteValue::Text(row.get(5)?),
                    ])
                })?;
        SqliteLogicalRow::values(
            next_position,
            ordinal,
            locator,
            self.record_kind.clone(),
            row,
        )
        .map(Some)
        .map_err(firebender_captured_error)
    }
}

struct FirebenderRowCandidate {
    rowid: i64,
    updated_at: i64,
    id_bytes: i64,
    name_bytes: i64,
    messages_bytes: i64,
    metadata_bytes: i64,
}

impl FirebenderRowCandidate {
    fn retained_bytes(&self) -> Result<u64> {
        [
            self.id_bytes,
            self.name_bytes,
            self.messages_bytes,
            self.metadata_bytes,
        ]
        .into_iter()
        .try_fold(FIREBENDER_SQLITE_VALUE_OVERHEAD_BYTES, |total, bytes| {
            let bytes = u64::try_from(bytes).map_err(|_| {
                CaptureError::InvalidPayload(
                    "Firebender SQLite text length must be nonnegative".to_owned(),
                )
            })?;
            total
                .checked_add(bytes)
                .ok_or(CaptureError::SystemInvariant(
                    "Firebender SQLite retained byte count overflowed",
                ))
        })
    }
}

#[derive(Clone, Copy)]
struct FirebenderKeyset {
    next_ordinal: u64,
    updated_at: i64,
    rowid: i64,
}

fn initial_firebender_position() -> Result<NativePosition> {
    NativePosition::new(FIREBENDER_POSITION_KIND, vec![0]).map_err(firebender_captured_error)
}

fn encode_firebender_position(keyset: FirebenderKeyset) -> Result<NativePosition> {
    let mut value = Vec::with_capacity(FIREBENDER_POSITION_ROW_BYTES);
    value.push(1);
    value.extend_from_slice(&keyset.next_ordinal.to_be_bytes());
    value.extend_from_slice(&firebender_ordered_i64(keyset.updated_at).to_be_bytes());
    value.extend_from_slice(&firebender_ordered_i64(keyset.rowid).to_be_bytes());
    NativePosition::new(FIREBENDER_POSITION_KIND, value).map_err(firebender_captured_error)
}

fn decode_firebender_position(position: &NativePosition) -> Result<Option<FirebenderKeyset>> {
    if position.kind() != FIREBENDER_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "Firebender cursor has an unexpected native-position kind".to_owned(),
        ));
    }
    if position.value() == [0] {
        return Ok(None);
    }
    if position.value().len() != FIREBENDER_POSITION_ROW_BYTES || position.value()[0] != 1 {
        return Err(CaptureError::InvalidPayload(
            "Firebender cursor has an invalid native-position payload".to_owned(),
        ));
    }
    let next_ordinal = firebender_decode_u64(&position.value()[1..9])?;
    let updated_at = firebender_unordered_i64(firebender_decode_u64(&position.value()[9..17])?);
    let rowid = firebender_unordered_i64(firebender_decode_u64(&position.value()[17..25])?);
    Ok(Some(FirebenderKeyset {
        next_ordinal,
        updated_at,
        rowid,
    }))
}

fn firebender_decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload("Firebender cursor integer has an invalid width".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

fn firebender_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

fn firebender_unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}

fn firebender_captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

fn firebender_sqlite_batch_error(error: SqliteLogicalRowsBatchError<CaptureError>) -> CaptureError {
    match error {
        SqliteLogicalRowsBatchError::Callback(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

pub(crate) fn import_firebender_sqlite_batched(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let database_path = firebender_chat_history_db_path(path)?;
    let mut context = context;
    context.source_path = Some(database_path.clone());
    let canonical_database_path = fs::canonicalize(&database_path)?;
    let snapshot = firebender_source_snapshot(&database_path)?;
    let cursor_source_path = provider_path_identity(&canonical_database_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        &cursor_source_path,
    );
    let source = SourceObservation::new(
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        format!("firebender-sqlite:{cursor_source_path}"),
        firebender_source_revision(&snapshot),
        cursor_stream,
        FIREBENDER_CAPTURE_REVISION,
        FIREBENDER_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(firebender_captured_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let initial_position = initial_firebender_position()?;
    let mut start_position = initial_position.clone();
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.matches_revisions(
                    source.source_revision(),
                    source.capture_revision(),
                    source.policy_revision(),
                ) =>
            {
                let _: () = certified.parser_checkpoint().deserialize()?;
                decode_firebender_position(certified.native_position())?;
                start_position = certified.native_position().clone();
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;

    let conn = open_provider_sqlite_readonly(&database_path)?;
    if !snapshot.revalidate(&database_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    if !sqlite_table_exists(&conn, "chat_sessions")? {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: database_path,
            reason: "Firebender chat_history.db is missing required chat_sessions table",
        });
    }
    let columns = sqlite_table_columns(&conn, "chat_sessions")?;
    ensure_sqlite_table_columns(
        &columns,
        "Firebender chat_sessions table",
        &[
            "id",
            "name",
            "created_at",
            "updated_at",
            "messages_json",
            "metadata_json",
        ],
    )?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;

    let record_kind =
        ProviderRecordKind::new(FIREBENDER_RECORD_KIND).map_err(firebender_captured_error)?;
    let source_exhausted = Cell::new(false);
    let producer_source_exhausted = &source_exhausted;
    let mut fetcher = FirebenderRowFetcher::new(&conn, &columns, record_kind)?;
    let mut producer = Some(SqliteLogicalRowBatchProducer::new(
        source,
        start_position,
        move |position| {
            let row = fetcher.fetch(position)?;
            if row.is_none() {
                producer_source_exhausted.set(true);
            }
            Ok(row)
        },
    ));
    let mut projector = FirebenderCapturedBatchProjector::new(
        context.clone(),
        database_path.clone(),
        schema_fingerprint,
    );
    drain_captured_batches(
        store,
        &admission,
        import_options,
        &context.machine_id,
        context.imported_at,
        expected_store_cursor,
        &initial_position,
        cursor_mode,
        &stream,
        &mut projector,
        || {
            let Some(active_producer) = producer.as_mut() else {
                return Ok(None);
            };
            if !snapshot.revalidate(&database_path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let batch = with_sqlite_read_snapshot(&conn, || {
                active_producer
                    .next_batch()
                    .map_err(firebender_sqlite_batch_error)
            })?;
            if !snapshot.revalidate(&database_path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            if source_exhausted.get() {
                producer.take();
            }
            Ok(batch)
        },
        || snapshot.revalidate(&database_path),
    )
}

pub(crate) fn firebender_chat_history_db_path(path: &Path) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "symlinked provider transcript roots are rejected",
        });
    }
    if file_type.is_file() {
        return Ok(path.to_path_buf());
    }
    if file_type.is_dir() {
        let db_path = path
            .join(".idea")
            .join("firebender")
            .join("chat_history.db");
        if db_path.exists() {
            return Ok(db_path);
        }
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Firebender project root is missing .idea/firebender/chat_history.db",
        });
    }
    Err(CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason: "Firebender import path must be chat_history.db or a project root",
    })
}

pub(crate) fn firebender_source_root(path: &Path) -> Result<PathBuf> {
    let db_path = firebender_chat_history_db_path(path)?;
    let Some(firebender_dir) = db_path.parent() else {
        return Ok(db_path);
    };
    if firebender_dir.file_name().and_then(|name| name.to_str()) != Some("firebender") {
        return Ok(db_path);
    }
    let Some(idea_dir) = firebender_dir.parent() else {
        return Ok(db_path);
    };
    if idea_dir.file_name().and_then(|name| name.to_str()) != Some(".idea") {
        return Ok(db_path);
    }
    Ok(idea_dir.parent().unwrap_or(&db_path).to_path_buf())
}

pub(crate) fn firebender_message_time(message: &Value, fallback: DateTime<Utc>) -> DateTime<Utc> {
    provider_timestamp_value(
        message
            .get("timestamp")
            .or_else(|| message.get("created_at"))
            .or_else(|| message.get("updated_at")),
        fallback,
    )
}

pub(crate) fn firebender_event(
    provider_session_id: &str,
    provider_event_index: u64,
    message: &Value,
    occurred_at: DateTime<Utc>,
) -> ProviderEventEnvelope {
    let role = message.get("role").and_then(Value::as_str);
    let tool_calls = message
        .get("tool_calls")
        .or_else(|| message.get("toolCalls"));
    let event_type = if role == Some("tool") {
        EventType::ToolOutput
    } else if tool_calls.is_some_and(|value| {
        value
            .as_array()
            .map(|items| !items.is_empty())
            .unwrap_or(true)
    }) {
        EventType::ToolCall
    } else {
        EventType::Message
    };
    native_event(NativeEventDraft {
        provider: CaptureProvider::Firebender,
        source_format: FIREBENDER_SQLITE_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index,
        provider_event_hash: message
            .get("id")
            .or_else(|| message.get("tool_call_id"))
            .or_else(|| message.get("toolCallId"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        cursor: format!("chat_sessions:{provider_session_id}:message:{provider_event_index}"),
        event_type,
        role: Some(provider_role(role)),
        occurred_at,
        text: firebender_message_text(message)
            .unwrap_or_else(|| format!("Firebender {}", role.unwrap_or("message"))),
        body: message.clone(),
        metadata: json!({
            "source": "firebender_chat_sessions",
            "source_format": FIREBENDER_SQLITE_SOURCE_FORMAT,
            "role": role,
            "name": message.get("name").and_then(Value::as_str),
            "tool_call_id": message
                .get("tool_call_id")
                .or_else(|| message.get("toolCallId"))
                .and_then(Value::as_str),
            "content_type": message
                .get("content")
                .and_then(|content| content.get("type"))
                .and_then(Value::as_str),
        }),
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn firebender_capture(
    row: &FirebenderChatSessionRow,
    metadata: &Value,
    path: &Path,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    schema_fingerprint: &str,
    context: &ProviderAdapterContext,
    event: Option<ProviderEventEnvelope>,
) -> ProviderCaptureEnvelope {
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Firebender,
            source_format: FIREBENDER_SQLITE_SOURCE_FORMAT,
            provider_session_id: row.id.clone(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            started_at,
            ended_at,
            cwd: None,
            fidelity: Fidelity::Imported,
            raw_source_path: path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": FIREBENDER_SQLITE_SOURCE_FORMAT,
                "schema_fingerprint": schema_fingerprint,
                "storage": ".idea/firebender/chat_history.db",
            }),
            session_metadata: json!({
                "source_format": FIREBENDER_SQLITE_SOURCE_FORMAT,
                "title": row.name,
                "metadata": provider_capped_json(metadata, PROVIDER_MAX_PREVIEW_CHARS),
                "storage": ".idea/firebender/chat_history.db",
                "timestamp_note": "message rows do not carry durable per-message timestamps; ctx preserves session created_at/updated_at and import order",
            }),
        },
        context,
        event,
    )
}

#[cfg(test)]
mod captured_batch_tests;
