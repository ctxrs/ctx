use std::{collections::BTreeSet, path::Path};

use rusqlite::{Connection, OptionalExtension, Statement};

use crate::captured_batch::sqlite_logical_rows::{SqliteLogicalRow, SqliteLogicalRowsBatchError};
use crate::captured_batch::{
    CapturedSqliteValue, NativeLocator, NativePosition, ProviderRecordKind,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, optional_column_expr, sqlite_table_columns, sqlite_table_exists,
    ProviderSqliteSourceSnapshot, SqliteLengthPreflightGuard,
};
use crate::{CaptureError, Result};

use super::{
    FORGECODE_CAPTURE_REVISION, FORGECODE_LOCATOR_KIND, FORGECODE_POLICY_REVISION,
    FORGECODE_POSITION_BYTES, FORGECODE_POSITION_KIND, FORGECODE_REJECTED_RECORD_KIND,
    FORGECODE_SQLITE_VALUE_OVERHEAD_BYTES,
};

pub(super) fn forgecode_source_snapshot(path: &Path) -> Result<ProviderSqliteSourceSnapshot> {
    ProviderSqliteSourceSnapshot::read(
        path,
        "ForgeCode SQLite source must be a regular non-symlink file",
        "ForgeCode SQLite sidecar must be a regular non-symlink file",
    )
}

pub(super) fn forgecode_source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    schema_fingerprint: &str,
) -> String {
    format!(
        "forgecode-sqlite-snapshot-v1:capture={FORGECODE_CAPTURE_REVISION};policy={FORGECODE_POLICY_REVISION};schema={schema_fingerprint};{}",
        snapshot.revision_component(),
    )
}

pub(super) struct ForgeCodeConversationRow {
    pub(super) rowid: i64,
    pub(super) conversation_id: String,
    pub(super) title: Option<String>,
    pub(super) workspace_id: i64,
    pub(super) context: Option<String>,
    pub(super) created_at: String,
    pub(super) updated_at: Option<String>,
    pub(super) metrics: Option<String>,
}

pub(super) struct ForgeCodeRowFetcher<'connection> {
    conn: &'connection Connection,
    initial_candidate: Statement<'connection>,
    next_candidate: Statement<'connection>,
    hydration: Statement<'connection>,
    record_kind: ProviderRecordKind,
    rejected_record_kind: ProviderRecordKind,
}

impl<'connection> ForgeCodeRowFetcher<'connection> {
    pub(super) fn new(
        conn: &'connection Connection,
        columns: &BTreeSet<String>,
        record_kind: ProviderRecordKind,
    ) -> Result<Self> {
        let title = optional_column_expr(columns, "title", "NULL");
        let context = optional_column_expr(columns, "context", "NULL");
        let updated_at = optional_column_expr(columns, "updated_at", "NULL");
        let metrics = optional_column_expr(columns, "metrics", "NULL");
        let retained_bytes = forgecode_retained_length_expr(&[
            "conversation_id".to_owned(),
            title.to_owned(),
            "CASE WHEN typeof(workspace_id) = 'integer' THEN NULL ELSE workspace_id END".to_owned(),
            context.to_owned(),
            "created_at".to_owned(),
            updated_at.to_owned(),
            metrics.to_owned(),
        ]);
        Ok(Self {
            conn,
            initial_candidate: conn.prepare(&forgecode_candidate_sql(
                &retained_bytes,
                title,
                context,
                updated_at,
                metrics,
                false,
            ))?,
            next_candidate: conn.prepare(&forgecode_candidate_sql(
                &retained_bytes,
                title,
                context,
                updated_at,
                metrics,
                true,
            ))?,
            hydration: conn.prepare(&format!(
                "select rowid, CAST(conversation_id AS BLOB), CAST({title} AS BLOB), \
                        workspace_id, CAST({context} AS BLOB), CAST(created_at AS BLOB), \
                        CAST({updated_at} AS BLOB), CAST({metrics} AS BLOB) \
                 from conversations where rowid = ?1"
            ))?,
            record_kind,
            rejected_record_kind: ProviderRecordKind::new(FORGECODE_REJECTED_RECORD_KIND)
                .map_err(forgecode_captured_error)?,
        })
    }

