use rusqlite::{Connection, OptionalExtension, Statement};

use crate::captured_batch::sqlite_logical_rows::{SqliteLogicalRow, SqliteLogicalRowsBatchError};
use crate::captured_batch::{
    CapturedSqliteValue, NativePosition, ProviderRecordKind,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::provider::sqlite::SqliteLengthPreflightGuard;
use crate::{CaptureError, Result};

use super::position::{
    decode_goose_position, encode_goose_position, goose_locator, GooseCapturePhase, GooseKeyset,
};
use super::schema::{
    goose_message_columns, goose_message_expressions, goose_message_only_values,
    goose_message_values_at, goose_session_columns, goose_session_expressions,
    goose_session_values, GOOSE_MESSAGE_RECORD_KIND, GOOSE_SESSION_RECORD_KIND,
};

const GOOSE_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 64 * 32;

#[derive(Clone, Copy)]
struct GooseRowCandidate {
    rowid: i64,
    retained_bytes: i64,
}

impl GooseRowCandidate {
    fn observed_bytes(&self) -> Result<u64> {
        goose_observed_bytes(self.retained_bytes)
    }
}

#[derive(Clone, Copy)]
struct GooseMessageCandidate {
    rowid: i64,
    retained_bytes: i64,
    parent_rowid: Option<i64>,
}

impl GooseMessageCandidate {
    fn observed_bytes(&self) -> Result<u64> {
        goose_observed_bytes(self.retained_bytes)
    }
}

pub(super) struct GooseRowFetcher<'connection> {
    conn: &'connection Connection,
    first_message_candidate: Statement<'connection>,
    next_message_candidate: Statement<'connection>,
    message_hydration: Statement<'connection>,
    first_session_candidate: Statement<'connection>,
    next_session_candidate: Statement<'connection>,
    session_hydration: Statement<'connection>,
    message_record_kind: ProviderRecordKind,
    session_record_kind: ProviderRecordKind,
    #[cfg(test)]
    pub(super) session_hydration_queries: usize,
}

impl<'connection> GooseRowFetcher<'connection> {
    pub(super) fn new(conn: &'connection Connection) -> Result<Self> {
        let session_columns = goose_session_columns(conn)?;
        let message_columns = goose_message_columns(conn)?;
        let session_expressions = goose_session_expressions(&session_columns, "s");
        let message_expressions = goose_message_expressions(&message_columns, "m");
        let message_lengths = goose_retained_length_expr(&message_expressions.retained);
        let session_lengths = goose_retained_length_expr(&session_expressions.retained);
        let message_select = message_expressions.hydration.join(", ");
        let session_select = session_expressions.hydration.join(", ");

        Ok(Self {
            conn,
            first_message_candidate: conn
                .prepare(&goose_message_candidate_sql(&message_lengths, false))?,
            next_message_candidate: conn
                .prepare(&goose_message_candidate_sql(&message_lengths, true))?,
            message_hydration: conn.prepare(&format!(
                "select {message_select} from messages m where m.rowid = ?1"
            ))?,
            first_session_candidate: conn.prepare(&goose_rowid_candidate_sql(
                "sessions",
                "s",
                &session_lengths,
                false,
            ))?,
            next_session_candidate: conn.prepare(&goose_rowid_candidate_sql(
                "sessions",
                "s",
                &session_lengths,
                true,
            ))?,
            session_hydration: conn.prepare(&format!(
                "select {session_select} from sessions s where s.rowid = ?1"
            ))?,
            message_record_kind: ProviderRecordKind::new(GOOSE_MESSAGE_RECORD_KIND)
                .map_err(goose_captured_error)?,
            session_record_kind: ProviderRecordKind::new(GOOSE_SESSION_RECORD_KIND)
                .map_err(goose_captured_error)?,
            #[cfg(test)]
            session_hydration_queries: 0,
        })
    }

