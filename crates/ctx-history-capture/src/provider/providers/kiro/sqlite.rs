use std::path::PathBuf;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, Fidelity, ProviderCaptureEnvelope, ProviderEventEnvelope,
    ProviderSourceTrust,
};
use rusqlite::{Connection, OptionalExtension, Statement};
use serde_json::{json, Value};

use crate::captured_batch::sqlite_logical_rows::{SqliteLogicalRow, SqliteLogicalRowsBatchError};
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, CapturedSqliteValue, NativeLocator,
    NativePosition, ProviderRecordKind, SourceObservation, CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::provider::file_touches::{
    event_type_supports_structured_file_touches, visit_provider_file_touches_with_context,
    ProviderFileTouchEnvelopeContext, PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
};
use crate::provider::importer::{
    BoundedParserCheckpoint, CapturedBatchCursorFinish, CapturedBatchProjector,
    CertifiedProviderCursor, ProviderProjectionFatal, ProviderProjectionOutput,
    ProviderProjectionResult,
};
use crate::provider::normalization::{
    native_provider_capture, provider_capped_json_value, provider_line_from_index,
    provider_nonnegative_i64_to_u64, NativeSessionDraft,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
    SqliteLengthPreflightGuard,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    KIRO_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::history::{
    decode_kiro_conversation, kiro_history_events, kiro_provider_session_id, kiro_session_ended_at,
    kiro_session_started_at, KiroConversationRow, KIRO_LEGACY_RECORD_KIND, KIRO_V2_RECORD_KIND,
};
const KIRO_POSITION_KIND: &str = "kiro-conversation-keyset-v1";
const KIRO_LOCATOR_KIND: &str = "kiro-conversation-row-v1";
const KIRO_REJECTED_RECORD_KIND: &str = "kiro-conversation-rejected-v1";
const KIRO_POSITION_ROW_BYTES: usize = 1 + 8 + 8;
const KIRO_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 6 * 64;
const KIRO_V2_CANDIDATE_SELECT_SQL: &str = "select rowid, coalesce(octet_length(key), 0) + \
        coalesce(octet_length(conversation_id), 0) + \
        coalesce(octet_length(value), 0) + \
        case when typeof(created_at) in ('null', 'integer') then 0 \
             else coalesce(octet_length(created_at), 0) end + \
        case when typeof(updated_at) in ('null', 'integer') then 0 \
             else coalesce(octet_length(updated_at), 0) end, \
        typeof(key) = 'text', \
        typeof(conversation_id) = 'text', \
        typeof(value) = 'text', \
        typeof(created_at) in ('null', 'integer'), \
        typeof(updated_at) in ('null', 'integer') \
     from conversations_v2";
const KIRO_LEGACY_CANDIDATE_SELECT_SQL: &str = "select rowid, coalesce(octet_length(key), 0) + \
        coalesce(octet_length(value), 0), \
        typeof(key) = 'text', 1, typeof(value) = 'text', 1, 1 \
     from conversations";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KiroConversationPhase {
    V2,
    Legacy,
}

impl KiroConversationPhase {
    fn tag(self) -> u8 {
        match self {
            Self::V2 => 1,
            Self::Legacy => 2,
        }
    }

    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(Self::V2),
            2 => Ok(Self::Legacy),
            _ => Err(CaptureError::InvalidPayload(
                "Kiro cursor has an unknown conversation phase".to_owned(),
            )),
        }
    }
}

#[derive(Clone, Copy)]
struct KiroKeyset {
    phase: KiroConversationPhase,
    next_ordinal: u64,
    rowid: i64,
}

#[derive(Clone, Copy)]
pub(super) struct KiroConversationTables {
    v2: bool,
    legacy: bool,
}

pub(super) struct KiroRowFetcher<'connection> {
    conn: &'connection Connection,
    v2_candidates: Option<KiroCandidateStatements<'connection>>,
    v2_hydration: Option<Statement<'connection>>,
    legacy_candidates: Option<KiroCandidateStatements<'connection>>,
    legacy_hydration: Option<Statement<'connection>>,
    v2_record_kind: ProviderRecordKind,
    legacy_record_kind: ProviderRecordKind,
    rejected_record_kind: ProviderRecordKind,
}

