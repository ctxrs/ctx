//! Bounded provider-owned Hermes SQLite traversal.

#[cfg(any(test, feature = "test-support"))]
use std::collections::VecDeque;
use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom, Write},
};

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static EXACT_GLOBAL_MESSAGE_TRAVERSALS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static SESSION_SCOPED_MESSAGE_CANDIDATE_QUERIES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static EXACT_MESSAGE_SPOOL_FILES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static ACTIVE_MESSAGE_REPLAYS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static PEAK_ACTIVE_MESSAGE_REPLAYS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static CREATED_MESSAGE_REPLAYS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static DROPPED_MESSAGE_REPLAYS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn reset_exact_message_query_counters() {
    EXACT_GLOBAL_MESSAGE_TRAVERSALS.with(|count| count.set(0));
    SESSION_SCOPED_MESSAGE_CANDIDATE_QUERIES.with(|count| count.set(0));
    EXACT_MESSAGE_SPOOL_FILES.with(|count| count.set(0));
    ACTIVE_MESSAGE_REPLAYS.with(|count| {
        assert_eq!(count.get(), 0, "Hermes message replay leaked across a scan");
    });
    PEAK_ACTIVE_MESSAGE_REPLAYS.with(|count| count.set(0));
    CREATED_MESSAGE_REPLAYS.with(|count| count.set(0));
    DROPPED_MESSAGE_REPLAYS.with(|count| count.set(0));
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn exact_message_spool_counters() -> (u64, u64, u64, u64, u64) {
    (
        EXACT_MESSAGE_SPOOL_FILES.with(std::cell::Cell::get),
        ACTIVE_MESSAGE_REPLAYS.with(std::cell::Cell::get),
        PEAK_ACTIVE_MESSAGE_REPLAYS.with(std::cell::Cell::get),
        CREATED_MESSAGE_REPLAYS.with(std::cell::Cell::get),
        DROPPED_MESSAGE_REPLAYS.with(std::cell::Cell::get),
    )
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn exact_message_query_counters() -> (u64, u64) {
    (
        EXACT_GLOBAL_MESSAGE_TRAVERSALS.with(std::cell::Cell::get),
        SESSION_SCOPED_MESSAGE_CANDIDATE_QUERIES.with(std::cell::Cell::get),
    )
}

use rusqlite::{params_from_iter, Connection, Statement};

use crate::{
    native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES,
    normalization::{provider_nonnegative_i64_to_u64, provider_required_timestamp_seconds},
    source_sqlite::SqliteLengthPreflightGuard,
};
use crate::{CaptureError, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES};

use super::layout::{
    decode_hermes_message, decode_hermes_session, HermesMessageRow, HermesSchema, HermesSessionRow,
    HermesSqliteValue,
};

const HERMES_FRONTIER_ACCOUNTING_BYTES: usize = 1 + 8 + 8;
const HERMES_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 64 * 9;
const HERMES_NATIVE_ROW_BATCH: usize = 64;
const HERMES_MESSAGE_SPOOL_RECORD_BYTES: u64 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HermesMessageSpoolRange {
    tail_offset: Option<u64>,
    rowids: u64,
}

pub(super) struct HermesExactMessageSpool {
    file: std::fs::File,
    next_offset: u64,
}

pub(super) struct HermesMessageReplay {
    file: std::fs::File,
    rowids: u64,
}

impl HermesExactMessageSpool {
    pub(super) fn new() -> Result<Self> {
        #[cfg(any(test, feature = "test-support"))]
        EXACT_MESSAGE_SPOOL_FILES.with(|count| count.set(count.get().saturating_add(1)));
        Ok(Self {
            file: tempfile::tempfile()?,
            next_offset: 0,
        })
    }

    pub(super) fn push(&mut self, range: &mut HermesMessageSpoolRange, rowid: i64) -> Result<()> {
        self.file.seek(SeekFrom::Start(self.next_offset))?;
        self.file
            .write_all(&range.tail_offset.unwrap_or(u64::MAX).to_be_bytes())?;
        self.file.write_all(&rowid.to_be_bytes())?;
        range.tail_offset = Some(self.next_offset);
        range.rowids = range
            .rowids
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Hermes message spool count overflowed",
            ))?;
        self.next_offset = self
            .next_offset
            .checked_add(HERMES_MESSAGE_SPOOL_RECORD_BYTES)
            .ok_or(CaptureError::SystemInvariant(
                "Hermes message spool offset overflowed",
            ))?;
        Ok(())
    }

    pub(super) fn prepare_replay(
        &mut self,
        range: HermesMessageSpoolRange,
    ) -> Result<HermesMessageReplay> {
        let mut replay = HermesMessageReplay {
            file: tempfile::tempfile()?,
            rowids: range.rowids,
        };
        #[cfg(any(test, feature = "test-support"))]
        {
            CREATED_MESSAGE_REPLAYS.with(|count| count.set(count.get().saturating_add(1)));
            ACTIVE_MESSAGE_REPLAYS.with(|active| {
                let current = active.get().saturating_add(1);
                active.set(current);
                PEAK_ACTIVE_MESSAGE_REPLAYS.with(|peak| peak.set(peak.get().max(current)));
            });
        }
        let mut offset = range.tail_offset;
        let mut visited = 0_u64;
        while let Some(current) = offset {
            self.file.seek(SeekFrom::Start(current))?;
            let mut previous = [0_u8; 8];
            let mut rowid = [0_u8; 8];
            self.file.read_exact(&mut previous)?;
            self.file.read_exact(&mut rowid)?;
            replay.file.write_all(&rowid)?;
            let previous = u64::from_be_bytes(previous);
            offset = (previous != u64::MAX).then_some(previous);
            visited = visited.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Hermes message replay count overflowed",
            ))?;
        }
        if visited != range.rowids {
            return Err(CaptureError::SystemInvariant(
                "Hermes message spool chain length mismatch",
            ));
        }
        Ok(replay)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for HermesMessageReplay {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "test-support"))]
        {
            ACTIVE_MESSAGE_REPLAYS.with(|active| {
                active.set(
                    active
                        .get()
                        .checked_sub(1)
                        .expect("Hermes message replay counter underflowed"),
                );
            });
            DROPPED_MESSAGE_REPLAYS.with(|count| count.set(count.get().saturating_add(1)));
        }
    }
}