    pub(super) fn fetch(&mut self, after: NativePosition) -> Result<Option<SqliteLogicalRow>> {
        let keyset = decode_goose_position(&after)?;
        let ordinal = keyset.map_or(0, |value| value.next_ordinal);
        match keyset.map(|value| value.phase) {
            None | Some(GooseCapturePhase::Sessions) => {
                let candidate = match keyset {
                    Some(value) => goose_fetch_next_candidate(
                        self.conn,
                        &mut self.next_session_candidate,
                        value.rowid,
                    )?,
                    None => {
                        goose_fetch_first_candidate(self.conn, &mut self.first_session_candidate)?
                    }
                };
                if let Some(candidate) = candidate {
                    return self.hydrate_session(candidate, ordinal).map(Some);
                }
                goose_fetch_first_message_candidate(self.conn, &mut self.first_message_candidate)?
                    .map_or(Ok(None), |candidate| {
                        self.hydrate_message(candidate, ordinal).map(Some)
                    })
            }
            Some(GooseCapturePhase::Messages) => goose_fetch_next_message_candidate(
                self.conn,
                &mut self.next_message_candidate,
                keyset.map_or(0, |value| value.rowid),
            )?
            .map_or(Ok(None), |candidate| {
                self.hydrate_message(candidate, ordinal).map(Some)
            }),
        }
    }

    fn hydrate_message(
        &mut self,
        candidate: GooseMessageCandidate,
        ordinal: u64,
    ) -> Result<SqliteLogicalRow> {
        let next_position = encode_goose_position(GooseKeyset {
            phase: GooseCapturePhase::Messages,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Goose captured row ordinal overflowed",
            ))?,
            rowid: candidate.rowid,
        })?;
        let locator = goose_locator(GooseCapturePhase::Messages, candidate.rowid)?;
        let observed_bytes = candidate.observed_bytes()?;
        if observed_bytes > goose_oversize_limit()? {
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                locator,
                self.message_record_kind.clone(),
                observed_bytes,
            )
            .map_err(goose_captured_error);
        }
        let mut values = vec![candidate
            .parent_rowid
            .map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Integer)];
        values.extend(
            self.message_hydration
                .query_row([candidate.rowid], goose_message_only_values)?,
        );
        SqliteLogicalRow::values(
            next_position,
            ordinal,
            locator,
            self.message_record_kind.clone(),
            values,
        )
        .map_err(goose_captured_error)
    }

    fn hydrate_session(
        &mut self,
        candidate: GooseRowCandidate,
        ordinal: u64,
    ) -> Result<SqliteLogicalRow> {
        let next_position = encode_goose_position(GooseKeyset {
            phase: GooseCapturePhase::Sessions,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Goose captured row ordinal overflowed",
            ))?,
            rowid: candidate.rowid,
        })?;
        let locator = goose_locator(GooseCapturePhase::Sessions, candidate.rowid)?;
        let observed_bytes = candidate.observed_bytes()?;
        if observed_bytes > goose_oversize_limit()? {
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                locator,
                self.session_record_kind.clone(),
                observed_bytes,
            )
            .map_err(goose_captured_error);
        }
        let values = self
            .session_hydration
            .query_row([candidate.rowid], |row| goose_session_values(row, 0))?;
        #[cfg(test)]
        {
            self.session_hydration_queries = self.session_hydration_queries.saturating_add(1);
        }
        SqliteLogicalRow::values(
            next_position,
            ordinal,
            locator,
            self.session_record_kind.clone(),
            values,
        )
        .map_err(goose_captured_error)
    }
}
fn goose_rowid_candidate_sql(
    table: &str,
    alias: &str,
    retained_lengths: &str,
    after: bool,
) -> String {
    let predicate = if after { "where rowid > ?1" } else { "" };
    format!(
        "select {alias}.rowid, {retained_lengths} from {table} {alias} \
         {predicate} order by {alias}.rowid limit 1"
    )
}

pub(super) fn goose_message_candidate_sql(retained_lengths: &str, after: bool) -> String {
    let predicate = if after { "where m.rowid > ?1" } else { "" };
    format!(
        "select m.rowid, {retained_lengths}, s.rowid from messages m \
         left join sessions s on s.id = m.session_id {predicate} \
         order by m.rowid limit 1"
    )
}

