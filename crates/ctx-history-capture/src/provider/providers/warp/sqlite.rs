#[cfg(test)]
use std::cell::{Cell, RefCell};

use rusqlite::{Connection, OptionalExtension, Statement};

use super::position::{
    decode_warp_position, encode_warp_position, warp_locator, WarpKeyset, WarpPhase,
};
use crate::captured_batch::sqlite_logical_rows::{SqliteLogicalRow, SqliteLogicalRowsBatchError};
use crate::captured_batch::{
    CapturedSqliteValue, NativePosition, ProviderRecordKind,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
    SqliteLengthPreflightGuard,
};
use crate::{CaptureError, Result};

pub(super) const WARP_CONVERSATION_START_RECORD_KIND: &str = "warp-conversation-start-v2";
pub(super) const WARP_CONVERSATION_OVERSIZE_RECORD_KIND: &str = "warp-conversation-oversize-v2";
pub(super) const WARP_TASK_RECORD_KIND: &str = "warp-task-v2";
pub(super) const WARP_TASK_INVALID_KEY_RECORD_KIND: &str = "warp-task-invalid-key-v2";
pub(super) const WARP_ORDERING_KEY_MAX_BYTES: usize = 240 * 1024;
const WARP_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 64 * 9;
const WARP_CHECKPOINT_TEXT_MAX_BYTES: usize = 8 * 1024;

#[cfg(test)]
thread_local! {
    static WARP_FETCH_TEST_COUNTS: Cell<(usize, usize, usize)> = const { Cell::new((0, 0, 0)) };
    static WARP_TASK_KEY_HYDRATION_TEST_ROWIDS: RefCell<Option<Vec<i64>>> =
        const { RefCell::new(None) };
}

pub(super) struct WarpSqliteSchema {
    task_keyset_index: String,
}

impl WarpSqliteSchema {
    pub(super) fn detect(conn: &Connection) -> Result<Self> {
        Ok(Self {
            task_keyset_index: warp_validate_schema(conn)?,
        })
    }
}

pub(super) struct WarpRowFetcher<'connection> {
    conn: &'connection Connection,
    conversation_candidate: Statement<'connection>,
    conversation_hydration: Statement<'connection>,
    task_first: Statement<'connection>,
    task_next: Statement<'connection>,
    task_hydration: Statement<'connection>,
    conversation_start_kind: ProviderRecordKind,
    conversation_oversize_kind: ProviderRecordKind,
    task_kind: ProviderRecordKind,
    task_invalid_key_kind: ProviderRecordKind,
}

impl<'connection> WarpRowFetcher<'connection> {
    #[cfg(test)]
    pub(super) fn new(conn: &'connection Connection, start: &NativePosition) -> Result<Self> {
        let schema = WarpSqliteSchema::detect(conn)?;
        Self::from_schema(conn, start, &schema)
    }

    pub(super) fn from_schema(
        conn: &'connection Connection,
        _start: &NativePosition,
        schema: &WarpSqliteSchema,
    ) -> Result<Self> {
        let task_keyset_index = warp_quote_identifier(&schema.task_keyset_index);
        #[cfg(test)]
        WARP_FETCH_TEST_COUNTS.with(|counts| {
            let (constructed, conversations, tasks) = counts.get();
            counts.set((constructed.saturating_add(1), conversations, tasks));
        });
        let conversation_bytes = warp_retained_length_expr(&[
            "c.conversation_id",
            "c.conversation_data",
            "c.last_modified_at",
        ]);
        let task_bytes = warp_retained_length_expr(&[
            "t.conversation_id",
            "t.task_id",
            "t.task",
            "t.last_modified_at",
        ]);
        Ok(Self {
            conn,
            conversation_candidate: conn.prepare(&format!(
                "select c.rowid, \
                        coalesce(octet_length(c.last_modified_at), 0) + \
                        coalesce(octet_length(c.conversation_id), 0), \
                        {conversation_bytes}, \
                        case when typeof(c.conversation_id) = 'text' \
                                  and coalesce(octet_length(c.conversation_id), 0) <= ?2 \
                                  and coalesce(octet_length(c.last_modified_at), 0) + \
                                      coalesce(octet_length(c.conversation_id), 0) <= ?3 \
                             then 1 else 0 end \
                 from agent_conversations c \
                 where c.rowid > ?1 \
                 order by c.rowid limit 1"
            ))?,
            conversation_hydration: conn.prepare(
                "select rowid, cast(conversation_id as text), \
                        cast(conversation_data as text), cast(last_modified_at as text) \
                 from agent_conversations where rowid = ?1",
            )?,
            task_first: conn.prepare(&format!(
                "select t.rowid, coalesce(octet_length(t.task_id), 0), \
                        {task_bytes}, \
                        case when typeof(t.task_id) = 'text' \
                                  and coalesce(octet_length(t.task_id), 0) <= ?1 \
                             then 1 else 0 end \
                 from agent_tasks t indexed by {task_keyset_index} \
                 order by t.task_id collate binary limit 1"
            ))?,
            task_next: conn.prepare(&format!(
                "select t.rowid, coalesce(octet_length(t.task_id), 0), \
                        {task_bytes}, \
                        case when typeof(t.task_id) = 'text' \
                                  and coalesce(octet_length(t.task_id), 0) <= ?2 \
                             then 1 else 0 end \
                 from agent_tasks t indexed by {task_keyset_index} \
                 where t.task_id collate binary > ( \
                           select previous.task_id from agent_tasks previous \
                           where previous.rowid = ?1 \
                       ) \
                 order by t.task_id collate binary limit 1"
            ))?,
            task_hydration: conn.prepare(
                "select rowid, cast(conversation_id as text), cast(task_id as text), task, \
                        cast(last_modified_at as text) \
                 from agent_tasks where rowid = ?1",
            )?,
            conversation_start_kind: ProviderRecordKind::new(WARP_CONVERSATION_START_RECORD_KIND)
                .map_err(warp_captured_error)?,
            conversation_oversize_kind: ProviderRecordKind::new(
                WARP_CONVERSATION_OVERSIZE_RECORD_KIND,
            )
            .map_err(warp_captured_error)?,
            task_kind: ProviderRecordKind::new(WARP_TASK_RECORD_KIND)
                .map_err(warp_captured_error)?,
            task_invalid_key_kind: ProviderRecordKind::new(WARP_TASK_INVALID_KEY_RECORD_KIND)
                .map_err(warp_captured_error)?,
        })
    }