    pub(super) fn fetch(&mut self, after: NativePosition) -> Result<Option<SqliteLogicalRow>> {
        let keyset = decode_forgecode_position(&after)?;
        let ordinal = keyset.map_or(0_u64, |keyset| keyset.next_ordinal);
        let candidate = with_forgecode_length_preflight(self.conn, || match keyset {
            None => self
                .initial_candidate
                .query_row([], forgecode_row_candidate)
                .optional(),
            Some(keyset) => self
                .next_candidate
                .query_row([keyset.rowid], forgecode_row_candidate)
                .optional(),
        })?;
        let Some(candidate) = candidate else {
            return Ok(None);
        };
        let next_position = encode_forgecode_position(ForgeCodeKeyset {
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "ForgeCode captured row ordinal overflowed",
            ))?,
            rowid: candidate.rowid,
        })?;
        let locator = NativeLocator::new(
            FORGECODE_LOCATOR_KIND,
            candidate.rowid.to_be_bytes().to_vec(),
        )
        .map_err(forgecode_captured_error)?;
        let observed_bytes = candidate.observed_bytes()?;
        if observed_bytes > forgecode_oversize_limit()? {
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                locator,
                self.record_kind.clone(),
                observed_bytes,
            )
            .map(Some)
            .map_err(forgecode_captured_error);
        }
        if let Some(reason) = candidate.rejection_reason() {
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
            .map(Some)
            .map_err(forgecode_captured_error);
        }
        let hydrated = self.hydration.query_row([candidate.rowid], |row| {
            Ok(ForgeCodeHydratedRow {
                rowid: row.get(0)?,
                conversation_id: row.get(1)?,
                title: row.get(2)?,
                workspace_id: row.get(3)?,
                context: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
                metrics: row.get(7)?,
            })
        })?;
        let values = match hydrated.captured_values() {
            Ok(values) => values,
            Err(reason) => {
                return SqliteLogicalRow::values(
                    next_position,
                    ordinal,
                    locator,
                    self.rejected_record_kind.clone(),
                    vec![
                        CapturedSqliteValue::Integer(candidate.rowid),
                        CapturedSqliteValue::Text(reason),
                    ],
                )
                .map(Some)
                .map_err(forgecode_captured_error);
            }
        };
        SqliteLogicalRow::values(
            next_position,
            ordinal,
            locator,
            self.record_kind.clone(),
            values,
        )
        .map(Some)
        .map_err(forgecode_captured_error)
    }
}

pub(super) fn forgecode_candidate_sql(
    retained_bytes: &str,
    title: &str,
    context: &str,
    updated_at: &str,
    metrics: &str,
    resumed: bool,
) -> String {
    let resume_predicate = if resumed { "where rowid > ?1 " } else { "" };
    format!(
        "select rowid, {retained_bytes}, \
                typeof(conversation_id), typeof({title}), typeof(workspace_id), \
                typeof({context}), typeof(created_at), typeof({updated_at}), \
                typeof({metrics}) \
         from conversations {resume_predicate}order by rowid limit 1"
    )
}

