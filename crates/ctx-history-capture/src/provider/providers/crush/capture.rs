use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, Statement};

use crate::captured_batch::sqlite_logical_rows::{SqliteLogicalRow, SqliteLogicalRowsBatchError};
use crate::captured_batch::{
    CapturedSqliteValue, NativeLocator, NativePosition, ProviderRecordKind,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::provider::sqlite::SqliteLengthPreflightGuard;
use crate::{CaptureError, Result};

use super::source::{
    file_projection, message_projection, optional_session_column, read_file_projection,
    retained_length_expr, session_projection,
};
use super::{
    CRUSH_FILE_RECORD_KIND, CRUSH_MESSAGE_CHILD_RECORD_KIND, CRUSH_READ_FILE_RECORD_KIND,
    CRUSH_SESSION_RECORD_KIND,
};

const CRUSH_POSITION_KIND: &str = "crush-sqlite-keyset-v1";
pub(crate) const CRUSH_LOCATOR_KIND: &str = "crush-sqlite-row-v1";
const CRUSH_POSITION_BYTES: usize = 1 + 8 + 8;
pub(super) const CRUSH_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 64 * 13;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CrushPhase {
    Sessions,
    Messages,
    Files,
    ReadFiles,
}

impl CrushPhase {
    fn tag(self) -> u8 {
        match self {
            Self::Sessions => 1,
            Self::Messages => 2,
            Self::Files => 3,
            Self::ReadFiles => 4,
        }
    }

    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(Self::Sessions),
            2 => Ok(Self::Messages),
            3 => Ok(Self::Files),
            4 => Ok(Self::ReadFiles),
            _ => Err(CaptureError::InvalidPayload(
                "Crush cursor has an unknown phase".to_owned(),
            )),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct CrushKeyset {
    pub(super) phase: CrushPhase,
    pub(super) next_ordinal: u64,
    pub(super) rowid: i64,
}

pub(super) struct CrushRowFetcher<'connection> {
    conn: &'connection Connection,
    pub(super) session_candidates: CrushCandidateStatements<'connection>,
    session_hydration: Statement<'connection>,
    pub(super) message_candidates: CrushCandidateStatements<'connection>,
    message_hydration: Statement<'connection>,
    pub(super) file_candidates: Option<CrushCandidateStatements<'connection>>,
    file_hydration: Option<Statement<'connection>>,
    pub(super) read_file_candidates: Option<CrushCandidateStatements<'connection>>,
    read_file_hydration: Option<Statement<'connection>>,
    session_record_kind: ProviderRecordKind,
    message_child_record_kind: ProviderRecordKind,
    file_record_kind: ProviderRecordKind,
    read_file_record_kind: ProviderRecordKind,
    #[cfg(test)]
    session_hydration_queries: usize,
}

pub(super) struct CrushCandidateStatements<'connection> {
    first: Statement<'connection>,
    next: Statement<'connection>,
}

impl<'connection> CrushCandidateStatements<'connection> {
    fn prepare(
        conn: &'connection Connection,
        rowid: &str,
        metadata: &str,
        source: &str,
    ) -> rusqlite::Result<Self> {
        Ok(Self {
            first: conn.prepare(&candidate_sql(rowid, metadata, source, false))?,
            next: conn.prepare(&candidate_sql(rowid, metadata, source, true))?,
        })
    }
}