    pub(super) fn fetch(&mut self, after: NativePosition) -> Result<Option<SqliteLogicalRow>> {
        match decode_warp_position(&after)? {
            None => self.fetch_next_conversation(None, 0),
            Some(keyset) => match keyset.phase {
                WarpPhase::Conversations => {
                    self.fetch_next_conversation(Some(keyset.rowid), keyset.next_ordinal)
                }
                WarpPhase::Tasks => self.fetch_next_task(keyset.rowid, keyset.next_ordinal),
            },
        }
    }

    fn fetch_next_conversation(
        &mut self,
        after_rowid: Option<i64>,
        ordinal: u64,
    ) -> Result<Option<SqliteLogicalRow>> {
        let key_limit = i64::try_from(WARP_ORDERING_KEY_MAX_BYTES).map_err(|_| {
            CaptureError::SystemInvariant("Warp ordering-key limit exceeds SQLite integer range")
        })?;
        let checkpoint_limit = i64::try_from(WARP_CHECKPOINT_TEXT_MAX_BYTES).map_err(|_| {
            CaptureError::SystemInvariant("Warp checkpoint-text limit exceeds SQLite integer range")
        })?;
        let conn = self.conn;
        let candidate = with_warp_length_preflight(conn, || {
            self.conversation_candidate
                .query_row(
                    rusqlite::params![after_rowid.unwrap_or(i64::MIN), checkpoint_limit, key_limit],
                    warp_candidate_from_row,
                )
                .optional()
        })?;
        match candidate {
            Some(candidate) => self.hydrate_conversation(candidate, ordinal).map(Some),
            None => self.fetch_first_task(ordinal),
        }
    }

    fn fetch_first_task(&mut self, ordinal: u64) -> Result<Option<SqliteLogicalRow>> {
        let key_limit = i64::try_from(WARP_ORDERING_KEY_MAX_BYTES).map_err(|_| {
            CaptureError::SystemInvariant("Warp ordering-key limit exceeds SQLite integer range")
        })?;
        let conn = self.conn;
        with_warp_length_preflight(conn, || {
            self.task_first
                .query_row([key_limit], warp_candidate_from_row)
                .optional()
        })?
        .map_or(Ok(None), |candidate| {
            self.hydrate_task(candidate, ordinal).map(Some)
        })
    }

    fn fetch_next_task(
        &mut self,
        after_rowid: i64,
        ordinal: u64,
    ) -> Result<Option<SqliteLogicalRow>> {
        let key_limit = i64::try_from(WARP_ORDERING_KEY_MAX_BYTES).map_err(|_| {
            CaptureError::SystemInvariant("Warp ordering-key limit exceeds SQLite integer range")
        })?;
        let conn = self.conn;
        with_warp_length_preflight(conn, || {
            self.task_next
                .query_row(
                    rusqlite::params![after_rowid, key_limit],
                    warp_candidate_from_row,
                )
                .optional()
        })?
        .map_or(Ok(None), |candidate| {
            self.hydrate_task(candidate, ordinal).map(Some)
        })
    }

