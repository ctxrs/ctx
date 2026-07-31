use std::collections::BTreeSet;

use rusqlite::{params_from_iter, types::Value as SqlValue, Connection};

use crate::provider::sqlite::{
    ensure_sqlite_table_columns, optional_text_column_expr, optional_timestamp_millis_expr,
    sqlite_table_columns, sqlite_table_exists, SqliteLengthPreflightGuard,
};
use crate::{CaptureError, Result};

use super::model::{ConversationRow, LegacyOrderKey, PlatformMessageRow};

const ASTRBOT_SET_READ_ROWS: usize = 256;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AstrBotQueryCounters {
    pub(crate) candidate_set_reads: u64,
    pub(crate) row_set_reads: u64,
    pub(crate) decoded_rows: u64,
}

#[cfg(test)]
thread_local! {
    static ASTRBOT_QUERY_COUNTERS: std::cell::Cell<AstrBotQueryCounters> =
        const { std::cell::Cell::new(AstrBotQueryCounters {
            candidate_set_reads: 0,
            row_set_reads: 0,
            decoded_rows: 0,
        }) };
}

#[cfg(test)]
pub(crate) fn reset_astrbot_query_counters() {
    ASTRBOT_QUERY_COUNTERS.set(AstrBotQueryCounters::default());
}

#[cfg(test)]
pub(crate) fn astrbot_query_counters() -> AstrBotQueryCounters {
    ASTRBOT_QUERY_COUNTERS.get()
}

fn record_candidate_set_read() {
    #[cfg(test)]
    ASTRBOT_QUERY_COUNTERS.with(|slot| {
        let mut counters = slot.get();
        counters.candidate_set_reads += 1;
        slot.set(counters);
    });
}

fn record_row_set_read() {
    #[cfg(test)]
    ASTRBOT_QUERY_COUNTERS.with(|slot| {
        let mut counters = slot.get();
        counters.row_set_reads += 1;
        slot.set(counters);
    });
}

fn record_decoded_row() {
    #[cfg(test)]
    ASTRBOT_QUERY_COUNTERS.with(|slot| {
        let mut counters = slot.get();
        counters.decoded_rows += 1;
        slot.set(counters);
    });
}

pub(super) struct AstrBotSql {
    pub(super) conversation_candidate_initial: String,
    pub(super) conversation_candidate_after: String,
    pub(super) conversation_rows: String,
    pub(super) platform_message_candidate_initial: Option<String>,
    pub(super) platform_message_candidate_after: Option<String>,
    pub(super) platform_message_rows: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RowCandidate {
    pub(super) physical_rowid: i64,
    pub(super) retained_bytes: i64,
    pub(super) legacy_order: LegacyOrderKey,
}

impl RowCandidate {
    pub(super) fn observed_bytes(self) -> Result<u64> {
        u64::try_from(self.retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload(
                "AstrBot retained SQLite byte count must be nonnegative".to_owned(),
            )
        })
    }
}

impl AstrBotSql {
    pub(super) fn new(conn: &Connection) -> Result<Self> {
        let conversation_columns = astrbot_conversation_columns(conn)?;
        let conversation_projection = astrbot_conversation_projection(&conversation_columns);
        let conversation_cte = format!(
            "with projected(physical_rowid, row_id, inner_conversation_id, conversation_id, \
             platform_id, user_id, content, title, persona_id, token_usage, created_at, \
             updated_at) as (select rowid, {conversation_projection} from conversations)"
        );
        let conversation_retained = astrbot_retained_length_expr(&[
            "row_id",
            "inner_conversation_id",
            "conversation_id",
            "platform_id",
            "user_id",
            "content",
            "title",
            "persona_id",
            "token_usage",
            "created_at",
            "updated_at",
        ]);
        let conversation_candidate_initial = format!(
            "{conversation_cte} select p.physical_rowid, {conversation_retained}, \
             p.created_at, p.row_id \
             from projected p order by p.physical_rowid limit ?1"
        );
        let conversation_candidate_after = format!(
            "{conversation_cte} select p.physical_rowid, {conversation_retained}, \
             p.created_at, p.row_id \
             from projected p where p.physical_rowid > ?1 \
             order by p.physical_rowid limit ?2"
        );
        let conversation_rows = format!(
            "{conversation_cte} select physical_rowid, row_id, inner_conversation_id, conversation_id, \
             platform_id, user_id, content, title, persona_id, token_usage, created_at, \
             updated_at from projected"
        );
        let (
            platform_message_candidate_initial,
            platform_message_candidate_after,
            platform_message_rows,
        ) = if sqlite_table_exists(conn, "platform_message_history")? {
            let columns = sqlite_table_columns(conn, "platform_message_history")?;
            let projection = astrbot_platform_message_projection(&columns);
            let cte = format!(
                "with projected(physical_rowid, id, platform_id, user_id, sender_id, \
                     sender_name, content, llm_checkpoint_id, created_at) as ( \
                         select rowid, {projection} from platform_message_history \
                     )"
            );
            let retained = astrbot_retained_length_expr(&[
                "id",
                "platform_id",
                "user_id",
                "sender_id",
                "sender_name",
                "content",
                "llm_checkpoint_id",
                "created_at",
            ]);
            (
                Some(format!(
                    "{cte} select p.physical_rowid, {retained}, p.created_at, p.id \
                         from projected p \
                         order by p.physical_rowid limit ?1"
                )),
                Some(format!(
                    "{cte} select p.physical_rowid, {retained}, p.created_at, p.id \
                         from projected p \
                         where p.physical_rowid > ?1 order by p.physical_rowid limit ?2"
                )),
                Some(format!(
                    "{cte} select physical_rowid, id, platform_id, user_id, sender_id, sender_name, \
                         content, llm_checkpoint_id, created_at from projected \
                         "
                )),
            )
        } else {
            (None, None, None)
        };

        Ok(Self {
            conversation_candidate_initial,
            conversation_candidate_after,
            conversation_rows,
            platform_message_candidate_initial,
            platform_message_candidate_after,
            platform_message_rows,
        })
    }
}

