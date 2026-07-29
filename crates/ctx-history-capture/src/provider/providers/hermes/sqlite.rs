//! Bounded provider-owned Hermes SQLite traversal.

use rusqlite::{Connection, OptionalExtension, Statement};

use crate::provider::{
    native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES,
    normalization::{provider_nonnegative_i64_to_u64, provider_required_timestamp_seconds},
    sqlite::SqliteLengthPreflightGuard,
};
use crate::{CaptureError, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES};

use super::layout::{
    decode_hermes_message, decode_hermes_session, HermesMessageRow, HermesSchema, HermesSessionRow,
    HermesSqliteValue,
};

// These constants remain the authority for the persisted Hermes frontier and
// locator wire contracts exercised at the serialization boundary.
#[allow(dead_code)]
pub(super) const HERMES_FRONTIER_VERSION: u32 = 1;
#[allow(dead_code)]
pub(super) const HERMES_LOCATOR_KIND: &str = "hermes-sqlite-row-v1";
const HERMES_FRONTIER_BYTES: usize = 1 + 8 + 8;
const HERMES_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 64 * 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(super) enum HermesPhase {
    Sessions,
    Messages,
}

impl HermesPhase {
    #[allow(dead_code)]
    fn tag(self) -> u8 {
        match self {
            Self::Sessions => 1,
            Self::Messages => 2,
        }
    }

    #[allow(dead_code)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(super) struct HermesFrontier {
    pub(super) phase: HermesPhase,
    pub(super) next_ordinal: u64,
    pub(super) rowid: i64,
}

impl HermesFrontier {
    pub(super) const fn initial() -> Self {
        Self {
            phase: HermesPhase::Sessions,
            next_ordinal: 0,
            rowid: i64::MIN,
        }
    }

    // Keep the codec local to the frontier type so persisted cursor shape stays
    // explicit even when the current scanner transfers the typed frontier.
    #[allow(dead_code)]
    pub(super) fn encode(self) -> Vec<u8> {
        let mut value = Vec::with_capacity(HERMES_FRONTIER_BYTES);
        value.push(self.phase.tag());
        value.extend_from_slice(&self.next_ordinal.to_be_bytes());
        value.extend_from_slice(&hermes_ordered_i64(self.rowid).to_be_bytes());
        value
    }