impl<'connection> CrushRowFetcher<'connection> {
    pub(super) fn new(
        conn: &'connection Connection,
        session_columns: &BTreeSet<String>,
        message_columns: &BTreeSet<String>,
        file_columns: Option<&BTreeSet<String>>,
        read_file_columns: Option<&BTreeSet<String>>,
    ) -> Result<Self> {
        let session_projection = session_projection(session_columns, "s");
        let session_bytes = retained_length_expr(
            session_columns,
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
        );
        let message_projection = message_projection(message_columns, "m");
        let message_local_bytes = retained_length_expr(
            message_columns,
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
        let message_parent_bytes =
            retained_length_expr(session_columns, "s", &["created_at", "updated_at"]);
        let message_bytes = format!("{message_local_bytes} + {message_parent_bytes}");
        let parent_created_at = optional_session_column(session_columns, "created_at");
        let parent_updated_at = optional_session_column(session_columns, "updated_at");
        let file_statements = file_columns
            .filter(|columns| columns.contains("session_id"))
            .map(|columns| {
                let projection = file_projection(columns, "f");
                let bytes = retained_length_expr(
                    columns,
                    "f",
                    &["session_id", "path", "version", "created_at", "updated_at"],
                );
                Ok::<_, CaptureError>((
                    CrushCandidateStatements::prepare(
                        conn,
                        "f.rowid",
                        &bytes,
                        "files f join sessions s on s.id = f.session_id",
                    )?,
                    conn.prepare(&format!(
                        "select {projection} from files f join sessions s on s.id = f.session_id \
                         where f.rowid = ?1"
                    ))?,
                ))
            })
            .transpose()?;
        let read_file_statements = read_file_columns
            .map(|columns| {
                let projection = read_file_projection(columns, "r");
                let bytes = retained_length_expr(columns, "r", &["session_id", "path", "read_at"]);
                Ok::<_, CaptureError>((
                    CrushCandidateStatements::prepare(
                        conn,
                        "r.rowid",
                        &bytes,
                        "read_files r join sessions s on s.id = r.session_id",
                    )?,
                    conn.prepare(&format!(
                        "select {projection} from read_files r \
                         join sessions s on s.id = r.session_id where r.rowid = ?1"
                    ))?,
                ))
            })
            .transpose()?;
        let (file_candidates, file_hydration) = file_statements.unzip();
        let (read_file_candidates, read_file_hydration) = read_file_statements.unzip();
        Ok(Self {
            conn,
            session_candidates: CrushCandidateStatements::prepare(
                conn,
                "s.rowid",
                &session_bytes,
                "sessions s",
            )?,
            session_hydration: conn.prepare(&format!(
                "select s.rowid, {session_projection} from sessions s where s.rowid = ?1"
            ))?,
            message_candidates: CrushCandidateStatements::prepare(
                conn,
                "m.rowid",
                &message_bytes,
                "messages m left join sessions s on s.id = m.session_id",
            )?,
            message_hydration: conn.prepare(&format!(
                "select s.rowid, cast({parent_created_at} as integer), \
                 cast({parent_updated_at} as integer), {message_projection} \
                 from messages m left join sessions s on s.id = m.session_id \
                 where m.rowid = ?1"
            ))?,
            file_candidates,
            file_hydration,
            read_file_candidates,
            read_file_hydration,
            session_record_kind: ProviderRecordKind::new(CRUSH_SESSION_RECORD_KIND)
                .map_err(captured_error)?,
            message_child_record_kind: ProviderRecordKind::new(CRUSH_MESSAGE_CHILD_RECORD_KIND)
                .map_err(captured_error)?,
            file_record_kind: ProviderRecordKind::new(CRUSH_FILE_RECORD_KIND)
                .map_err(captured_error)?,
            read_file_record_kind: ProviderRecordKind::new(CRUSH_READ_FILE_RECORD_KIND)
                .map_err(captured_error)?,
            #[cfg(test)]
            session_hydration_queries: 0,
        })
    }

    pub(super) fn fetch(&mut self, after: NativePosition) -> Result<Option<SqliteLogicalRow>> {
        let keyset = decode_position(&after)?;
        let (phase, after_rowid, ordinal) = match keyset {
            Some(keyset) => (keyset.phase, Some(keyset.rowid), keyset.next_ordinal),
            None => (CrushPhase::Sessions, None, 0),
        };
        match phase {
            CrushPhase::Sessions => {
                if let Some(candidate) =
                    fetch_candidate(self.conn, &mut self.session_candidates, after_rowid)?
                {
                    return self.hydrate(candidate, phase, ordinal).map(Some);
                }
                self.fetch_first_after_phase(CrushPhase::Messages, ordinal)
            }
            CrushPhase::Messages => self.fetch_message(after_rowid, ordinal),
            CrushPhase::Files => {
                if let Some(candidate) =
                    fetch_optional_candidate(self.conn, &mut self.file_candidates, after_rowid)?
                {
                    return self.hydrate(candidate, phase, ordinal).map(Some);
                }
                self.fetch_first_after_phase(CrushPhase::ReadFiles, ordinal)
            }
            CrushPhase::ReadFiles => {
                fetch_optional_candidate(self.conn, &mut self.read_file_candidates, after_rowid)?
                    .map(|candidate| self.hydrate(candidate, phase, ordinal))
                    .transpose()
            }
        }
    }

    fn fetch_first_after_phase(
        &mut self,
        phase: CrushPhase,
        ordinal: u64,
    ) -> Result<Option<SqliteLogicalRow>> {
        match phase {
            CrushPhase::Sessions => Err(CaptureError::SystemInvariant(
                "Crush phase transition cannot return to sessions",
            )),
            CrushPhase::Messages => self.fetch_message(None, ordinal),
            CrushPhase::Files => {
                if let Some(candidate) =
                    fetch_optional_candidate(self.conn, &mut self.file_candidates, None)?
                {
                    return self.hydrate(candidate, phase, ordinal).map(Some);
                }
                self.fetch_first_after_phase(CrushPhase::ReadFiles, ordinal)
            }
            CrushPhase::ReadFiles => {
                fetch_optional_candidate(self.conn, &mut self.read_file_candidates, None)?
                    .map(|candidate| self.hydrate(candidate, phase, ordinal))
                    .transpose()
            }
        }
    }

    fn fetch_message(
        &mut self,
        after_rowid: Option<i64>,
        ordinal: u64,
    ) -> Result<Option<SqliteLogicalRow>> {
        let Some(candidate) =
            fetch_candidate(self.conn, &mut self.message_candidates, after_rowid)?
        else {
            return self.fetch_first_after_phase(CrushPhase::Files, ordinal);
        };
        self.hydrate_message(candidate, ordinal).map(Some)
    }

    fn hydrate(
        &mut self,
        candidate: CrushCandidate,
        phase: CrushPhase,
        ordinal: u64,
    ) -> Result<SqliteLogicalRow> {
        let next_position = encode_position(CrushKeyset {
            phase,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Crush captured row ordinal overflowed",
            ))?,
            rowid: candidate.rowid,
        })?;
        let locator = locator(phase, candidate.rowid)?;
        let observed_bytes = candidate.observed_bytes()?;
        let record_kind = self.record_kind(phase).clone();
        if observed_bytes > oversize_limit()? {
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                locator,
                record_kind,
                observed_bytes,
            )
            .map_err(captured_error);
        }
        #[cfg(test)]
        if phase == CrushPhase::Sessions {
            self.session_hydration_queries += 1;
        }
        let values = match phase {
            CrushPhase::Sessions => self
                .session_hydration
                .query_row([candidate.rowid], session_values)?,
            CrushPhase::Messages => {
                return Err(CaptureError::SystemInvariant(
                    "Crush generic hydration cannot hydrate a message candidate",
                ));
            }
            CrushPhase::Files => self
                .file_hydration
                .as_mut()
                .ok_or(CaptureError::SystemInvariant(
                    "Crush file hydration statement is unavailable",
                ))?
                .query_row([candidate.rowid], file_values)?,
            CrushPhase::ReadFiles => self
                .read_file_hydration
                .as_mut()
                .ok_or(CaptureError::SystemInvariant(
                    "Crush read-file hydration statement is unavailable",
                ))?
                .query_row([candidate.rowid], read_file_values)?,
        };
        SqliteLogicalRow::values(next_position, ordinal, locator, record_kind, values)
            .map_err(captured_error)
    }

    #[cfg(test)]
    pub(super) fn session_hydration_query_count(&self) -> usize {
        self.session_hydration_queries
    }

    fn hydrate_message(
        &mut self,
        candidate: CrushCandidate,
        ordinal: u64,
    ) -> Result<SqliteLogicalRow> {
        let next_position = encode_position(CrushKeyset {
            phase: CrushPhase::Messages,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Crush captured row ordinal overflowed",
            ))?,
            rowid: candidate.rowid,
        })?;
        let locator = locator(CrushPhase::Messages, candidate.rowid)?;
        if candidate.observed_bytes()? > oversize_limit()? {
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                locator,
                self.message_child_record_kind.clone(),
                candidate.observed_bytes()?,
            )
            .map_err(captured_error);
        }
        let values = self
            .message_hydration
            .query_row([candidate.rowid], message_child_values)?;
        SqliteLogicalRow::values(
            next_position,
            ordinal,
            locator,
            self.message_child_record_kind.clone(),
            values,
        )
        .map_err(captured_error)
    }

    fn record_kind(&self, phase: CrushPhase) -> &ProviderRecordKind {
        match phase {
            CrushPhase::Sessions => &self.session_record_kind,
            CrushPhase::Messages => &self.message_child_record_kind,
            CrushPhase::Files => &self.file_record_kind,
            CrushPhase::ReadFiles => &self.read_file_record_kind,
        }
    }
}