pub(super) fn astrbot_conversation_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, "conversations")? {
        return Err(CaptureError::InvalidPayload(
            "AstrBot data_v4.db is missing required conversations table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "conversations")?;
    ensure_sqlite_table_columns(&columns, "AstrBot conversations table", &["content"])?;
    Ok(columns)
}

fn astrbot_conversation_projection(columns: &BTreeSet<String>) -> String {
    let row_id = if columns.contains("id") {
        "id"
    } else {
        "rowid"
    };
    let inner_conversation_id = optional_text_column_expr(columns, "inner_conversation_id", "NULL");
    let conversation_id = if columns.contains("conversation_id") {
        "CAST(conversation_id AS TEXT)".to_owned()
    } else if columns.contains("inner_conversation_id") {
        "CAST(inner_conversation_id AS TEXT)".to_owned()
    } else {
        "CAST(rowid AS TEXT)".to_owned()
    };
    let platform_id = optional_text_column_expr(columns, "platform_id", "NULL");
    let user_id = optional_text_column_expr(columns, "user_id", "NULL");
    let title = optional_text_column_expr(columns, "title", "NULL");
    let persona_id = optional_text_column_expr(columns, "persona_id", "NULL");
    let token_usage = optional_text_column_expr(columns, "token_usage", "NULL");
    let created_at = optional_timestamp_millis_expr(columns, "created_at", "NULL");
    let updated_at = optional_timestamp_millis_expr(columns, "updated_at", "NULL");
    format!(
        "{row_id}, {inner_conversation_id}, {conversation_id}, {platform_id}, {user_id}, \
         content, {title}, {persona_id}, {token_usage}, {created_at}, {updated_at}"
    )
}

fn astrbot_platform_message_projection(columns: &BTreeSet<String>) -> String {
    let id = if columns.contains("id") {
        "id"
    } else {
        "rowid"
    };
    let platform_id = optional_text_column_expr(columns, "platform_id", "NULL");
    let user_id = optional_text_column_expr(columns, "user_id", "NULL");
    let sender_id = optional_text_column_expr(columns, "sender_id", "NULL");
    let sender_name = optional_text_column_expr(columns, "sender_name", "NULL");
    let content = optional_text_column_expr(columns, "content", "NULL");
    let llm_checkpoint_id = optional_text_column_expr(columns, "llm_checkpoint_id", "NULL");
    let created_at = optional_timestamp_millis_expr(columns, "created_at", "NULL");
    format!(
        "{id}, {platform_id}, {user_id}, {sender_id}, {sender_name}, {content}, \
         {llm_checkpoint_id}, {created_at}"
    )
}

fn astrbot_retained_length_expr(columns: &[&str]) -> String {
    // Keep the size probe on the source column: octet_length() can read the encoded byte count
    // lazily, while casting an oversize value to BLOB can trip SQLite's length limit first.
    columns
        .iter()
        .map(|column| format!("coalesce(octet_length(p.{column}), 0)"))
        .collect::<Vec<_>>()
        .join(" + ")
}

pub(super) fn with_astrbot_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    // SQLITE_LIMIT_LENGTH can reject even integer-only octet_length inspection of an oversized
    // stored value. AstrBot candidate/setup queries return only rowids, order keys, and byte
    // counts, so lift the limit only around metadata preflight and restore it before row reads.
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

pub(super) fn fetch_candidates(
    conn: &Connection,
    initial_sql: &str,
    after_sql: &str,
    after_rowid: Option<i64>,
    limit: usize,
) -> Result<Vec<RowCandidate>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let limit = i64::try_from(limit)
        .map_err(|_| CaptureError::SystemInvariant("AstrBot query page exceeds i64"))?;
    let map_row = |row: &rusqlite::Row<'_>| {
        let physical_rowid = row.get(0)?;
        let timestamp = row.get::<_, Option<i64>>(2)?;
        Ok(RowCandidate {
            physical_rowid,
            retained_bytes: row.get(1)?,
            legacy_order: LegacyOrderKey {
                timestamp_is_present: timestamp.is_some(),
                timestamp: timestamp.unwrap_or(0),
                logical_id: row.get(3)?,
                physical_rowid,
            },
        })
    };
    record_candidate_set_read();
    with_astrbot_length_preflight(conn, || {
        let mut statement = conn.prepare(if after_rowid.is_some() {
            after_sql
        } else {
            initial_sql
        })?;
        let rows = match after_rowid {
            Some(rowid) => statement.query_map((rowid, limit), map_row)?,
            None => statement.query_map([limit], map_row)?,
        };
        rows.collect()
    })
}