impl<'connection> KiroRowFetcher<'connection> {
    pub(super) fn new(
        conn: &'connection Connection,
        tables: KiroConversationTables,
    ) -> Result<Self> {
        Ok(Self {
            conn,
            v2_candidates: tables
                .v2
                .then(|| KiroCandidateStatements::new(conn, KIRO_V2_CANDIDATE_SELECT_SQL))
                .transpose()?,
            v2_hydration: tables
                .v2
                .then(|| {
                    conn.prepare(
                        "select rowid, key, conversation_id, value, created_at, updated_at \
                     from conversations_v2 where rowid = ?1",
                    )
                })
                .transpose()?,
            legacy_candidates: tables
                .legacy
                .then(|| KiroCandidateStatements::new(conn, KIRO_LEGACY_CANDIDATE_SELECT_SQL))
                .transpose()?,
            legacy_hydration: tables
                .legacy
                .then(|| {
                    conn.prepare("select rowid, key, value from conversations where rowid = ?1")
                })
                .transpose()?,
            v2_record_kind: ProviderRecordKind::new(KIRO_V2_RECORD_KIND)
                .map_err(kiro_captured_error)?,
            legacy_record_kind: ProviderRecordKind::new(KIRO_LEGACY_RECORD_KIND)
                .map_err(kiro_captured_error)?,
            rejected_record_kind: ProviderRecordKind::new(KIRO_REJECTED_RECORD_KIND)
                .map_err(kiro_captured_error)?,
        })
    }

    pub(super) fn fetch(&mut self, after: NativePosition) -> Result<Option<SqliteLogicalRow>> {
        let keyset = decode_kiro_position(&after)?;
        let (phase, after_rowid, ordinal) = match keyset {
            Some(keyset) => (keyset.phase, Some(keyset.rowid), keyset.next_ordinal),
            None if self.v2_candidates.is_some() => (KiroConversationPhase::V2, None, 0),
            None => (KiroConversationPhase::Legacy, None, 0),
        };
        if phase == KiroConversationPhase::V2 {
            if let Some(candidate) =
                kiro_fetch_candidate(self.conn, &mut self.v2_candidates, after_rowid)?
            {
                return self.hydrate(candidate, phase, ordinal).map(Some);
            }
            return kiro_fetch_candidate(self.conn, &mut self.legacy_candidates, None)?.map_or(
                Ok(None),
                |candidate| {
                    self.hydrate(candidate, KiroConversationPhase::Legacy, ordinal)
                        .map(Some)
                },
            );
        }
        kiro_fetch_candidate(self.conn, &mut self.legacy_candidates, after_rowid)?
            .map_or(Ok(None), |candidate| {
                self.hydrate(candidate, phase, ordinal).map(Some)
            })
    }

    fn hydrate(
        &mut self,
        candidate: KiroCandidate,
        phase: KiroConversationPhase,
        ordinal: u64,
    ) -> Result<SqliteLogicalRow> {
        let next_position = encode_kiro_position(KiroKeyset {
            phase,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Kiro captured row ordinal overflowed",
            ))?,
            rowid: candidate.rowid,
        })?;
        let locator = kiro_locator(phase, candidate.rowid)?;
        let observed_bytes = candidate.observed_bytes()?;
        let record_kind = match phase {
            KiroConversationPhase::V2 => self.v2_record_kind.clone(),
            KiroConversationPhase::Legacy => self.legacy_record_kind.clone(),
        };
        if observed_bytes > kiro_oversize_limit()? {
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                locator,
                record_kind,
                observed_bytes,
            )
            .map_err(kiro_captured_error);
        }
        if let Some(reason) = candidate.rejection_reason(phase) {
            return SqliteLogicalRow::values(
                next_position,
                ordinal,
                locator,
                self.rejected_record_kind.clone(),
                vec![
                    CapturedSqliteValue::Integer(candidate.rowid),
                    CapturedSqliteValue::Text(reason.to_owned()),
                ],
            )
            .map_err(kiro_captured_error);
        }
        let values = match phase {
            KiroConversationPhase::V2 => self
                .v2_hydration
                .as_mut()
                .ok_or(CaptureError::SystemInvariant(
                    "Kiro v2 hydration statement is unavailable",
                ))?
                .query_row([candidate.rowid], kiro_v2_values)?,
            KiroConversationPhase::Legacy => self
                .legacy_hydration
                .as_mut()
                .ok_or(CaptureError::SystemInvariant(
                    "Kiro legacy hydration statement is unavailable",
                ))?
                .query_row([candidate.rowid], kiro_legacy_values)?,
        };
        SqliteLogicalRow::values(next_position, ordinal, locator, record_kind, values)
            .map_err(kiro_captured_error)
    }
}