pub(super) struct CrushCandidate {
    pub(super) rowid: i64,
    pub(super) retained_bytes: i64,
}

impl CrushCandidate {
    pub(super) fn observed_bytes(&self) -> Result<u64> {
        let payload = u64::try_from(self.retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload(
                "Crush SQLite retained byte count must be nonnegative".to_owned(),
            )
        })?;
        CRUSH_SQLITE_VALUE_OVERHEAD_BYTES
            .checked_add(payload)
            .ok_or(CaptureError::SystemInvariant(
                "Crush SQLite retained byte count overflowed",
            ))
    }
}

pub(super) fn fetch_candidate(
    conn: &Connection,
    statements: &mut CrushCandidateStatements<'_>,
    after_rowid: Option<i64>,
) -> Result<Option<CrushCandidate>> {
    with_length_preflight(conn, || {
        let read_candidate = |row: &rusqlite::Row<'_>| {
            Ok(CrushCandidate {
                rowid: row.get(0)?,
                retained_bytes: row.get(1)?,
            })
        };
        match after_rowid {
            Some(after_rowid) => statements
                .next
                .query_row([after_rowid], read_candidate)
                .optional(),
            None => statements.first.query_row([], read_candidate).optional(),
        }
    })
}

pub(super) fn fetch_optional_candidate(
    conn: &Connection,
    statements: &mut Option<CrushCandidateStatements<'_>>,
    after_rowid: Option<i64>,
) -> Result<Option<CrushCandidate>> {
    let Some(statements) = statements.as_mut() else {
        return Ok(None);
    };
    fetch_candidate(conn, statements, after_rowid)
}