fn forgecode_row_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<ForgeCodeRowCandidate> {
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

fn with_forgecode_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    // SQLITE_LIMIT_LENGTH rejects even an integer-only octet_length inspection
    // of an oversized stored value. Candidate SQL returns only rowid, storage
    // classes, and byte counts, so lift the limit only for that metadata-only
    // preflight and restore it before any cast, hydration, or JSON projection.
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

pub(super) struct ForgeCodeRowCandidate {
    pub(super) rowid: i64,
    pub(super) retained_bytes: i64,
    pub(super) storage_classes: [String; 7],
}

impl ForgeCodeRowCandidate {
    pub(super) fn observed_bytes(&self) -> Result<u64> {
        let payload = u64::try_from(self.retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload(
                "ForgeCode SQLite retained byte count must be nonnegative".to_owned(),
            )
        })?;
        FORGECODE_SQLITE_VALUE_OVERHEAD_BYTES
            .checked_add(payload)
            .ok_or(CaptureError::SystemInvariant(
                "ForgeCode SQLite retained byte count overflowed",
            ))
    }

    fn rejection_reason(&self) -> Option<&'static str> {
        let [conversation_id, title, workspace_id, context, created_at, updated_at, metrics] =
            self.storage_classes.each_ref();
        let castable_required_text =
            |storage_class: &str| matches!(storage_class, "integer" | "real" | "text");
        let castable_optional_text =
            |storage_class: &str| storage_class == "null" || castable_required_text(storage_class);
        let optional_text = |storage_class: &str| matches!(storage_class, "null" | "text");
        if !castable_required_text(conversation_id) {
            Some("ForgeCode conversations.conversation_id has an unsupported SQLite storage class")
        } else if !optional_text(title) {
            Some("ForgeCode conversations.title has an unsupported SQLite storage class")
        } else if workspace_id.as_str() != "integer" {
            Some("ForgeCode conversations.workspace_id has an unsupported SQLite storage class")
        } else if !optional_text(context) {
            Some("ForgeCode conversations.context has an unsupported SQLite storage class")
        } else if !castable_required_text(created_at) {
            Some("ForgeCode conversations.created_at has an unsupported SQLite storage class")
        } else if !castable_optional_text(updated_at) {
            Some("ForgeCode conversations.updated_at has an unsupported SQLite storage class")
        } else if !optional_text(metrics) {
            Some("ForgeCode conversations.metrics has an unsupported SQLite storage class")
        } else {
            None
        }
    }
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

impl ForgeCodeHydratedRow {
    fn captured_values(self) -> std::result::Result<Vec<CapturedSqliteValue>, String> {
        Ok(vec![
            CapturedSqliteValue::Integer(self.rowid),
            forgecode_captured_text(self.conversation_id, "conversation_id")?,
            forgecode_captured_optional_bytes(self.title, "title")?,
            CapturedSqliteValue::Integer(self.workspace_id),
            forgecode_captured_optional_bytes(self.context, "context")?,
            forgecode_captured_text(self.created_at, "created_at")?,
            forgecode_captured_optional_bytes(self.updated_at, "updated_at")?,
            forgecode_captured_optional_bytes(self.metrics, "metrics")?,
        ])
    }
}

fn forgecode_captured_text(
    value: Vec<u8>,
    field: &'static str,
) -> std::result::Result<CapturedSqliteValue, String> {
    String::from_utf8(value)
        .map(CapturedSqliteValue::Text)
        .map_err(|_| format!("ForgeCode conversations.{field} is not valid UTF-8"))
}

fn forgecode_captured_optional_bytes(
    value: Option<Vec<u8>>,
    field: &'static str,
) -> std::result::Result<CapturedSqliteValue, String> {
    value.map_or(Ok(CapturedSqliteValue::Null), |value| {
        forgecode_captured_text(value, field)
    })
}

#[derive(Clone, Copy)]
pub(super) struct ForgeCodeKeyset {
    pub(super) next_ordinal: u64,
    pub(super) rowid: i64,
}

pub(super) fn initial_forgecode_position() -> Result<NativePosition> {
    NativePosition::new(FORGECODE_POSITION_KIND, vec![0]).map_err(forgecode_captured_error)
}