struct KiroCandidateStatements<'connection> {
    first: Statement<'connection>,
    next: Statement<'connection>,
}

impl<'connection> KiroCandidateStatements<'connection> {
    fn new(conn: &'connection Connection, select_sql: &str) -> rusqlite::Result<Self> {
        Ok(Self {
            first: conn.prepare(&kiro_candidate_sql(select_sql, KiroCandidateSeek::First))?,
            next: conn.prepare(&kiro_candidate_sql(select_sql, KiroCandidateSeek::Next))?,
        })
    }
}

#[derive(Clone, Copy)]
enum KiroCandidateSeek {
    First,
    Next,
}

fn kiro_candidate_sql(select_sql: &str, seek: KiroCandidateSeek) -> String {
    match seek {
        KiroCandidateSeek::First => format!("{select_sql} order by rowid limit 1"),
        KiroCandidateSeek::Next => {
            format!("{select_sql} where rowid > ?1 order by rowid limit 1")
        }
    }
}

struct KiroCandidate {
    rowid: i64,
    retained_bytes: i64,
    type_valid: [bool; 5],
}

impl KiroCandidate {
    fn observed_bytes(&self) -> Result<u64> {
        let payload = u64::try_from(self.retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload(
                "Kiro SQLite retained byte count must be nonnegative".to_owned(),
            )
        })?;
        KIRO_SQLITE_VALUE_OVERHEAD_BYTES
            .checked_add(payload)
            .ok_or(CaptureError::SystemInvariant(
                "Kiro SQLite retained byte count overflowed",
            ))
    }

    fn rejection_reason(&self, phase: KiroConversationPhase) -> Option<&'static str> {
        let [key, conversation_id, value, created_at, updated_at] = self.type_valid;
        if !key {
            Some(match phase {
                KiroConversationPhase::V2 => {
                    "Kiro conversations_v2.key has an unsupported SQLite storage class"
                }
                KiroConversationPhase::Legacy => {
                    "Kiro conversations.key has an unsupported SQLite storage class"
                }
            })
        } else if phase == KiroConversationPhase::V2 && !conversation_id {
            Some("Kiro conversations_v2.conversation_id has an unsupported SQLite storage class")
        } else if !value {
            Some(match phase {
                KiroConversationPhase::V2 => {
                    "Kiro conversations_v2.value has an unsupported SQLite storage class"
                }
                KiroConversationPhase::Legacy => {
                    "Kiro conversations.value has an unsupported SQLite storage class"
                }
            })
        } else if phase == KiroConversationPhase::V2 && !created_at {
            Some("Kiro conversations_v2.created_at has an unsupported SQLite storage class")
        } else if phase == KiroConversationPhase::V2 && !updated_at {
            Some("Kiro conversations_v2.updated_at has an unsupported SQLite storage class")
        } else {
            None
        }
    }
}

fn kiro_fetch_candidate(
    conn: &Connection,
    statements: &mut Option<KiroCandidateStatements<'_>>,
    after_rowid: Option<i64>,
) -> Result<Option<KiroCandidate>> {
    let Some(statements) = statements.as_mut() else {
        return Ok(None);
    };
    with_kiro_length_preflight(conn, || match after_rowid {
        Some(rowid) => statements
            .next
            .query_row([rowid], kiro_candidate_from_row)
            .optional(),
        None => statements
            .first
            .query_row([], kiro_candidate_from_row)
            .optional(),
    })
}