    fn hydrate_conversation(
        &mut self,
        candidate: WarpCandidate,
        ordinal: u64,
    ) -> Result<SqliteLogicalRow> {
        let next_position = encode_warp_position(WarpKeyset {
            phase: WarpPhase::Conversations,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Warp captured row ordinal overflowed",
            ))?,
            rowid: candidate.rowid,
            key_valid: candidate.key_valid,
        })?;
        let locator = warp_locator(WarpPhase::Conversations, candidate.rowid)?;
        let observed_bytes = candidate.observed_bytes()?;
        if !candidate.key_valid || observed_bytes > warp_oversize_limit()? {
            return SqliteLogicalRow::values(
                next_position,
                ordinal,
                locator,
                self.conversation_oversize_kind.clone(),
                vec![
                    CapturedSqliteValue::Integer(candidate.rowid),
                    CapturedSqliteValue::Integer(i64::try_from(observed_bytes).unwrap_or(i64::MAX)),
                ],
            )
            .map_err(warp_captured_error);
        }
        let values = self
            .conversation_hydration
            .query_row([candidate.rowid], warp_conversation_values)?;
        #[cfg(test)]
        WARP_FETCH_TEST_COUNTS.with(|counts| {
            let (constructed, conversations, tasks) = counts.get();
            counts.set((constructed, conversations.saturating_add(1), tasks));
        });
        SqliteLogicalRow::values(
            next_position,
            ordinal,
            locator,
            self.conversation_start_kind.clone(),
            values,
        )
        .map_err(warp_captured_error)
    }

    fn hydrate_task(&mut self, candidate: WarpCandidate, ordinal: u64) -> Result<SqliteLogicalRow> {
        let next_position = encode_warp_position(WarpKeyset {
            phase: WarpPhase::Tasks,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Warp captured row ordinal overflowed",
            ))?,
            rowid: candidate.rowid,
            key_valid: candidate.key_valid,
        })?;
        let locator = warp_locator(WarpPhase::Tasks, candidate.rowid)?;
        let observed_bytes = candidate.observed_bytes()?;
        if !candidate.key_valid {
            return SqliteLogicalRow::values(
                next_position,
                ordinal,
                locator,
                self.task_invalid_key_kind.clone(),
                vec![
                    CapturedSqliteValue::Integer(candidate.rowid),
                    CapturedSqliteValue::Integer(candidate.key_bytes),
                ],
            )
            .map_err(warp_captured_error);
        }
        if observed_bytes > warp_oversize_limit()? {
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                locator,
                self.task_kind.clone(),
                observed_bytes,
            )
            .map_err(warp_captured_error);
        }
        #[cfg(test)]
        warp_trace_task_key_hydration(candidate.rowid);
        let values = self
            .task_hydration
            .query_row([candidate.rowid], warp_task_values)?;
        #[cfg(test)]
        WARP_FETCH_TEST_COUNTS.with(|counts| {
            let (constructed, conversations, tasks) = counts.get();
            counts.set((constructed, conversations, tasks.saturating_add(1)));
        });
        SqliteLogicalRow::values(
            next_position,
            ordinal,
            locator,
            self.task_kind.clone(),
            values,
        )
        .map_err(warp_captured_error)
    }
}

struct WarpCandidate {
    rowid: i64,
    key_bytes: i64,
    retained_bytes: i64,
    key_valid: bool,
}

fn with_warp_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

impl WarpCandidate {
    fn observed_bytes(&self) -> Result<u64> {
        let _key_bytes = u64::try_from(self.key_bytes).map_err(|_| {
            CaptureError::InvalidPayload(
                "Warp SQLite ordering-key byte count must be nonnegative".to_owned(),
            )
        })?;
        let payload = u64::try_from(self.retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload(
                "Warp SQLite retained byte count must be nonnegative".to_owned(),
            )
        })?;
        WARP_SQLITE_VALUE_OVERHEAD_BYTES
            .checked_add(payload)
            .ok_or(CaptureError::SystemInvariant(
                "Warp SQLite retained byte count overflowed",
            ))
    }
}

fn warp_candidate_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WarpCandidate> {
    Ok(WarpCandidate {
        rowid: row.get(0)?,
        key_bytes: row.get(1)?,
        retained_bytes: row.get(2)?,
        key_valid: row.get::<_, i64>(3)? != 0,
    })
}

#[cfg(test)]
pub(super) fn warp_fetch_test_counts() -> (usize, usize, usize) {
    WARP_FETCH_TEST_COUNTS.with(Cell::get)
}

#[cfg(test)]
pub(super) fn warp_reset_fetch_test_counts() {
    WARP_FETCH_TEST_COUNTS.with(|counts| counts.set((0, 0, 0)));
}