impl HermesMessageSpoolRange {
    pub(super) const fn empty() -> Self {
        Self {
            tail_offset: None,
            rowids: 0,
        }
    }
}

impl HermesMessageReplay {
    fn rowid_page(&mut self, consumed: u64) -> Result<Vec<i64>> {
        if consumed >= self.rowids {
            return Ok(Vec::new());
        }
        let count = self
            .rowids
            .saturating_sub(consumed)
            .min(HERMES_NATIVE_ROW_BATCH as u64);
        let start = self
            .rowids
            .saturating_sub(consumed)
            .saturating_sub(count)
            .checked_mul(8)
            .ok_or(CaptureError::SystemInvariant(
                "Hermes message replay offset overflowed",
            ))?;
        self.file.seek(SeekFrom::Start(start))?;
        let mut rowids = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let mut rowid = [0_u8; 8];
            self.file.read_exact(&mut rowid)?;
            rowids.push(i64::from_be_bytes(rowid));
        }
        rowids.reverse();
        Ok(rowids)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HermesSessionIdentityRow {
    pub(super) rowid: i64,
    pub(super) provider_session_id: String,
}

pub(super) fn hermes_session_identity_page(
    conn: &Connection,
    after_rowid: Option<i64>,
    upper_rowid: i64,
) -> Result<Vec<HermesSessionIdentityRow>> {
    let predicate = after_rowid
        .map(|_| " where rowid > ?1 and rowid <= ?2")
        .unwrap_or(" where rowid <= ?1");
    let sql = format!(
        "select rowid, id from sessions{predicate} order by rowid limit {HERMES_NATIVE_ROW_BATCH}"
    );
    with_length_preflight(conn, || {
        let mut statement = conn.prepare(&sql)?;
        let read = |row: &rusqlite::Row<'_>| {
            Ok(HermesSessionIdentityRow {
                rowid: row.get(0)?,
                provider_session_id: row.get(1)?,
            })
        };
        match after_rowid {
            Some(rowid) => statement.query_map([rowid, upper_rowid], read)?.collect(),
            None => statement.query_map([upper_rowid], read)?.collect(),
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HermesMessageCursorRow {
    pub(super) rowid: i64,
    pub(super) provider_session_id: String,
}

pub(super) fn hermes_message_cursor_page(
    conn: &Connection,
    after_rowid: i64,
    upper_rowid: i64,
) -> Result<Vec<HermesMessageCursorRow>> {
    with_length_preflight(conn, || {
        let mut statement = conn.prepare(&format!(
            "select rowid, session_id from messages where rowid > ?1 and rowid <= ?2 \
             order by rowid limit {HERMES_NATIVE_ROW_BATCH}"
        ))?;
        let rows = statement
            .query_map([after_rowid, upper_rowid], |row| {
                Ok(HermesMessageCursorRow {
                    rowid: row.get(0)?,
                    provider_session_id: row.get(1)?,
                })
            })?
            .collect();
        rows
    })
}

pub(super) fn hermes_max_rowid(conn: &Connection, table: &str) -> Result<i64> {
    if !matches!(table, "sessions" | "messages") {
        return Err(CaptureError::SystemInvariant(
            "Hermes rowid cursor requested an unknown table",
        ));
    }
    with_length_preflight(conn, || {
        conn.query_row(
            &format!("select coalesce(max(rowid), 0) from {table}"),
            [],
            |row| row.get(0),
        )
    })
}

pub(super) fn hermes_message_session_id(conn: &Connection, rowid: i64) -> Result<String> {
    with_length_preflight(conn, || {
        conn.query_row(
            "select session_id from messages where rowid = ?1",
            [rowid],
            |row| row.get(0),
        )
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(super) enum HermesPhase {
    Sessions,
    Messages,
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HermesLocator {
    pub(super) phase: HermesPhase,
    pub(super) rowid: i64,
}

#[derive(Debug, Clone)]
pub(super) enum HermesNativeRecord {
    Session(HermesSessionRow),
    Message {
        row: HermesMessageRow,
        values: Vec<HermesSqliteValue>,
    },
    Rejected(String),
}

#[derive(Debug, Clone)]
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
    session_lookup_index: Option<&str>,
) -> String {
    let session_scoped = session_lookup_index.is_some();
    let mut predicates = Vec::with_capacity(2);
    if session_scoped {
        predicates.push("s.id collate binary = ?1 collate binary");
    }
    if has_after_rowid {
        predicates.push(if session_scoped {
            "s.rowid > ?2"
        } else {
            "s.rowid > ?1"
        });
    }
    let where_clause = if predicates.is_empty() {
        String::new()
    } else {
        format!(" where {}", predicates.join(" and "))
    };
    let indexed_by = session_lookup_index
        .map(|index| format!(" indexed by \"{}\"", index.replace('"', "\"\"")))
        .unwrap_or_default();
    format!(
        "select s.rowid, {retained_bytes}, {storage_error} from sessions s{indexed_by}{where_clause} \
         order by s.rowid limit {HERMES_NATIVE_ROW_BATCH}"
    )
}

pub(super) fn hermes_message_candidate_sql(
    retained_bytes: &str,
    storage_error: &str,
    visibility: &str,
    has_after_rowid: bool,
    session_scoped: bool,
) -> String {
    let mut predicates = Vec::with_capacity(3);
    if session_scoped {
        predicates.push("m.session_id collate binary = ?1 collate binary");
    }
    if has_after_rowid {
        predicates.push(if session_scoped {
            "m.rowid > ?2"
        } else {
            "m.rowid > ?1"
        });
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
        "select m.rowid, {retained_bytes}, {storage_error} \
         from messages m{where_clause} \
         order by m.rowid limit {HERMES_NATIVE_ROW_BATCH}"
    )
}

pub(super) struct HermesRowReader<'connection> {
    conn: &'connection Connection,
    schema: HermesSchema,
    session_scope: Option<String>,
    first_session_candidate: Statement<'connection>,
    next_session_candidate: Statement<'connection>,
    first_message_candidate: Statement<'connection>,
    next_message_candidate: Statement<'connection>,
    #[cfg(test)]
    buffered: VecDeque<HermesNativeRow>,
    #[cfg(test)]
    buffered_frontier: Option<HermesFrontier>,
    candidate_query_batches: u64,
    hydration_query_batches: u64,
    max_hydration_rows: u64,
    #[cfg(test)]
    pub(super) session_hydration_queries: usize,
    #[cfg(test)]
    pub(super) message_hydration_queries: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HermesRowReaderCounters {
    pub(super) candidate_query_batches: u64,
    pub(super) hydration_query_batches: u64,
    pub(super) max_hydration_rows: u64,
}

impl<'connection> HermesRowReader<'connection> {
    pub(super) fn new(conn: &'connection Connection, schema: &HermesSchema) -> Result<Self> {
        Self::with_session_scope(conn, schema, None)
    }

    pub(super) fn for_session(
        conn: &'connection Connection,
        schema: &HermesSchema,
        provider_session_id: &str,
    ) -> Result<Self> {
        Self::with_session_scope(conn, schema, Some(provider_session_id.to_owned()))
    }

    fn with_session_scope(
        conn: &'connection Connection,
        schema: &HermesSchema,
        session_scope: Option<String>,
    ) -> Result<Self> {
        let sessions = schema.sessions();
        let messages = schema.messages();
        let session_scoped = session_scope.is_some();
        Ok(Self {
            conn,
            schema: schema.clone(),
            session_scope,
            first_session_candidate: conn.prepare(&hermes_session_candidate_sql(
                &sessions.retained_length_expr(),
                &sessions.storage_class_error_expr(),
                false,
                session_scoped.then(|| schema.session_id_lookup_index()),
            ))?,
            next_session_candidate: conn.prepare(&hermes_session_candidate_sql(
                &sessions.retained_length_expr(),
                &sessions.storage_class_error_expr(),
                true,
                session_scoped.then(|| schema.session_id_lookup_index()),
            ))?,
            first_message_candidate: conn.prepare(&hermes_message_candidate_sql(
                &messages.retained_length_expr(),
                &messages.storage_class_error_expr(),
                schema.message_visibility(),
                false,
                session_scoped,
            ))?,
            next_message_candidate: conn.prepare(&hermes_message_candidate_sql(
                &messages.retained_length_expr(),
                &messages.storage_class_error_expr(),
                schema.message_visibility(),
                true,
                session_scoped,
            ))?,
            #[cfg(test)]
            buffered: VecDeque::new(),
            #[cfg(test)]
            buffered_frontier: None,
            candidate_query_batches: 0,
            hydration_query_batches: 0,
            max_hydration_rows: 0,
            #[cfg(test)]
            session_hydration_queries: 0,
            #[cfg(test)]
            message_hydration_queries: 0,
        })
    }

    pub(super) fn next_session_inventory_page(
        &mut self,
        after: Option<i64>,
    ) -> Result<Vec<HermesNativeRow>> {
        let candidates = bounded_candidate_prefix(self.session_candidates(after)?)?;
        self.hydrate_candidates(candidates, 0)
    }

    pub(super) fn next_message_inventory_page(
        &mut self,
        after: Option<i64>,
        first_ordinal: u64,
    ) -> Result<Vec<HermesNativeRow>> {
        let candidates = bounded_candidate_prefix(self.message_candidates(after)?)?;
        self.hydrate_candidates(candidates, first_ordinal)
    }

    pub(super) fn exact_message_page(
        &mut self,
        replay: &mut HermesMessageReplay,
        consumed: u64,
        first_ordinal: u64,
    ) -> Result<Vec<HermesNativeRow>> {
        let rowids = replay.rowid_page(consumed)?;
        let candidates = bounded_candidate_prefix(self.message_candidates_for_rowids(&rowids)?)?;
        self.hydrate_candidates(candidates, first_ordinal)
    }

    #[cfg(test)]
    pub(super) fn next(&mut self, frontier: HermesFrontier) -> Result<Option<HermesNativeRow>> {
        if self.buffered_frontier != Some(frontier) || self.buffered.is_empty() {
            self.buffered = self.read_page(frontier)?.into();
            self.buffered_frontier = Some(frontier);
        }
        let row = self.buffered.pop_front();
        if let Some(row) = &row {
            self.buffered_frontier = Some(row.next_frontier);
        }
        Ok(row)
    }

    pub(super) fn next_page(&mut self, frontier: HermesFrontier) -> Result<Vec<HermesNativeRow>> {
        #[cfg(test)]
        {
            self.buffered.clear();
            self.buffered_frontier = None;
        }
        self.read_page(frontier)
    }

    pub(super) fn counters(&self) -> HermesRowReaderCounters {
        HermesRowReaderCounters {
            candidate_query_batches: self.candidate_query_batches,
            hydration_query_batches: self.hydration_query_batches,
            max_hydration_rows: self.max_hydration_rows,
        }
    }

    fn read_page(&mut self, frontier: HermesFrontier) -> Result<Vec<HermesNativeRow>> {
        let candidates = if frontier.phase == HermesPhase::Sessions {
            let after = (frontier.next_ordinal != 0).then_some(frontier.rowid);
            let sessions = self.session_candidates(after)?;
            if sessions.is_empty() {
                self.message_candidates(None)?
            } else {
                sessions
            }
        } else {
            self.message_candidates(Some(frontier.rowid))?
        };
        let candidates = bounded_candidate_prefix(candidates)?;
        self.hydrate_candidates(candidates, frontier.next_ordinal)
    }

    fn session_candidates(&mut self, after: Option<i64>) -> Result<Vec<HermesCandidate>> {
        self.candidate_query_batches =
            checked_reader_counter(self.candidate_query_batches, "candidate query batches")?;
        let conn = self.conn;
        with_length_preflight(conn, || {
            let read = |row: &rusqlite::Row<'_>| {
                Ok(HermesCandidate {
                    phase: HermesPhase::Sessions,
                    rowid: row.get(0)?,
                    retained_bytes: row.get(1)?,
                    storage_error_code: row.get(2)?,
                })
            };
            match (self.session_scope.as_deref(), after) {
                (Some(session), Some(rowid)) => self
                    .next_session_candidate
                    .query_map(rusqlite::params![session, rowid], read)?
                    .collect(),
                (Some(session), None) => self
                    .first_session_candidate
                    .query_map([session], read)?
                    .collect(),
                (None, Some(rowid)) => self
                    .next_session_candidate
                    .query_map([rowid], read)?
                    .collect(),
                (None, None) => self.first_session_candidate.query_map([], read)?.collect(),
            }
        })
    }

    fn message_candidates(&mut self, after: Option<i64>) -> Result<Vec<HermesCandidate>> {
        #[cfg(any(test, feature = "test-support"))]
        if self.session_scope.is_some() {
            SESSION_SCOPED_MESSAGE_CANDIDATE_QUERIES
                .with(|count| count.set(count.get().saturating_add(1)));
        } else if after.is_none() {
            EXACT_GLOBAL_MESSAGE_TRAVERSALS.with(|count| count.set(count.get().saturating_add(1)));
        }
        self.candidate_query_batches =
            checked_reader_counter(self.candidate_query_batches, "candidate query batches")?;
        let conn = self.conn;
        with_length_preflight(conn, || {
            let read = |row: &rusqlite::Row<'_>| {
                Ok(HermesCandidate {
                    phase: HermesPhase::Messages,
                    rowid: row.get(0)?,
                    retained_bytes: row.get(1)?,
                    storage_error_code: row.get(2)?,
                })
            };
            match (self.session_scope.as_deref(), after) {
                (Some(session), Some(rowid)) => self
                    .next_message_candidate
                    .query_map(rusqlite::params![session, rowid], read)?
                    .collect(),
                (Some(session), None) => self
                    .first_message_candidate
                    .query_map([session], read)?
                    .collect(),
                (None, Some(rowid)) => self
                    .next_message_candidate
                    .query_map([rowid], read)?
                    .collect(),
                (None, None) => self.first_message_candidate.query_map([], read)?.collect(),
            }
        })
    }

    fn message_candidates_for_rowids(&mut self, rowids: &[i64]) -> Result<Vec<HermesCandidate>> {
        if rowids.is_empty() {
            return Ok(Vec::new());
        }
        self.candidate_query_batches =
            checked_reader_counter(self.candidate_query_batches, "candidate query batches")?;
        let placeholders = (1..=rowids.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let messages = self.schema.messages();
        let visibility = self.schema.message_visibility();
        let visibility = if visibility.is_empty() {
            String::new()
        } else {
            format!(" and {visibility}")
        };
        let sql = format!(
            "select m.rowid, {}, {} from messages m \
             where m.rowid in ({placeholders}){visibility} order by m.rowid",
            messages.retained_length_expr(),
            messages.storage_class_error_expr(),
        );
        with_length_preflight(self.conn, || {
            let mut statement = self.conn.prepare(&sql)?;
            let candidates = statement
                .query_map(params_from_iter(rowids), |row| {
                    Ok(HermesCandidate {
                        phase: HermesPhase::Messages,
                        rowid: row.get(0)?,
                        retained_bytes: row.get(1)?,
                        storage_error_code: row.get(2)?,
                    })
                })?
                .collect();
            candidates
        })
    }

    fn hydrate_candidates(
        &mut self,
        candidates: Vec<HermesCandidate>,
        first_ordinal: u64,
    ) -> Result<Vec<HermesNativeRow>> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let phase = candidates[0].phase;
        if candidates.iter().any(|candidate| candidate.phase != phase) {
            return Err(CaptureError::SystemInvariant(
                "Hermes native row batch crossed traversal phases",
            ));
        }
        let mut hydratable_rowids = Vec::with_capacity(candidates.len());
        for candidate in &candidates {
            if candidate.requires_hydration()? {
                hydratable_rowids.push(candidate.rowid);
            }
        }
        let mut hydrated = if hydratable_rowids.is_empty() {
            BTreeMap::new()
        } else {
            self.load_values(phase, &hydratable_rowids)?
        };
        candidates
            .into_iter()
            .enumerate()
            .map(|(offset, candidate)| {
                let offset = u64::try_from(offset).map_err(|_| {
                    CaptureError::SystemInvariant("Hermes native row batch ordinal overflowed")
                })?;
                let ordinal =
                    first_ordinal
                        .checked_add(offset)
                        .ok_or(CaptureError::SystemInvariant(
                            "Hermes native row ordinal overflowed",
                        ))?;
                let values = hydrated.remove(&candidate.rowid);
                self.hydrate_candidate(candidate, ordinal, values)
            })
            .collect()
    }

    fn load_values(
        &mut self,
        phase: HermesPhase,
        rowids: &[i64],
    ) -> Result<BTreeMap<i64, Vec<HermesSqliteValue>>> {
        self.hydration_query_batches =
            checked_reader_counter(self.hydration_query_batches, "hydration query batches")?;
        self.max_hydration_rows = self.max_hydration_rows.max(rowids.len() as u64);
        #[cfg(test)]
        match phase {
            HermesPhase::Sessions => self.session_hydration_queries += 1,
            HermesPhase::Messages => self.message_hydration_queries += 1,
        }
        let placeholders = (1..=rowids.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let (table, alias, projection, visibility) = match phase {
            HermesPhase::Sessions => (
                "sessions",
                "s",
                self.schema.sessions().projection(),
                String::new(),
            ),
            HermesPhase::Messages => {
                let visibility = self.schema.message_visibility();
                (
                    "messages",
                    "m",
                    self.schema.messages().projection(),
                    if visibility.is_empty() {
                        String::new()
                    } else {
                        format!(" and {visibility}")
                    },
                )
            }
        };
        let sql = format!(
            "select {alias}.rowid, {projection} from {table} {alias}
             where {alias}.rowid in ({placeholders}){visibility}
             order by {alias}.rowid"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(rowids), |row| {
            let rowid = row.get::<_, i64>(0)?;
            let values = match phase {
                HermesPhase::Sessions => self.schema.sessions().capture_values(row, 1)?,
                HermesPhase::Messages => self.schema.messages().capture_values(row, 1)?,
            };
            Ok((rowid, values))
        })?;
        rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()
            .map_err(CaptureError::from)
    }

    fn hydrate_candidate(
        &self,
        candidate: HermesCandidate,
        ordinal: u64,
        values: Option<Vec<HermesSqliteValue>>,
    ) -> Result<HermesNativeRow> {
        let next_frontier = HermesFrontier {
            phase: candidate.phase,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Hermes native row ordinal overflowed",
            ))?,
            rowid: candidate.rowid,
        };
        let observed_bytes = candidate.observed_bytes()?;
        let locator = HermesLocator {
            phase: candidate.phase,
            rowid: candidate.rowid,
        };
        let hydration_limit_exceeded = observed_bytes > MAX_PROVIDER_SQLITE_VALUE_BYTES;
        let native_page_limit_exceeded = observed_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES;
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
        let values = values.ok_or(CaptureError::SourceChangedDuringCapture)?;
        let record = match candidate.phase {
            HermesPhase::Sessions => {
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
                let row = decode_hermes_message(&self.schema, &values)?;
                let validation = provider_nonnegative_i64_to_u64(row.id, "Hermes message id")
                    .and_then(|_| {
                        provider_required_timestamp_seconds(
                            row.timestamp,
                            "Hermes message timestamp",
                        )
                        .map(|_| ())
                    });
                match validation {
                    Ok(()) => HermesNativeRecord::Message { row, values },
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

fn bounded_candidate_prefix(candidates: Vec<HermesCandidate>) -> Result<Vec<HermesCandidate>> {
    let mut selected = Vec::with_capacity(candidates.len());
    let mut hydrated_bytes = 0_usize;
    for candidate in candidates {
        let candidate_bytes = if candidate.requires_hydration()? {
            candidate.observed_bytes()?
        } else {
            0
        };
        let next = hydrated_bytes.saturating_add(candidate_bytes);
        if !selected.is_empty() && next > NATIVE_INGESTION_PAGE_MAX_BYTES {
            break;
        }
        hydrated_bytes = next;
        selected.push(candidate);
    }
    Ok(selected)
}

fn checked_reader_counter(value: u64, name: &str) -> Result<u64> {
    value
        .checked_add(1)
        .ok_or_else(|| CaptureError::InvalidPayload(format!("Hermes SQLite {name} overflowed")))
}

fn rejection_owned_bytes(reason: &str) -> usize {
    // Ordinal, locator, frontier, record tag, and the length-prefixed reason.
    (8 + 9 + HERMES_FRONTIER_ACCOUNTING_BYTES + 1 + 8).saturating_add(reason.len())
}

fn with_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    let _guard = SqliteLengthPreflightGuard::new(conn)?;
    query().map_err(CaptureError::from)
}

struct HermesCandidate {
    phase: HermesPhase,
    rowid: i64,
    retained_bytes: i64,
    storage_error_code: i64,
}

impl HermesCandidate {
    fn requires_hydration(&self) -> Result<bool> {
        let observed_bytes = self.observed_bytes()?;
        Ok(self.storage_error_code == 0
            && observed_bytes <= MAX_PROVIDER_SQLITE_VALUE_BYTES
            && observed_bytes <= NATIVE_INGESTION_PAGE_MAX_BYTES)
    }

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