pub(super) fn candidate_sql(rowid: &str, metadata: &str, source: &str, next: bool) -> String {
    let after = if next {
        format!(" where {rowid} > ?1")
    } else {
        String::new()
    };
    format!("select {rowid}, {metadata} from {source}{after} order by {rowid} limit 1")
}

pub(super) fn with_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    // SQLITE_LIMIT_LENGTH rejects even integer-only storage-class/octet_length
    // inspection of an oversized stored value. Candidate statements return only
    // rowids and numeric metadata, so lift the limit only for that preflight. The
    // guard restores the provider cap before any raw TEXT/BLOB hydration executes.
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

fn session_values(row: &rusqlite::Row<'_>) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    let mut values = vec![CapturedSqliteValue::Integer(row.get(0)?)];
    values.extend(session_values_at(row, 1)?);
    Ok(values)
}

fn session_values_at(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    Ok(vec![
        optional_text_value(row.get(offset)?),
        optional_text_value(row.get(offset + 1)?),
        optional_text_value(row.get(offset + 2)?),
        optional_integer_value(row.get(offset + 3)?),
        optional_integer_value(row.get(offset + 4)?),
        optional_integer_value(row.get(offset + 5)?),
        optional_integer_value(row.get(offset + 6)?),
        optional_real_value(row.get(offset + 7)?),
        optional_text_value(row.get(offset + 8)?),
    ])
}

pub(crate) fn message_child_values(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    let mut values = vec![
        optional_integer_value(row.get(0)?),
        optional_integer_value(row.get(1)?),
        optional_integer_value(row.get(2)?),
    ];
    values.extend(message_values_at(row, 3)?);
    Ok(values)
}