#[cfg(test)]
fn warp_trace_task_key_hydration(rowid: i64) {
    WARP_TASK_KEY_HYDRATION_TEST_ROWIDS.with(|trace| {
        if let Some(rowids) = trace.borrow_mut().as_mut() {
            rowids.push(rowid);
        }
    });
}

#[cfg(test)]
pub(super) fn warp_start_task_key_hydration_trace() {
    WARP_TASK_KEY_HYDRATION_TEST_ROWIDS.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
}

#[cfg(test)]
pub(super) fn warp_take_task_key_hydration_trace() -> Vec<i64> {
    WARP_TASK_KEY_HYDRATION_TEST_ROWIDS
        .with(|trace| trace.borrow_mut().take())
        .unwrap_or_default()
}

fn warp_conversation_values(row: &rusqlite::Row<'_>) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    Ok(vec![
        CapturedSqliteValue::Integer(row.get(0)?),
        CapturedSqliteValue::Text(row.get(1)?),
        CapturedSqliteValue::Text(row.get(2)?),
        CapturedSqliteValue::Text(row.get(3)?),
    ])
}

fn warp_task_values(row: &rusqlite::Row<'_>) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    Ok(vec![
        CapturedSqliteValue::Integer(row.get(0)?),
        CapturedSqliteValue::Text(row.get(1)?),
        CapturedSqliteValue::Text(row.get(2)?),
        CapturedSqliteValue::Blob(row.get(3)?),
        CapturedSqliteValue::Text(row.get(4)?),
    ])
}

fn warp_retained_length_expr(expressions: &[&str]) -> String {
    expressions
        .iter()
        .map(|expression| format!("coalesce(octet_length({expression}), 0)"))
        .collect::<Vec<_>>()
        .join(" + ")
}

fn warp_oversize_limit() -> Result<u64> {
    u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("Warp byte limit exceeds u64"))
}

pub(super) fn warp_captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(super) fn warp_sqlite_batch_error(
    error: SqliteLogicalRowsBatchError<CaptureError>,
) -> CaptureError {
    match error {
        SqliteLogicalRowsBatchError::Callback(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn warp_validate_schema(conn: &Connection) -> Result<String> {
    if !sqlite_table_exists(conn, "agent_conversations")? {
        return Err(CaptureError::InvalidPayload(
            "Warp SQLite database is missing required agent_conversations table".into(),
        ));
    }
    let conversation_columns = sqlite_table_columns(conn, "agent_conversations")?;
    ensure_sqlite_table_columns(
        &conversation_columns,
        "Warp agent_conversations table",
        &["conversation_id", "conversation_data", "last_modified_at"],
    )?;
    if !sqlite_table_exists(conn, "agent_tasks")? {
        return Err(CaptureError::InvalidPayload(
            "Warp SQLite database is missing required agent_tasks table".into(),
        ));
    }
    let task_columns = sqlite_table_columns(conn, "agent_tasks")?;
    ensure_sqlite_table_columns(
        &task_columns,
        "Warp agent_tasks table",
        &["conversation_id", "task_id", "task", "last_modified_at"],
    )?;
    warp_task_keyset_index(conn)
}

pub(super) fn warp_task_keyset_index(conn: &Connection) -> Result<String> {
    let task_id_not_null: i64 = conn.query_row(
        "select count(*) from pragma_table_info('agent_tasks') \
         where name = 'task_id' and \"notnull\" = 1",
        [],
        |row| row.get(0),
    )?;
    if task_id_not_null != 1 {
        return Err(CaptureError::InvalidPayload(
            "Warp agent_tasks task_id must be declared NOT NULL for bounded keyset traversal"
                .to_owned(),
        ));
    }

    let mut indexes = conn.prepare(
        "select name, \"unique\", partial from pragma_index_list('agent_tasks') order by seq",
    )?;
    let indexes = indexes
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? != 0,
                row.get::<_, i64>(2)? != 0,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (name, unique, partial) in indexes {
        if !unique || partial {
            continue;
        }
        let mut columns = conn.prepare(
            "select seqno, name, \"desc\", coll from pragma_index_xinfo(?1) \
             where key = 1 order by seqno",
        )?;
        let columns = columns
            .query_map([name.as_str()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let supported = matches!(
            columns.as_slice(),
            [(0, Some(task_id), false, collation)]
                if task_id == "task_id" && collation.eq_ignore_ascii_case("binary")
        );
        if supported {
            return Ok(name);
        }
    }
    Err(CaptureError::InvalidPayload(
        "Warp agent_tasks requires a non-partial ascending UNIQUE BINARY index on task_id for bounded global keyset traversal"
            .to_owned(),
    ))
}

fn warp_quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('\"', "\"\""))
}

#[cfg(test)]
#[path = "sqlite_tests.rs"]
mod tests;