fn kiro_candidate_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<KiroCandidate> {
    Ok(KiroCandidate {
        rowid: row.get(0)?,
        retained_bytes: row.get(1)?,
        type_valid: [
            row.get::<_, i64>(2)? != 0,
            row.get::<_, i64>(3)? != 0,
            row.get::<_, i64>(4)? != 0,
            row.get::<_, i64>(5)? != 0,
            row.get::<_, i64>(6)? != 0,
        ],
    })
}

fn with_kiro_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    // SQLITE_LIMIT_LENGTH rejects even integer-only octet_length inspection of
    // an oversized stored record. Candidate SQL returns only rowid and byte
    // counts, so temporarily lift the limit and restore it before hydration.
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

fn kiro_v2_values(row: &rusqlite::Row<'_>) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    Ok(vec![
        CapturedSqliteValue::Integer(row.get(0)?),
        CapturedSqliteValue::Text(row.get(1)?),
        CapturedSqliteValue::Text(row.get(2)?),
        CapturedSqliteValue::Text(row.get(3)?),
        kiro_optional_integer_value(row.get(4)?),
        kiro_optional_integer_value(row.get(5)?),
    ])
}

fn kiro_legacy_values(row: &rusqlite::Row<'_>) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    Ok(vec![
        CapturedSqliteValue::Integer(row.get(0)?),
        CapturedSqliteValue::Text(row.get(1)?),
        CapturedSqliteValue::Text(row.get(2)?),
    ])
}

fn kiro_optional_integer_value(value: Option<i64>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Integer)
}

pub(super) struct KiroCapturedBatchProjector {
    context: ProviderAdapterContext,
    database_path: PathBuf,
    user_version: i64,
    schema_fingerprint: String,
}

pub(crate) struct KiroCaptureData<'a> {
    pub(crate) row: &'a KiroConversationRow,
    pub(crate) provider_session_id: &'a str,
    pub(crate) value: &'a Value,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
}

impl KiroCapturedBatchProjector {
    pub(super) fn new(
        context: ProviderAdapterContext,
        database_path: PathBuf,
        user_version: i64,
        schema_fingerprint: String,
    ) -> Self {
        Self {
            context,
            database_path,
            user_version,
            schema_fingerprint,
        }
    }

    fn emit_capture(
        &self,
        line: usize,
        capture: &KiroCaptureData<'_>,
        raw_entry: Option<&Value>,
        event: Option<ProviderEventEnvelope>,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let raw_source_path = self.database_path.display().to_string();
        let source_root = self.context.source_root_display();
        output.use_explicit_file_touches();
        let touch_context = event.as_ref().zip(raw_entry).map(|(event, entry)| {
            (
                entry,
                event_type_supports_structured_file_touches(event.event_type),
                event.occurred_at,
                event.provider_event_index,
            )
        });
        let mut pending_capture = Some((
            line,
            kiro_capture(
                capture,
                &raw_source_path,
                self.user_version,
                &self.schema_fingerprint,
                event,
                &self.context,
            ),
        ));
        let mut file_touch_outcome = None;
        if let Some((entry, include_structured_touches, occurred_at, provider_event_index)) =
            touch_context
        {
            file_touch_outcome = Some(visit_provider_file_touches_with_context(
                ProviderFileTouchEnvelopeContext {
                    provider: CaptureProvider::KiroCli,
                    provider_session_id: capture.provider_session_id,
                    source_format: KIRO_SQLITE_SOURCE_FORMAT,
                    raw_source_path: Some(raw_source_path.as_str()),
                    source_root: source_root.as_deref(),
                    occurred_at,
                    provider_event_index: Some(provider_event_index),
                    provider_touch_base_index: provider_event_index << 16,
                    line_number: line,
                },
                entry,
                include_structured_touches,
                |file_touch| {
                    output.emit_normalization(ProviderNormalizationResult {
                        captures: pending_capture.take().into_iter().collect(),
                        files_touched: vec![file_touch],
                        ..ProviderNormalizationResult::default()
                    })
                },
            )?);
        }
        if let Some(capture) = pending_capture {
            output.emit_normalization(ProviderNormalizationResult {
                captures: vec![capture],
                ..ProviderNormalizationResult::default()
            })?;
        }
        if file_touch_outcome.is_some_and(|outcome| outcome.limit_exceeded()) {
            output.reject_record(line, PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned());
        }
        Ok(())
    }
}