pub(super) fn crush_message_values_at_rowid(
    conn: &Connection,
    rowid: i64,
) -> Result<Option<Vec<CapturedSqliteValue>>> {
    let session_columns = super::source::session_columns(conn)?;
    let message_columns = super::source::message_columns(conn)?;
    let parent_created_at = optional_session_column(&session_columns, "created_at");
    let parent_updated_at = optional_session_column(&session_columns, "updated_at");
    let message_projection = message_projection(&message_columns, "m");
    let sql = format!(
        "select s.rowid, cast({parent_created_at} as integer), \
                cast({parent_updated_at} as integer), {message_projection} \
         from messages m left join sessions s on s.id = m.session_id where m.rowid = ?1"
    );
    conn.query_row(&sql, [rowid], message_child_values)
        .optional()
        .map_err(CaptureError::from)
}

fn message_values_at(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    Ok(vec![
        CapturedSqliteValue::Integer(row.get(offset)?),
        CapturedSqliteValue::Text(row.get(offset + 1)?),
        CapturedSqliteValue::Text(row.get(offset + 2)?),
        CapturedSqliteValue::Text(row.get(offset + 3)?),
        CapturedSqliteValue::Text(row.get(offset + 4)?),
        optional_integer_value(row.get(offset + 5)?),
        optional_integer_value(row.get(offset + 6)?),
        optional_text_value(row.get(offset + 7)?),
        optional_text_value(row.get(offset + 8)?),
        CapturedSqliteValue::Integer(row.get::<_, Option<i64>>(offset + 9)?.unwrap_or(0)),
    ])
}

fn file_values(row: &rusqlite::Row<'_>) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    Ok(vec![
        CapturedSqliteValue::Integer(row.get(0)?),
        CapturedSqliteValue::Text(row.get(1)?),
        CapturedSqliteValue::Text(row.get(2)?),
        optional_text_value(row.get(3)?),
        optional_integer_value(row.get(4)?),
        optional_integer_value(row.get(5)?),
    ])
}

fn read_file_values(row: &rusqlite::Row<'_>) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    Ok(vec![
        CapturedSqliteValue::Integer(row.get(0)?),
        CapturedSqliteValue::Text(row.get(1)?),
        CapturedSqliteValue::Text(row.get(2)?),
        optional_integer_value(row.get(3)?),
    ])
}

fn optional_text_value(value: Option<String>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Text)
}

fn optional_integer_value(value: Option<i64>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Integer)
}

fn optional_real_value(value: Option<f64>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::from_real)
}

pub(super) fn initial_position() -> Result<NativePosition> {
    NativePosition::new(CRUSH_POSITION_KIND, vec![0]).map_err(captured_error)
}

pub(super) fn encode_position(keyset: CrushKeyset) -> Result<NativePosition> {
    let mut value = Vec::with_capacity(CRUSH_POSITION_BYTES);
    value.push(keyset.phase.tag());
    value.extend_from_slice(&keyset.next_ordinal.to_be_bytes());
    value.extend_from_slice(&ordered_i64(keyset.rowid).to_be_bytes());
    NativePosition::new(CRUSH_POSITION_KIND, value).map_err(captured_error)
}

pub(super) fn decode_position(position: &NativePosition) -> Result<Option<CrushKeyset>> {
    if position.kind() != CRUSH_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "Crush cursor has an unexpected native-position kind".to_owned(),
        ));
    }
    if position.value() == [0] {
        return Ok(None);
    }
    if position.value().len() != CRUSH_POSITION_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Crush cursor has an invalid native-position payload".to_owned(),
        ));
    }
    Ok(Some(CrushKeyset {
        phase: CrushPhase::from_tag(position.value()[0])?,
        next_ordinal: decode_u64(&position.value()[1..9])?,
        rowid: unordered_i64(decode_u64(&position.value()[9..17])?),
    }))
}

fn locator(phase: CrushPhase, rowid: i64) -> Result<NativeLocator> {
    let mut value = Vec::with_capacity(1 + 8);
    value.push(phase.tag());
    value.extend_from_slice(&ordered_i64(rowid).to_be_bytes());
    NativeLocator::new(CRUSH_LOCATOR_KIND, value).map_err(captured_error)
}

fn decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload("Crush cursor integer has an invalid width".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

fn ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

fn unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}

pub(super) fn oversize_limit() -> Result<u64> {
    u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("Crush byte limit exceeds u64"))
}

pub(super) fn captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(super) fn sqlite_batch_error(error: SqliteLogicalRowsBatchError<CaptureError>) -> CaptureError {
    match error {
        SqliteLogicalRowsBatchError::Callback(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}