    #[allow(dead_code)]
    pub(super) fn decode(value: &[u8]) -> Result<Self> {
        if value.len() != HERMES_FRONTIER_BYTES {
            return Err(CaptureError::InvalidPayload(
                "Hermes cursor has an invalid frontier width".to_owned(),
            ));
        }
        Ok(Self {
            phase: HermesPhase::from_tag(value[0])?,
            next_ordinal: decode_u64(&value[1..9])?,
            rowid: hermes_unordered_i64(decode_u64(&value[9..17])?),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HermesLocator {
    pub(super) phase: HermesPhase,
    pub(super) rowid: i64,
}

#[derive(Debug)]
pub(super) enum HermesNativeRecord {
    Session(HermesSessionRow),
    Message {
        row: HermesMessageRow,
        values: Vec<HermesSqliteValue>,
        prepared: Option<super::HermesPreparedCoreMessage>,
    },
    Rejected(String),
}

#[derive(Debug)]
pub(super) struct HermesNativeRow {
    pub(super) ordinal: u64,
    pub(super) locator: HermesLocator,
    pub(super) next_frontier: HermesFrontier,
    pub(super) observed_bytes: usize,
    pub(super) record: HermesNativeRecord,
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
    format!(
        "select m.rowid, {retained_bytes}, {storage_error}, m.role = 'tool' \
         from messages m{where_clause} \
         order by m.rowid limit 1"
    )
}

pub(super) struct HermesRowReader<'connection> {
    conn: &'connection Connection,
    schema: HermesSchema,
    first_session_candidate: Statement<'connection>,
    next_session_candidate: Statement<'connection>,
    session_hydration: Statement<'connection>,
    first_message_candidate: Statement<'connection>,
    next_message_candidate: Statement<'connection>,
    message_hydration: Statement<'connection>,
    #[cfg(test)]
    pub(super) session_hydration_queries: usize,
}

impl<'connection> HermesRowReader<'connection> {
    pub(super) fn new(conn: &'connection Connection, schema: &HermesSchema) -> Result<Self> {
        let sessions = schema.sessions();
        let messages = schema.messages();
        Ok(Self {
            conn,
            schema: schema.clone(),
            first_session_candidate: conn.prepare(&hermes_session_candidate_sql(
                &sessions.retained_length_expr(),
                &sessions.storage_class_error_expr(),
                false,
            ))?,
            next_session_candidate: conn.prepare(&hermes_session_candidate_sql(
                &sessions.retained_length_expr(),
                &sessions.storage_class_error_expr(),
                true,
            ))?,
            session_hydration: conn.prepare(&format!(
                "select {} from sessions s where s.rowid = ?1",
                sessions.projection()
            ))?,
            first_message_candidate: conn.prepare(&hermes_message_candidate_sql(
                &messages.retained_length_expr(),
                &messages.storage_class_error_expr(),
                schema.message_visibility(),
                false,
            ))?,
            next_message_candidate: conn.prepare(&hermes_message_candidate_sql(
                &messages.retained_length_expr(),
                &messages.storage_class_error_expr(),
                schema.message_visibility(),
                true,
            ))?,
            message_hydration: conn.prepare(&format!(
                "select {} from messages m where m.rowid = ?1",
                messages.projection()
            ))?,
            #[cfg(test)]
            session_hydration_queries: 0,
        })
    }

    pub(super) fn next(&mut self, frontier: HermesFrontier) -> Result<Option<HermesNativeRow>> {
        if frontier.phase == HermesPhase::Sessions {
            let after = (frontier.next_ordinal != 0).then_some(frontier.rowid);
            if let Some(candidate) = self.session_candidate(after)? {
                return self.hydrate(candidate, frontier.next_ordinal).map(Some);
            }
            return self.message_candidate(None)?.map_or(Ok(None), |candidate| {
                self.hydrate(candidate, frontier.next_ordinal).map(Some)
            });
        }
        self.message_candidate(Some(frontier.rowid))?
            .map_or(Ok(None), |candidate| {
                self.hydrate(candidate, frontier.next_ordinal).map(Some)
            })
    }

    fn session_candidate(&mut self, after: Option<i64>) -> Result<Option<HermesCandidate>> {
        let conn = self.conn;
        with_length_preflight(conn, || {
            let read = |row: &rusqlite::Row<'_>| {
                Ok(HermesCandidate {
                    phase: HermesPhase::Sessions,
                    rowid: row.get(0)?,
                    retained_bytes: row.get(1)?,
                    storage_error_code: row.get(2)?,
                    indivisible: true,
                })
            };
            match after {
                Some(rowid) => self
                    .next_session_candidate
                    .query_row([rowid], read)
                    .optional(),
                None => self.first_session_candidate.query_row([], read).optional(),
            }
        })
    }

    fn message_candidate(&mut self, after: Option<i64>) -> Result<Option<HermesCandidate>> {
        let conn = self.conn;
        with_length_preflight(conn, || {
            let read = |row: &rusqlite::Row<'_>| {
                Ok(HermesCandidate {
                    phase: HermesPhase::Messages,
                    rowid: row.get(0)?,
                    retained_bytes: row.get(1)?,
                    storage_error_code: row.get(2)?,
                    indivisible: row.get::<_, i64>(3)? != 0,
                })
            };
            match after {
                Some(rowid) => self
                    .next_message_candidate
                    .query_row([rowid], read)
                    .optional(),
                None => self.first_message_candidate.query_row([], read).optional(),
            }
        })
    }

    fn hydrate(&mut self, candidate: HermesCandidate, ordinal: u64) -> Result<HermesNativeRow> {
        let next_frontier = HermesFrontier {
            phase: candidate.phase,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Hermes native row ordinal overflowed",
            ))?,
            rowid: candidate.rowid,
        };
        let mut observed_bytes = candidate.observed_bytes()?;
        let locator = HermesLocator {
            phase: candidate.phase,
            rowid: candidate.rowid,
        };
        let hydration_limit_exceeded = observed_bytes > MAX_PROVIDER_SQLITE_VALUE_BYTES;
        let native_page_limit_exceeded =
            observed_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES && candidate.indivisible;
        if hydration_limit_exceeded || native_page_limit_exceeded {
            let limit = if hydration_limit_exceeded {
                MAX_PROVIDER_SQLITE_VALUE_BYTES
            } else {
                NATIVE_INGESTION_PAGE_MAX_BYTES
            };
            let label = if hydration_limit_exceeded {
                "hydration"
            } else {
                "NativePath page"
            };
            let reason = format!(
                "Hermes {:?} row {} is an indivisible {}-byte record and exceeds the {}-byte {label} limit",
                candidate.phase,
                candidate.rowid,
                observed_bytes,
                limit
            );
            return Ok(HermesNativeRow {
                ordinal,
                locator,
                next_frontier,
                observed_bytes: rejection_owned_bytes(&reason),
                record: HermesNativeRecord::Rejected(reason),
            });
        }
        if candidate.storage_error_code != 0 {
            let reason =
                storage_error_reason(&self.schema, candidate.phase, candidate.storage_error_code)?;
            return Ok(HermesNativeRow {
                ordinal,
                locator,
                next_frontier,
                observed_bytes: rejection_owned_bytes(&reason),
                record: HermesNativeRecord::Rejected(reason),
            });
        }
        let record = match candidate.phase {
            HermesPhase::Sessions => {
                #[cfg(test)]
                {
                    self.session_hydration_queries += 1;
                }
                let layout = self.schema.sessions();
                let values = self
                    .session_hydration
                    .query_row([candidate.rowid], |row| layout.capture_values(row, 0))?;
                let row = decode_hermes_session(&self.schema, &values, 0)?;
                let validation = provider_required_timestamp_seconds(
                    row.started_at,
                    "Hermes session started_at",
                )
                .and_then(|_| {
                    row.ended_at
                        .map(|ended_at| {
                            provider_required_timestamp_seconds(ended_at, "Hermes session ended_at")
                                .map(|_| ())
                        })
                        .transpose()
                        .map(|_| ())
                });
                match validation {
                    Ok(()) => HermesNativeRecord::Session(row),
                    Err(CaptureError::InvalidPayload(reason)) => {
                        HermesNativeRecord::Rejected(reason)
                    }
                    Err(error) => return Err(error),
                }
            }
            HermesPhase::Messages => {
                let layout = self.schema.messages();
                let mut values = self
                    .message_hydration
                    .query_row([candidate.rowid], |row| layout.capture_values(row, 0))?;
                let mut row = decode_hermes_message(&self.schema, &values)?;
                let validation = provider_nonnegative_i64_to_u64(row.id, "Hermes message id")
                    .and_then(|_| {
                        provider_required_timestamp_seconds(
                            row.timestamp,
                            "Hermes message timestamp",
                        )
                        .map(|_| ())
                    });
                match validation {
                    Ok(()) => {
                        let prepared = (observed_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES)
                            .then(|| super::prepare_hermes_core_message(&row, ordinal, &values))
                            .transpose()?;
                        if let Some(prepared) = prepared.as_ref() {
                            observed_bytes = prepared
                                .owned_bytes()
                                .saturating_add(row.session_id.len())
                                .saturating_add(row.role.len())
                                .saturating_add(256);
                            row.content = None;
                            row.tool_call_id = None;
                            row.tool_calls = None;
                            row.tool_name = None;
                            row.finish_reason = None;
                            row.reasoning = None;
                            row.reasoning_content = None;
                            row.reasoning_details = None;
                            row.codex_reasoning_items = None;
                            row.codex_message_items = None;
                            row.platform_message_id = None;
                            values.clear();
                        }
                        HermesNativeRecord::Message {
                            row,
                            values,
                            prepared,
                        }
                    }
                    Err(CaptureError::InvalidPayload(reason)) => {
                        HermesNativeRecord::Rejected(reason)
                    }
                    Err(error) => return Err(error),
                }
            }
        };
        Ok(HermesNativeRow {
            ordinal,
            locator,
            next_frontier,
            observed_bytes,
            record,
        })
    }
}