impl CapturedBatchProjector for KiroCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Kiro projector requires SQLite logical values",
            ));
        };
        if record.record_kind().as_str() == KIRO_REJECTED_RECORD_KIND {
            let [CapturedSqliteValue::Integer(rowid), CapturedSqliteValue::Text(reason)] =
                values.as_slice()
            else {
                return Err(ProviderProjectionFatal::system_invariant(
                    "Kiro rejected conversation has an invalid value shape",
                ));
            };
            let row_index =
                provider_nonnegative_i64_to_u64(*rowid, "Kiro conversation rowid").unwrap_or(0);
            output.reject_record(provider_line_from_index(row_index), reason.clone());
            return Ok(());
        }
        let row = decode_kiro_conversation(record.record_kind().as_str(), values)
            .map_err(ProviderProjectionFatal::new)?;
        let row_index = match provider_nonnegative_i64_to_u64(row.rowid, "Kiro conversation rowid")
        {
            Ok(index) => index,
            Err(error) => {
                output.reject_record(0, error.to_string());
                return Ok(());
            }
        };
        let line = provider_line_from_index(row_index);
        let value: Value = match serde_json::from_str(&row.value) {
            Ok(value) => value,
            Err(error) => {
                output.reject_record(
                    line,
                    format!(
                        "invalid JSON in Kiro {} row {} for key {}: {error}",
                        row.table, row.rowid, row.key
                    ),
                );
                return Ok(());
            }
        };
        let provider_session_id = kiro_provider_session_id(&row, &value);
        let started_at = kiro_session_started_at(&row, &value, self.context.imported_at);
        let ended_at = Some(kiro_session_ended_at(&row, &value, started_at));
        let capture = KiroCaptureData {
            row: &row,
            provider_session_id: &provider_session_id,
            value: &value,
            started_at,
            ended_at,
        };
        let mut emitted_event = false;
        for decoded in kiro_history_events(&row, &provider_session_id, &value, started_at) {
            let (mut event, entry, complete_text) = decoded.into_projection_parts();
            crate::complete_content::sqlite::attach_sqlite_complete_content_locator(
                &mut event,
                CaptureProvider::KiroCli,
                KIRO_SQLITE_SOURCE_FORMAT,
                record.locator(),
                values,
                complete_text,
            )
            .map_err(ProviderProjectionFatal::new)?;
            self.emit_capture(line, &capture, Some(entry), Some(event), output)?;
            emitted_event = true;
        }

        if !emitted_event {
            self.emit_capture(line, &capture, None, None, output)?;
        }
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if *position != initial_kiro_position()? {
            return Err(CaptureError::InvalidPayload(
                "Kiro initial cursor candidate is not at the SQLite source start".to_owned(),
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

pub(super) fn initial_kiro_position() -> Result<NativePosition> {
    NativePosition::new(KIRO_POSITION_KIND, vec![0]).map_err(kiro_captured_error)
}

fn encode_kiro_position(keyset: KiroKeyset) -> Result<NativePosition> {
    let mut value = Vec::with_capacity(KIRO_POSITION_ROW_BYTES);
    value.push(keyset.phase.tag());
    value.extend_from_slice(&keyset.next_ordinal.to_be_bytes());
    value.extend_from_slice(&kiro_ordered_i64(keyset.rowid).to_be_bytes());
    NativePosition::new(KIRO_POSITION_KIND, value).map_err(kiro_captured_error)
}

fn decode_kiro_position(position: &NativePosition) -> Result<Option<KiroKeyset>> {
    if position.kind() != KIRO_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "Kiro cursor has an unexpected native-position kind".to_owned(),
        ));
    }
    if position.value() == [0] {
        return Ok(None);
    }
    if position.value().len() != KIRO_POSITION_ROW_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Kiro cursor has an invalid native-position payload".to_owned(),
        ));
    }
    Ok(Some(KiroKeyset {
        phase: KiroConversationPhase::from_tag(position.value()[0])?,
        next_ordinal: kiro_decode_u64(&position.value()[1..9])?,
        rowid: kiro_unordered_i64(kiro_decode_u64(&position.value()[9..17])?),
    }))
}

