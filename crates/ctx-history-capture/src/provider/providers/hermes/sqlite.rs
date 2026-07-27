//! Bounded Hermes SQLite traversal and native position codecs.
//!
//! This module consumes `layout` policy; it does not own normalization, Store transactions,
//! cursor publication, or source admission.

use rusqlite::{Connection, OptionalExtension, Statement};

use crate::captured_batch::sqlite_logical_rows::{SqliteLogicalRow, SqliteLogicalRowsBatchError};
use crate::captured_batch::{
    CapturedSqliteValue, NativeLocator, NativePosition, ProviderRecordKind,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::provider::sqlite::SqliteLengthPreflightGuard;
use crate::{CaptureError, Result};

use super::hermes_captured_error;
use super::layout::HermesSchema;

pub(super) const HERMES_SESSION_RECORD_KIND: &str = "hermes-session-v1";
pub(super) const HERMES_MESSAGE_RECORD_KIND: &str = "hermes-message-v1";
pub(super) const HERMES_MALFORMED_RECORD_KIND: &str = "hermes-malformed-v1";

const HERMES_POSITION_KIND: &str = "hermes-sqlite-keyset-v1";
const HERMES_LOCATOR_KIND: &str = "hermes-sqlite-row-v1";
const HERMES_POSITION_BYTES: usize = 1 + 8 + 8;
const HERMES_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 64 * 9;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HermesPhase {
    Sessions,
    Messages,
}

impl HermesPhase {
    fn tag(self) -> u8 {
        match self {
            Self::Sessions => 1,
            Self::Messages => 2,
        }
    }

    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(Self::Sessions),
            2 => Ok(Self::Messages),
            _ => Err(CaptureError::InvalidPayload(
                "Hermes cursor has an unknown phase".to_owned(),
            )),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct HermesKeyset {
    pub(super) phase: HermesPhase,
    pub(super) next_ordinal: u64,
    pub(super) rowid: i64,
}

pub(super) fn hermes_session_candidate_sql(
    retained_bytes: &str,
    storage_error: &str,
    has_after_rowid: bool,
) -> String {
    let rowid_bound = if has_after_rowid {
        " where s.rowid > ?1"
    } else {
        ""
    };
    format!(
        "select s.rowid, {retained_bytes}, {storage_error} from sessions s{rowid_bound} \
         order by s.rowid limit 1"
    )
}

pub(super) fn hermes_message_candidate_sql(
    retained_bytes: &str,
    storage_error: &str,
    visibility: &str,
    has_after_rowid: bool,
) -> String {
    let mut predicates = Vec::with_capacity(2);
    if has_after_rowid {
        predicates.push("m.rowid > ?1");
    }
    if !visibility.is_empty() {
        predicates.push(visibility);
    }
    let where_clause = if predicates.is_empty() {
        String::new()
    } else {
        format!(" where {}", predicates.join(" and "))
    };
    // The inner join preserves Hermes' existing orphan policy but projects no
    // parent payload. Sessions are emitted authoritatively in the first phase.
    format!(
        "select m.rowid, {retained_bytes}, {storage_error} \
         from messages m join sessions s on s.id = m.session_id{where_clause} \
         order by m.rowid limit 1"
    )
}

pub(super) struct HermesRowFetcher<'connection> {
    conn: &'connection Connection,
    schema: HermesSchema,
    first_session_candidate: Statement<'connection>,
    next_session_candidate: Statement<'connection>,
    session_hydration: Statement<'connection>,
    first_message_candidate: Statement<'connection>,
    next_message_candidate: Statement<'connection>,
    message_hydration: Statement<'connection>,
    session_record_kind: ProviderRecordKind,
    message_record_kind: ProviderRecordKind,
    malformed_record_kind: ProviderRecordKind,
    #[cfg(test)]
    pub(super) session_hydration_queries: usize,
}

impl<'connection> HermesRowFetcher<'connection> {
    pub(super) fn new(conn: &'connection Connection, schema: &HermesSchema) -> Result<Self> {
        let session_layout = schema.sessions();
        let message_layout = schema.messages();
        let first_session_candidate_sql = hermes_session_candidate_sql(
            &session_layout.retained_length_expr(),
            &session_layout.storage_class_error_expr(),
            false,
        );
        let next_session_candidate_sql = hermes_session_candidate_sql(
            &session_layout.retained_length_expr(),
            &session_layout.storage_class_error_expr(),
            true,
        );
        let first_message_candidate_sql = hermes_message_candidate_sql(
            &message_layout.retained_length_expr(),
            &message_layout.storage_class_error_expr(),
            schema.message_visibility(),
            false,
        );
        let next_message_candidate_sql = hermes_message_candidate_sql(
            &message_layout.retained_length_expr(),
            &message_layout.storage_class_error_expr(),
            schema.message_visibility(),
            true,
        );
        Ok(Self {
            conn,
            schema: schema.clone(),
            first_session_candidate: conn.prepare(&first_session_candidate_sql)?,
            next_session_candidate: conn.prepare(&next_session_candidate_sql)?,
            session_hydration: conn.prepare(&format!(
                "select {} from sessions s where s.rowid = ?1",
                session_layout.projection()
            ))?,
            first_message_candidate: conn.prepare(&first_message_candidate_sql)?,
            next_message_candidate: conn.prepare(&next_message_candidate_sql)?,
            message_hydration: conn.prepare(&format!(
                "select {} from messages m where m.rowid = ?1",
                message_layout.projection()
            ))?,
            session_record_kind: ProviderRecordKind::new(HERMES_SESSION_RECORD_KIND)
                .map_err(hermes_captured_error)?,
            message_record_kind: ProviderRecordKind::new(HERMES_MESSAGE_RECORD_KIND)
                .map_err(hermes_captured_error)?,
            malformed_record_kind: ProviderRecordKind::new(HERMES_MALFORMED_RECORD_KIND)
                .map_err(hermes_captured_error)?,
            #[cfg(test)]
            session_hydration_queries: 0,
        })
    }

    pub(super) fn fetch(&mut self, after: NativePosition) -> Result<Option<SqliteLogicalRow>> {
        let keyset = decode_hermes_position(&after)?;
        let (phase, after_rowid, ordinal) = match keyset {
            Some(keyset) => (keyset.phase, Some(keyset.rowid), keyset.next_ordinal),
            None => (HermesPhase::Sessions, None, 0),
        };
        if phase == HermesPhase::Sessions {
            if let Some(candidate) = self.fetch_session_candidate(after_rowid)? {
                return self.hydrate_session(candidate, ordinal).map(Some);
            }
            return self
                .fetch_message_candidate(None)?
                .map_or(Ok(None), |candidate| {
                    self.hydrate_message(candidate, ordinal).map(Some)
                });
        }
        self.fetch_message_candidate(after_rowid)?
            .map_or(Ok(None), |candidate| {
                self.hydrate_message(candidate, ordinal).map(Some)
            })
    }

    fn fetch_session_candidate(
        &mut self,
        after_rowid: Option<i64>,
    ) -> Result<Option<HermesCandidate>> {
        let conn = self.conn;
        with_hermes_length_preflight(conn, || {
            if let Some(after_rowid) = after_rowid {
                self.next_session_candidate
                    .query_row([after_rowid], |row| {
                        Ok(HermesCandidate {
                            phase: HermesPhase::Sessions,
                            rowid: row.get(0)?,
                            retained_bytes: row.get(1)?,
                            storage_error_code: row.get(2)?,
                        })
                    })
                    .optional()
            } else {
                self.first_session_candidate
                    .query_row([], |row| {
                        Ok(HermesCandidate {
                            phase: HermesPhase::Sessions,
                            rowid: row.get(0)?,
                            retained_bytes: row.get(1)?,
                            storage_error_code: row.get(2)?,
                        })
                    })
                    .optional()
            }
        })
    }

    fn fetch_message_candidate(
        &mut self,
        after_rowid: Option<i64>,
    ) -> Result<Option<HermesCandidate>> {
        let conn = self.conn;
        with_hermes_length_preflight(conn, || {
            if let Some(after_rowid) = after_rowid {
                self.next_message_candidate
                    .query_row([after_rowid], |row| {
                        Ok(HermesCandidate {
                            phase: HermesPhase::Messages,
                            rowid: row.get(0)?,
                            retained_bytes: row.get(1)?,
                            storage_error_code: row.get(2)?,
                        })
                    })
                    .optional()
            } else {
                self.first_message_candidate
                    .query_row([], |row| {
                        Ok(HermesCandidate {
                            phase: HermesPhase::Messages,
                            rowid: row.get(0)?,
                            retained_bytes: row.get(1)?,
                            storage_error_code: row.get(2)?,
                        })
                    })
                    .optional()
            }
        })
    }

    fn hydrate_session(
        &mut self,
        candidate: HermesCandidate,
        ordinal: u64,
    ) -> Result<SqliteLogicalRow> {
        let next_position = encode_hermes_position(HermesKeyset {
            phase: HermesPhase::Sessions,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Hermes captured row ordinal overflowed",
            ))?,
            rowid: candidate.rowid,
        })?;
        let locator = hermes_locator(HermesPhase::Sessions, candidate.rowid)?;
        if candidate.observed_bytes()? > hermes_oversize_limit()? {
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                locator,
                self.session_record_kind.clone(),
                candidate.observed_bytes()?,
            )
            .map_err(hermes_captured_error);
        }
        if candidate.storage_error_code != 0 {
            hermes_storage_error_reason(
                &self.schema,
                candidate.phase,
                candidate.storage_error_code,
            )?;
            return SqliteLogicalRow::values(
                next_position,
                ordinal,
                locator,
                self.malformed_record_kind.clone(),
                hermes_malformed_values(&candidate),
            )
            .map_err(hermes_captured_error);
        }
        #[cfg(test)]
        {
            self.session_hydration_queries += 1;
        }
        let layout = self.schema.sessions();
        let values = self
            .session_hydration
            .query_row([candidate.rowid], |row| layout.capture_values(row, 0))?;
        SqliteLogicalRow::values(
            next_position,
            ordinal,
            locator,
            self.session_record_kind.clone(),
            values,
        )
        .map_err(hermes_captured_error)
    }

    fn hydrate_message(
        &mut self,
        candidate: HermesCandidate,
        ordinal: u64,
    ) -> Result<SqliteLogicalRow> {
        let next_position = encode_hermes_position(HermesKeyset {
            phase: HermesPhase::Messages,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Hermes captured row ordinal overflowed",
            ))?,
            rowid: candidate.rowid,
        })?;
        let locator = hermes_locator(HermesPhase::Messages, candidate.rowid)?;
        if candidate.observed_bytes()? > hermes_oversize_limit()? {
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                locator,
                self.message_record_kind.clone(),
                candidate.observed_bytes()?,
            )
            .map_err(hermes_captured_error);
        }
        if candidate.storage_error_code != 0 {
            hermes_storage_error_reason(
                &self.schema,
                candidate.phase,
                candidate.storage_error_code,
            )?;
            return SqliteLogicalRow::values(
                next_position,
                ordinal,
                locator,
                self.malformed_record_kind.clone(),
                hermes_malformed_values(&candidate),
            )
            .map_err(hermes_captured_error);
        }
        let layout = self.schema.messages();
        let values = self
            .message_hydration
            .query_row([candidate.rowid], |row| layout.capture_values(row, 0))?;
        SqliteLogicalRow::values(
            next_position,
            ordinal,
            locator,
            self.message_record_kind.clone(),
            values,
        )
        .map_err(hermes_captured_error)
    }
}