pub(super) fn goose_retained_length_expr(expressions: &[String]) -> String {
    // octet_length returns an integer without materializing stored TEXT/BLOB values.
    // Candidate statements run under a temporary limit lift and return only rowids
    // and this integer sum; hydration always runs after the provider cap is restored.
    expressions
        .iter()
        .map(|expression| format!("coalesce(octet_length({expression}), 0)"))
        .collect::<Vec<_>>()
        .join(" + ")
}

fn with_goose_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

pub(super) fn goose_message_values_at_rowid(
    conn: &Connection,
    rowid: i64,
) -> Result<Option<Vec<CapturedSqliteValue>>> {
    let columns = goose_message_columns(conn)?;
    let expressions = goose_message_expressions(&columns, "m");
    let select = expressions.hydration.join(", ");
    conn.query_row(
        &format!(
            "select s.rowid, {select} from messages m \
             left join sessions s on s.id = m.session_id where m.rowid = ?1"
        ),
        [rowid],
        |row| {
            let mut values = vec![row
                .get::<_, Option<i64>>(0)?
                .map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Integer)];
            values.extend(goose_message_values_at(row, 1)?);
            Ok(values)
        },
    )
    .optional()
    .map_err(CaptureError::from)
}

fn goose_fetch_first_candidate(
    conn: &Connection,
    statement: &mut Statement<'_>,
) -> Result<Option<GooseRowCandidate>> {
    with_goose_length_preflight(conn, || {
        statement
            .query_row([], |row| {
                Ok(GooseRowCandidate {
                    rowid: row.get(0)?,
                    retained_bytes: row.get(1)?,
                })
            })
            .optional()
    })
}

fn goose_fetch_next_candidate(
    conn: &Connection,
    statement: &mut Statement<'_>,
    after_rowid: i64,
) -> Result<Option<GooseRowCandidate>> {
    with_goose_length_preflight(conn, || {
        statement
            .query_row([after_rowid], |row| {
                Ok(GooseRowCandidate {
                    rowid: row.get(0)?,
                    retained_bytes: row.get(1)?,
                })
            })
            .optional()
    })
}

fn goose_fetch_first_message_candidate(
    conn: &Connection,
    statement: &mut Statement<'_>,
) -> Result<Option<GooseMessageCandidate>> {
    with_goose_length_preflight(conn, || {
        statement
            .query_row([], |row| {
                Ok(GooseMessageCandidate {
                    rowid: row.get(0)?,
                    retained_bytes: row.get(1)?,
                    parent_rowid: row.get(2)?,
                })
            })
            .optional()
    })
}

fn goose_fetch_next_message_candidate(
    conn: &Connection,
    statement: &mut Statement<'_>,
    after_rowid: i64,
) -> Result<Option<GooseMessageCandidate>> {
    with_goose_length_preflight(conn, || {
        statement
            .query_row([after_rowid], |row| {
                Ok(GooseMessageCandidate {
                    rowid: row.get(0)?,
                    retained_bytes: row.get(1)?,
                    parent_rowid: row.get(2)?,
                })
            })
            .optional()
    })
}

pub(super) fn goose_oversize_limit() -> Result<u64> {
    u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("Goose byte limit exceeds u64"))
}

fn goose_observed_bytes(retained_bytes: i64) -> Result<u64> {
    let payload = u64::try_from(retained_bytes).map_err(|_| {
        CaptureError::InvalidPayload(
            "Goose SQLite retained byte count must be nonnegative".to_owned(),
        )
    })?;
    GOOSE_SQLITE_VALUE_OVERHEAD_BYTES
        .checked_add(payload)
        .ok_or(CaptureError::SystemInvariant(
            "Goose SQLite retained byte count overflowed",
        ))
}

fn goose_captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(super) fn goose_sqlite_batch_error(
    error: SqliteLogicalRowsBatchError<CaptureError>,
) -> CaptureError {
    match error {
        SqliteLogicalRowsBatchError::Callback(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}