pub(super) fn validate_kiro_position(position: &NativePosition) -> Result<()> {
    decode_kiro_position(position).map(drop)
}

fn kiro_locator(phase: KiroConversationPhase, rowid: i64) -> Result<NativeLocator> {
    let mut value = Vec::with_capacity(1 + 8);
    value.push(phase.tag());
    value.extend_from_slice(&kiro_ordered_i64(rowid).to_be_bytes());
    NativeLocator::new(KIRO_LOCATOR_KIND, value).map_err(kiro_captured_error)
}

fn kiro_decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload("Kiro cursor integer has an invalid width".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

fn kiro_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

fn kiro_unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}

fn kiro_oversize_limit() -> Result<u64> {
    u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("Kiro byte limit exceeds u64"))
}

pub(super) fn kiro_captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(super) fn kiro_sqlite_batch_error(
    error: SqliteLogicalRowsBatchError<CaptureError>,
) -> CaptureError {
    match error {
        SqliteLogicalRowsBatchError::Callback(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn kiro_capture(
    capture: &KiroCaptureData<'_>,
    raw_source_path: &str,
    user_version: i64,
    schema_fingerprint: &str,
    event: Option<ProviderEventEnvelope>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let row = capture.row;
    let provider_session_id = capture.provider_session_id;
    let value = capture.value;
    let started_at = capture.started_at;
    let ended_at = capture.ended_at;
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::KiroCli,
            source_format: KIRO_SQLITE_SOURCE_FORMAT,
            provider_session_id: provider_session_id.to_owned(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            started_at,
            ended_at,
            cwd: (!row.key.trim().is_empty()).then(|| row.key.clone()),
            fidelity: Fidelity::Imported,
            raw_source_path: raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": KIRO_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": user_version,
                "schema_fingerprint": schema_fingerprint,
                "source_path": raw_source_path,
                "table": row.table,
            }),
            session_metadata: json!({
                "source_format": KIRO_SQLITE_SOURCE_FORMAT,
                "table": row.table,
                "key": row.key,
                "conversation_id": provider_session_id,
                "created_at": row.created_at,
                "updated_at": row.updated_at,
                "history_len": value
                    .get("history")
                    .and_then(Value::as_array)
                    .map(Vec::len),
                "conversation": provider_capped_json_value(value, PROVIDER_MAX_PREVIEW_CHARS),
            }),
        },
        context,
        event,
    )
}

pub(super) fn kiro_conversation_tables(conn: &Connection) -> Result<KiroConversationTables> {
    let v2 = sqlite_table_exists(conn, "conversations_v2")?;
    if v2 {
        ensure_sqlite_table_columns(
            &sqlite_table_columns(conn, "conversations_v2")?,
            "Kiro conversations_v2 table",
            &[
                "key",
                "conversation_id",
                "value",
                "created_at",
                "updated_at",
            ],
        )?;
    }
    let legacy = sqlite_table_exists(conn, "conversations")?;
    if legacy {
        ensure_sqlite_table_columns(
            &sqlite_table_columns(conn, "conversations")?,
            "Kiro conversations table",
            &["key", "value"],
        )?;
    }
    if !v2 && !legacy {
        return Err(CaptureError::InvalidPayload(
            "Kiro SQLite database is missing required conversations_v2 or conversations table"
                .into(),
        ));
    }
    Ok(KiroConversationTables { v2, legacy })
}

#[cfg(test)]
#[path = "sqlite_tests.rs"]
mod tests;