fn with_hermes_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    // SQLITE_LIMIT_LENGTH rejects even integer-only octet_length inspection of
    // an oversized stored record. Candidate SQL returns only rowid, byte
    // counts, and a compact storage-class code, so temporarily lift the limit
    // and restore it before hydration.
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

struct HermesCandidate {
    phase: HermesPhase,
    rowid: i64,
    retained_bytes: i64,
    storage_error_code: i64,
}

impl HermesCandidate {
    fn observed_bytes(&self) -> Result<u64> {
        let payload = u64::try_from(self.retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload(
                "Hermes SQLite retained byte count must be nonnegative".to_owned(),
            )
        })?;
        HERMES_SQLITE_VALUE_OVERHEAD_BYTES
            .checked_add(payload)
            .ok_or(CaptureError::SystemInvariant(
                "Hermes SQLite retained byte count overflowed",
            ))
    }
}

fn hermes_malformed_values(candidate: &HermesCandidate) -> Vec<CapturedSqliteValue> {
    vec![
        CapturedSqliteValue::Integer(i64::from(candidate.phase.tag())),
        CapturedSqliteValue::Integer(candidate.rowid),
        CapturedSqliteValue::Integer(candidate.storage_error_code),
    ]
}

fn hermes_storage_error_reason(
    schema: &HermesSchema,
    phase: HermesPhase,
    code: i64,
) -> Result<String> {
    let (record, column) = match phase {
        HermesPhase::Sessions => ("session", schema.sessions().rejected_column(code)?),
        HermesPhase::Messages => ("message", schema.messages().rejected_column(code)?),
    };
    Ok(format!(
        "Hermes {record} {column} has an unsupported SQLite storage class"
    ))
}