pub(super) fn encode_forgecode_position(keyset: ForgeCodeKeyset) -> Result<NativePosition> {
    let mut value = Vec::with_capacity(FORGECODE_POSITION_BYTES);
    value.push(1);
    value.extend_from_slice(&keyset.next_ordinal.to_be_bytes());
    value.extend_from_slice(&forgecode_ordered_i64(keyset.rowid).to_be_bytes());
    NativePosition::new(FORGECODE_POSITION_KIND, value).map_err(forgecode_captured_error)
}

pub(super) fn decode_forgecode_position(
    position: &NativePosition,
) -> Result<Option<ForgeCodeKeyset>> {
    if position.kind() != FORGECODE_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "ForgeCode cursor has an unexpected native-position kind".to_owned(),
        ));
    }
    if position.value() == [0] {
        return Ok(None);
    }
    if position.value().len() != FORGECODE_POSITION_BYTES || position.value()[0] != 1 {
        return Err(CaptureError::InvalidPayload(
            "ForgeCode cursor has an invalid native-position payload".to_owned(),
        ));
    }
    Ok(Some(ForgeCodeKeyset {
        next_ordinal: forgecode_decode_u64(&position.value()[1..9])?,
        rowid: forgecode_unordered_i64(forgecode_decode_u64(&position.value()[9..17])?),
    }))
}

fn forgecode_decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload("ForgeCode cursor integer has an invalid width".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

fn forgecode_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

fn forgecode_unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}

pub(super) fn forgecode_retained_length_expr(expressions: &[String]) -> String {
    // Keep probes on the stored columns. Unlike CAST-to-TEXT or CAST-to-BLOB,
    // octet_length returns integer byte metadata without materializing large
    // TEXT/BLOB values through the temporarily raised SQLite length limit.
    expressions
        .iter()
        .map(|expression| format!("coalesce(octet_length({expression}), 0)"))
        .collect::<Vec<_>>()
        .join(" + ")
}

pub(super) fn forgecode_oversize_limit() -> Result<u64> {
    u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("ForgeCode byte limit exceeds u64"))
}

pub(super) fn forgecode_captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(super) fn forgecode_sqlite_batch_error(
    error: SqliteLogicalRowsBatchError<CaptureError>,
) -> CaptureError {
    match error {
        SqliteLogicalRowsBatchError::Callback(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

pub(super) fn decode_forgecode_conversation(
    values: &[CapturedSqliteValue],
) -> Result<ForgeCodeConversationRow> {
    let [CapturedSqliteValue::Integer(rowid), CapturedSqliteValue::Text(conversation_id), title, CapturedSqliteValue::Integer(workspace_id), context, CapturedSqliteValue::Text(created_at), updated_at, metrics] =
        values
    else {
        return Err(CaptureError::SystemInvariant(
            "ForgeCode logical row has an invalid value shape",
        ));
    };
    Ok(ForgeCodeConversationRow {
        rowid: *rowid,
        conversation_id: conversation_id.clone(),
        title: forgecode_optional_text(title)?,
        workspace_id: *workspace_id,
        context: forgecode_optional_text(context)?,
        created_at: created_at.clone(),
        updated_at: forgecode_optional_text(updated_at)?,
        metrics: forgecode_optional_text(metrics)?,
    })
}

fn forgecode_optional_text(value: &CapturedSqliteValue) -> Result<Option<String>> {
    match value {
        CapturedSqliteValue::Null => Ok(None),
        CapturedSqliteValue::Text(value) => Ok(Some(value.clone())),
        _ => Err(CaptureError::SystemInvariant(
            "ForgeCode logical row has an invalid optional text value",
        )),
    }
}

pub(super) fn forgecode_conversation_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, "conversations")? {
        return Err(CaptureError::InvalidPayload(
            "ForgeCode .forge.db is missing required conversations table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "conversations")?;
    ensure_sqlite_table_columns(
        &columns,
        "ForgeCode conversations table",
        &["conversation_id", "workspace_id", "created_at"],
    )?;
    Ok(columns)
}