fn rejection_owned_bytes(reason: &str) -> usize {
    // Ordinal, locator, frontier, record tag, and the length-prefixed reason.
    (8 + 9 + HERMES_FRONTIER_BYTES + 1 + 8).saturating_add(reason.len())
}

fn with_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

struct HermesCandidate {
    phase: HermesPhase,
    rowid: i64,
    retained_bytes: i64,
    storage_error_code: i64,
    indivisible: bool,
}

impl HermesCandidate {
    fn observed_bytes(&self) -> Result<usize> {
        let payload = u64::try_from(self.retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload(
                "Hermes SQLite retained byte count must be nonnegative".to_owned(),
            )
        })?;
        let total = HERMES_SQLITE_VALUE_OVERHEAD_BYTES
            .checked_add(payload)
            .ok_or(CaptureError::SystemInvariant(
                "Hermes SQLite retained byte count overflowed",
            ))?;
        usize::try_from(total).map_err(|_| {
            CaptureError::InvalidPayload(
                "Hermes SQLite retained byte count exceeds platform limits".to_owned(),
            )
        })
    }
}

fn storage_error_reason(schema: &HermesSchema, phase: HermesPhase, code: i64) -> Result<String> {
    let (record, column) = match phase {
        HermesPhase::Sessions => ("session", schema.sessions().rejected_column(code)?),
        HermesPhase::Messages => ("message", schema.messages().rejected_column(code)?),
    };
    Ok(format!(
        "Hermes {record} {column} has an unsupported SQLite storage class"
    ))
}

#[allow(dead_code)]
fn decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload("Hermes cursor integer has an invalid width".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

#[allow(dead_code)]
fn hermes_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

#[allow(dead_code)]
fn hermes_unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}