pub(super) fn decode_hermes_storage_rejection(
    schema: &HermesSchema,
    values: &[CapturedSqliteValue],
) -> Result<String> {
    let [CapturedSqliteValue::Integer(phase), CapturedSqliteValue::Integer(_rowid), CapturedSqliteValue::Integer(code)] =
        values
    else {
        return Err(CaptureError::SystemInvariant(
            "Hermes malformed logical row has an invalid value shape",
        ));
    };
    let phase = u8::try_from(*phase)
        .ok()
        .and_then(|phase| HermesPhase::from_tag(phase).ok())
        .ok_or(CaptureError::SystemInvariant(
            "Hermes malformed logical row has an invalid phase",
        ))?;
    hermes_storage_error_reason(schema, phase, *code)
}

pub(super) fn initial_hermes_position() -> Result<NativePosition> {
    NativePosition::new(HERMES_POSITION_KIND, vec![0]).map_err(hermes_captured_error)
}

pub(super) fn encode_hermes_position(keyset: HermesKeyset) -> Result<NativePosition> {
    let mut value = Vec::with_capacity(HERMES_POSITION_BYTES);
    value.push(keyset.phase.tag());
    value.extend_from_slice(&keyset.next_ordinal.to_be_bytes());
    value.extend_from_slice(&hermes_ordered_i64(keyset.rowid).to_be_bytes());
    NativePosition::new(HERMES_POSITION_KIND, value).map_err(hermes_captured_error)
}