pub(super) fn visit_conversations<E>(
    conn: &Connection,
    sql: &str,
    physical_rowids: &[i64],
    mut visit: impl FnMut(i64, ConversationRow) -> std::result::Result<(), E>,
) -> std::result::Result<(), E>
where
    E: From<CaptureError>,
{
    visit_rows(
        conn,
        sql,
        physical_rowids,
        |row| {
            let physical_rowid = row.get(0)?;
            Ok((
                physical_rowid,
                ConversationRow {
                    row_id: row.get(1)?,
                    inner_conversation_id: row.get(2)?,
                    conversation_id: row.get(3)?,
                    platform_id: row.get(4)?,
                    user_id: row.get(5)?,
                    content: row.get(6)?,
                    title: row.get(7)?,
                    persona_id: row.get(8)?,
                    token_usage: row.get(9)?,
                    created_at: row.get(10)?,
                    updated_at: row.get(11)?,
                },
            ))
        },
        &mut visit,
    )
}

pub(super) fn visit_platform_messages<E>(
    conn: &Connection,
    sql: &str,
    physical_rowids: &[i64],
    mut visit: impl FnMut(i64, PlatformMessageRow) -> std::result::Result<(), E>,
) -> std::result::Result<(), E>
where
    E: From<CaptureError>,
{
    visit_rows(
        conn,
        sql,
        physical_rowids,
        |row| {
            let physical_rowid = row.get(0)?;
            Ok((
                physical_rowid,
                PlatformMessageRow {
                    id: row.get(1)?,
                    platform_id: row.get(2)?,
                    user_id: row.get(3)?,
                    sender_id: row.get(4)?,
                    sender_name: row.get(5)?,
                    content: row.get(6)?,
                    llm_checkpoint_id: row.get(7)?,
                    created_at: row.get(8)?,
                },
            ))
        },
        &mut visit,
    )
}

fn visit_rows<T, E>(
    conn: &Connection,
    sql: &str,
    physical_rowids: &[i64],
    mut decode: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<(i64, T)>,
    visit: &mut impl FnMut(i64, T) -> std::result::Result<(), E>,
) -> std::result::Result<(), E>
where
    E: From<CaptureError>,
{
    if physical_rowids.is_empty() {
        return Ok(());
    }
    if physical_rowids.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(E::from(CaptureError::SystemInvariant(
            "AstrBot row set must be strictly ordered",
        )));
    }

    for physical_rowids in physical_rowids.chunks(ASTRBOT_SET_READ_ROWS) {
        record_row_set_read();
        let placeholders = std::iter::repeat_n("?", physical_rowids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query =
            format!("{sql} where physical_rowid in ({placeholders}) order by physical_rowid");
        let parameters = physical_rowids.iter().copied().map(SqlValue::Integer);
        let mut statement = conn.prepare(&query).map_err(CaptureError::from)?;
        let mut rows = statement
            .query(params_from_iter(parameters))
            .map_err(CaptureError::from)?;
        let mut expected = physical_rowids.iter().copied();
        while let Some(row) = rows.next().map_err(CaptureError::from)? {
            let (physical_rowid, value) = decode(row).map_err(CaptureError::from)?;
            if expected.next() != Some(physical_rowid) {
                return Err(E::from(CaptureError::from(
                    rusqlite::Error::QueryReturnedNoRows,
                )));
            }
            record_decoded_row();
            visit(physical_rowid, value)?;
        }
        if expected.next().is_some() {
            return Err(E::from(CaptureError::from(
                rusqlite::Error::QueryReturnedNoRows,
            )));
        }
    }
    Ok(())
}