pub(super) fn decode_hermes_position(position: &NativePosition) -> Result<Option<HermesKeyset>> {
    if position.kind() != HERMES_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "Hermes cursor has an unexpected native-position kind".to_owned(),
        ));
    }
    if position.value() == [0] {
        return Ok(None);
    }
    if position.value().len() != HERMES_POSITION_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Hermes cursor has an invalid native-position payload".to_owned(),
        ));
    }
    Ok(Some(HermesKeyset {
        phase: HermesPhase::from_tag(position.value()[0])?,
        next_ordinal: hermes_decode_u64(&position.value()[1..9])?,
        rowid: hermes_unordered_i64(hermes_decode_u64(&position.value()[9..17])?),
    }))
}

pub(super) fn hermes_locator(phase: HermesPhase, rowid: i64) -> Result<NativeLocator> {
    let mut value = Vec::with_capacity(9);
    value.push(phase.tag());
    value.extend_from_slice(&rowid.to_be_bytes());
    NativeLocator::new(HERMES_LOCATOR_KIND, value).map_err(hermes_captured_error)
}

fn hermes_decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload("Hermes cursor integer has an invalid width".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

fn hermes_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

fn hermes_unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}

fn hermes_oversize_limit() -> Result<u64> {
    u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("Hermes captured record byte limit exceeds u64"))
}

pub(super) fn hermes_sqlite_batch_error(
    error: SqliteLogicalRowsBatchError<CaptureError>,
) -> CaptureError {
    match error {
        SqliteLogicalRowsBatchError::Callback(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}
