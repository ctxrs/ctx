use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension};

use crate::captured_batch::{CapturedSqliteValue, CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
};
use crate::{CaptureError, Result};

use super::position::{NanoClawKeyset, NanoClawMessageSource};

const NANOCLAW_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 64 * 32;

#[derive(Debug, Clone)]
pub(super) struct NanoClawSessionRow {
    pub(super) id: String,
    pub(super) agent_group_id: String,
    pub(super) messaging_group_id: Option<String>,
    pub(super) thread_id: Option<String>,
    pub(super) agent_provider: Option<String>,
    pub(super) status: Option<String>,
    pub(super) container_status: Option<String>,
    pub(super) last_active: Option<i64>,
    pub(super) created_at: Option<i64>,
    pub(super) agent_group_name: Option<String>,
    pub(super) agent_group_folder: Option<String>,
    pub(super) messaging_channel_type: Option<String>,
    pub(super) messaging_platform_id: Option<String>,
    pub(super) messaging_instance: Option<String>,
    pub(super) messaging_name: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct NanoClawMessageRow {
    pub(super) source: &'static str,
    pub(super) id: String,
    pub(super) seq: Option<i64>,
    pub(super) kind: Option<String>,
    pub(super) timestamp: Option<i64>,
    pub(super) status: Option<String>,
    pub(super) in_reply_to: Option<String>,
    pub(super) platform_id: Option<String>,
    pub(super) channel_type: Option<String>,
    pub(super) thread_id: Option<String>,
    pub(super) content: Option<String>,
    pub(super) trigger: Option<String>,
    pub(super) source_session_id: Option<String>,
    pub(super) on_wake: Option<i64>,
}

pub(super) struct NanoClawSessionCandidate {
    pub(super) rowid: i64,
    retained_bytes: i64,
}

impl NanoClawSessionCandidate {
    pub(super) fn observed_bytes(&self) -> Result<u64> {
        nanoclaw_observed_bytes(self.retained_bytes)
    }
}

#[derive(Debug, Clone)]
pub(super) struct NanoClawMessageCandidate {
    pub(super) source: NanoClawMessageSource,
    pub(super) rowid: i64,
    pub(super) timestamp: i64,
    pub(super) seq: i64,
    retained_bytes: i64,
}

impl NanoClawMessageCandidate {
    pub(super) fn observed_bytes(&self, session_bytes: u64) -> Result<u64> {
        nanoclaw_observed_bytes(self.retained_bytes)?
            .checked_add(session_bytes)
            .ok_or(CaptureError::SystemInvariant(
                "NanoClaw joined logical-row byte count overflowed",
            ))
    }
}

#[derive(Clone, Copy)]
pub(super) struct NanoClawMessageAfter {
    timestamp: i64,
    seq: i64,
    source: NanoClawMessageSource,
    rowid: i64,
}

pub(super) fn nanoclaw_session_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, "sessions")? {
        return Err(CaptureError::InvalidPayload(
            "NanoClaw data/v2.db is missing required sessions table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "sessions")?;
    ensure_sqlite_table_columns(
        &columns,
        "NanoClaw sessions table",
        &["id", "agent_group_id"],
    )?;
    Ok(columns)
}

pub(super) fn nanoclaw_session_projection(
    conn: &Connection,
    columns: &BTreeSet<String>,
) -> Result<Vec<String>> {
    let agent_group_columns = if sqlite_table_exists(conn, "agent_groups")? {
        sqlite_table_columns(conn, "agent_groups")?
    } else {
        BTreeSet::new()
    };
    let messaging_columns = if columns.contains("messaging_group_id")
        && sqlite_table_exists(conn, "messaging_groups")?
    {
        sqlite_table_columns(conn, "messaging_groups")?
    } else {
        BTreeSet::new()
    };
    Ok(vec![
        "CAST(s.id AS TEXT)".to_owned(),
        "CAST(s.agent_group_id AS TEXT)".to_owned(),
        nanoclaw_qualified_optional(columns, "s", "messaging_group_id", "NULL"),
        nanoclaw_qualified_optional(columns, "s", "thread_id", "NULL"),
        nanoclaw_qualified_optional(columns, "s", "agent_provider", "NULL"),
        nanoclaw_qualified_optional(columns, "s", "status", "NULL"),
        nanoclaw_qualified_optional(columns, "s", "container_status", "NULL"),
        nanoclaw_qualified_timestamp(columns, "s", "last_active"),
        nanoclaw_qualified_timestamp(columns, "s", "created_at"),
        if agent_group_columns.contains("id") && agent_group_columns.contains("name") {
            "(select name from agent_groups where agent_groups.id = s.agent_group_id)".to_owned()
        } else {
            "NULL".to_owned()
        },
        if agent_group_columns.contains("id") && agent_group_columns.contains("folder") {
            "(select folder from agent_groups where agent_groups.id = s.agent_group_id)".to_owned()
        } else {
            "NULL".to_owned()
        },
        nanoclaw_messaging_projection(&messaging_columns, "channel_type"),
        nanoclaw_messaging_projection(&messaging_columns, "platform_id"),
        nanoclaw_messaging_projection(&messaging_columns, "instance"),
        nanoclaw_messaging_projection(&messaging_columns, "name"),
    ])
}

fn nanoclaw_qualified_optional(
    columns: &BTreeSet<String>,
    alias: &str,
    column: &str,
    fallback: &str,
) -> String {
    if columns.contains(column) {
        format!("{alias}.{column}")
    } else {
        fallback.to_owned()
    }
}

fn nanoclaw_qualified_timestamp(columns: &BTreeSet<String>, alias: &str, column: &str) -> String {
    if !columns.contains(column) {
        return "NULL".to_owned();
    }
    let qualified = format!("{alias}.{column}");
    let text = format!("trim(CAST({qualified} AS TEXT))");
    let numeric_body = format!(
        "CASE WHEN substr({text}, 1, 1) IN ('+', '-') THEN substr({text}, 2) ELSE {text} END"
    );
    let numeric_value = format!(
        "CASE WHEN abs(CAST({qualified} AS REAL)) < 100000000000 \
         THEN CAST(ROUND(CAST({qualified} AS REAL) * 1000) AS INTEGER) \
         ELSE CAST(ROUND(CAST({qualified} AS REAL)) AS INTEGER) END"
    );
    format!(
        "CASE WHEN {qualified} IS NULL THEN NULL \
         WHEN typeof({qualified}) IN ('integer', 'real') THEN {numeric_value} \
         WHEN {numeric_body} != '' AND {numeric_body} != '.' \
              AND {numeric_body} NOT GLOB '*[^0-9.]*' \
              AND length({numeric_body}) - length(replace({numeric_body}, '.', '')) <= 1 \
         THEN {numeric_value} \
         ELSE CAST(ROUND(unixepoch({qualified}, 'subsec') * 1000) AS INTEGER) END"
    )
}

fn nanoclaw_messaging_projection(columns: &BTreeSet<String>, column: &str) -> String {
    if columns.contains("id") && columns.contains(column) {
        format!(
            "(select {column} from messaging_groups where messaging_groups.id = s.messaging_group_id)"
        )
    } else {
        "NULL".to_owned()
    }
}

pub(super) fn nanoclaw_retained_length_expr(expressions: &[String]) -> String {
    // Unlike a cast to BLOB, octet_length can inspect large TEXT/BLOB columns without
    // materializing them through the bounded connection's SQLITE_LIMIT_LENGTH.
    expressions
        .iter()
        .map(|expression| format!("coalesce(octet_length({expression}), 0)"))
        .collect::<Vec<_>>()
        .join(" + ")
}

pub(super) fn nanoclaw_fetch_session_candidate(
    conn: &Connection,
    columns: &BTreeSet<String>,
    after_rowid: Option<i64>,
) -> Result<Option<NanoClawSessionCandidate>> {
    let retained = nanoclaw_retained_length_expr(&nanoclaw_session_projection(conn, columns)?);
    let (has_after, after_rowid) = after_rowid.map_or((0_i64, 0_i64), |rowid| (1, rowid));
    conn.query_row(
        &format!(
            "select s.rowid, {retained} from sessions s \
             where (?1 = 0 or s.rowid > ?2) order by s.rowid limit 1"
        ),
        [has_after, after_rowid],
        |row| {
            Ok(NanoClawSessionCandidate {
                rowid: row.get(0)?,
                retained_bytes: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(CaptureError::from)
}

pub(super) fn nanoclaw_session_candidate_by_rowid(
    conn: &Connection,
    columns: &BTreeSet<String>,
    rowid: i64,
) -> Result<Option<NanoClawSessionCandidate>> {
    let retained = nanoclaw_retained_length_expr(&nanoclaw_session_projection(conn, columns)?);
    conn.query_row(
        &format!("select rowid, {retained} from sessions s where rowid = ?1"),
        [rowid],
        |row| {
            Ok(NanoClawSessionCandidate {
                rowid: row.get(0)?,
                retained_bytes: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(CaptureError::from)
}

pub(super) fn nanoclaw_hydrate_session(
    conn: &Connection,
    columns: &BTreeSet<String>,
    rowid: i64,
) -> Result<(NanoClawSessionRow, Vec<CapturedSqliteValue>)> {
    let projection = nanoclaw_session_projection(conn, columns)?.join(", ");
    let values = conn.query_row(
        &format!("select {projection} from sessions s where s.rowid = ?1"),
        [rowid],
        nanoclaw_session_values_from_row,
    )?;
    let row = decode_nanoclaw_session(&values)?;
    Ok((row, values))
}

fn nanoclaw_session_values_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    Ok(vec![
        CapturedSqliteValue::Text(row.get(0)?),
        CapturedSqliteValue::Text(row.get(1)?),
        nanoclaw_row_optional_text(row, 2)?,
        nanoclaw_row_optional_text(row, 3)?,
        nanoclaw_row_optional_text(row, 4)?,
        nanoclaw_row_optional_text(row, 5)?,
        nanoclaw_row_optional_text(row, 6)?,
        nanoclaw_row_optional_i64(row, 7)?,
        nanoclaw_row_optional_i64(row, 8)?,
        nanoclaw_row_optional_text(row, 9)?,
        nanoclaw_row_optional_text(row, 10)?,
        nanoclaw_row_optional_text(row, 11)?,
        nanoclaw_row_optional_text(row, 12)?,
        nanoclaw_row_optional_text(row, 13)?,
        nanoclaw_row_optional_text(row, 14)?,
    ])
}

pub(super) fn nanoclaw_session_captured_values(
    row: &NanoClawSessionRow,
) -> Vec<CapturedSqliteValue> {
    vec![
        CapturedSqliteValue::Text(row.id.clone()),
        CapturedSqliteValue::Text(row.agent_group_id.clone()),
        nanoclaw_optional_text_value(row.messaging_group_id.clone()),
        nanoclaw_optional_text_value(row.thread_id.clone()),
        nanoclaw_optional_text_value(row.agent_provider.clone()),
        nanoclaw_optional_text_value(row.status.clone()),
        nanoclaw_optional_text_value(row.container_status.clone()),
        nanoclaw_optional_i64_value(row.last_active),
        nanoclaw_optional_i64_value(row.created_at),
        nanoclaw_optional_text_value(row.agent_group_name.clone()),
        nanoclaw_optional_text_value(row.agent_group_folder.clone()),
        nanoclaw_optional_text_value(row.messaging_channel_type.clone()),
        nanoclaw_optional_text_value(row.messaging_platform_id.clone()),
        nanoclaw_optional_text_value(row.messaging_instance.clone()),
        nanoclaw_optional_text_value(row.messaging_name.clone()),
    ]
}

pub(super) fn nanoclaw_message_after(
    conn: &Connection,
    columns: &BTreeSet<String>,
    source: NanoClawMessageSource,
    keyset: NanoClawKeyset,
) -> Result<NanoClawMessageAfter> {
    let timestamp = nanoclaw_message_timestamp_expr(columns, "m");
    let seq = nanoclaw_qualified_optional(columns, "m", "seq", "NULL");
    conn.query_row(
        &format!(
            "select coalesce({timestamp}, 0), coalesce({seq}, 0) \
             from {} m where m.rowid = ?1",
            source.table()
        ),
        [keyset.message_rowid],
        |row| {
            Ok(NanoClawMessageAfter {
                timestamp: row.get(0)?,
                seq: row.get(1)?,
                source,
                rowid: keyset.message_rowid,
            })
        },
    )
    .optional()?
    .ok_or(CaptureError::SourceChangedDuringCapture)
}

pub(super) fn nanoclaw_fetch_message_candidate(
    conn: &Connection,
    columns: &BTreeSet<String>,
    source: NanoClawMessageSource,
    after: Option<NanoClawMessageAfter>,
) -> Result<Option<NanoClawMessageCandidate>> {
    let timestamp = nanoclaw_message_timestamp_expr(columns, "m");
    let seq = nanoclaw_qualified_optional(columns, "m", "seq", "NULL");
    let retained = nanoclaw_retained_length_expr(&nanoclaw_message_projection(source, columns));
    let (has_after, after_timestamp, after_seq, after_source, after_rowid) =
        after.map_or((0_i64, 0_i64, 0_i64, 0_i64, 0_i64), |after| {
            (
                1,
                after.timestamp,
                after.seq,
                i64::from(after.source.tag()),
                after.rowid,
            )
        });
    let source_tag = i64::from(source.tag());
    let table = source.table();
    conn.query_row(
        &format!(
            "select m.rowid, coalesce({timestamp}, 0), coalesce({seq}, 0), {retained} \
             from {table} m where ?1 = 0 \
                or coalesce({timestamp}, 0) > ?2 \
                or (coalesce({timestamp}, 0) = ?2 and coalesce({seq}, 0) > ?3) \
                or (coalesce({timestamp}, 0) = ?2 and coalesce({seq}, 0) = ?3 and ( \
                    ?4 < ?5 or (?4 = ?5 and ( \
                        CAST(m.id AS TEXT) > (select CAST(a.id AS TEXT) from {table} a where a.rowid = ?6) \
                        or (CAST(m.id AS TEXT) = (select CAST(a.id AS TEXT) from {table} a where a.rowid = ?6) \
                            and m.rowid > ?6) \
                    )) \
                )) \
             order by coalesce({timestamp}, 0), coalesce({seq}, 0), CAST(m.id AS TEXT), m.rowid \
             limit 1"
        ),
        rusqlite::params![
            has_after,
            after_timestamp,
            after_seq,
            after_source,
            source_tag,
            after_rowid,
        ],
        |row| {
            Ok(NanoClawMessageCandidate {
                source,
                rowid: row.get(0)?,
                timestamp: row.get(1)?,
                seq: row.get(2)?,
                retained_bytes: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(CaptureError::from)
}

pub(super) fn nanoclaw_message_candidate_key(
    candidate: &NanoClawMessageCandidate,
) -> (i64, i64, NanoClawMessageSource) {
    (candidate.timestamp, candidate.seq, candidate.source)
}

fn nanoclaw_message_projection(
    source: NanoClawMessageSource,
    columns: &BTreeSet<String>,
) -> Vec<String> {
    let status = if source == NanoClawMessageSource::Inbound {
        nanoclaw_qualified_optional(columns, "m", "status", "NULL")
    } else {
        "NULL".to_owned()
    };
    let in_reply_to = if source == NanoClawMessageSource::Outbound {
        nanoclaw_qualified_optional(columns, "m", "in_reply_to", "NULL")
    } else {
        "NULL".to_owned()
    };
    let trigger = if source == NanoClawMessageSource::Inbound {
        nanoclaw_qualified_optional_text(columns, "m", "trigger")
    } else {
        "NULL".to_owned()
    };
    let source_session_id = if source == NanoClawMessageSource::Inbound {
        nanoclaw_qualified_optional(columns, "m", "source_session_id", "NULL")
    } else {
        "NULL".to_owned()
    };
    let on_wake = if source == NanoClawMessageSource::Inbound {
        nanoclaw_qualified_optional(columns, "m", "on_wake", "NULL")
    } else {
        "NULL".to_owned()
    };
    vec![
        "CAST(m.id AS TEXT)".to_owned(),
        nanoclaw_qualified_optional(columns, "m", "seq", "NULL"),
        nanoclaw_qualified_optional(columns, "m", "kind", "NULL"),
        nanoclaw_message_timestamp_expr(columns, "m"),
        status,
        in_reply_to,
        nanoclaw_qualified_optional(columns, "m", "platform_id", "NULL"),
        nanoclaw_qualified_optional(columns, "m", "channel_type", "NULL"),
        nanoclaw_qualified_optional(columns, "m", "thread_id", "NULL"),
        nanoclaw_qualified_optional(columns, "m", "content", "NULL"),
        trigger,
        source_session_id,
        on_wake,
    ]
}

fn nanoclaw_qualified_optional_text(
    columns: &BTreeSet<String>,
    alias: &str,
    column: &str,
) -> String {
    if columns.contains(column) {
        format!("CAST({alias}.{column} AS TEXT)")
    } else {
        "NULL".to_owned()
    }
}

fn nanoclaw_message_timestamp_expr(columns: &BTreeSet<String>, alias: &str) -> String {
    nanoclaw_qualified_timestamp(columns, alias, "timestamp")
}

pub(super) fn nanoclaw_hydrate_message(
    conn: &Connection,
    columns: &BTreeSet<String>,
    source: NanoClawMessageSource,
    rowid: i64,
) -> Result<Vec<CapturedSqliteValue>> {
    let projection = nanoclaw_message_projection(source, columns).join(", ");
    let mut values = conn.query_row(
        &format!(
            "select {projection} from {} m where m.rowid = ?1",
            source.table()
        ),
        [rowid],
        nanoclaw_message_values_from_row,
    )?;
    values.insert(0, CapturedSqliteValue::Text(source.label().to_owned()));
    Ok(values)
}

fn nanoclaw_message_values_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    Ok(vec![
        CapturedSqliteValue::Text(row.get(0)?),
        nanoclaw_row_optional_i64(row, 1)?,
        nanoclaw_row_optional_text(row, 2)?,
        nanoclaw_row_optional_i64(row, 3)?,
        nanoclaw_row_optional_text(row, 4)?,
        nanoclaw_row_optional_text(row, 5)?,
        nanoclaw_row_optional_text(row, 6)?,
        nanoclaw_row_optional_text(row, 7)?,
        nanoclaw_row_optional_text(row, 8)?,
        nanoclaw_row_optional_text(row, 9)?,
        nanoclaw_row_optional_text(row, 10)?,
        nanoclaw_row_optional_text(row, 11)?,
        nanoclaw_row_optional_i64(row, 12)?,
    ])
}

pub(super) fn decode_nanoclaw_message_record(
    values: &[CapturedSqliteValue],
) -> Result<(NanoClawMessageRow, NanoClawSessionRow)> {
    if values.len() != 29 {
        return Err(CaptureError::SystemInvariant(
            "NanoClaw message logical row has an invalid value shape",
        ));
    }
    let source = nanoclaw_required_text(&values[0])?;
    let source = match source.as_str() {
        "inbound" => "inbound",
        "outbound" => "outbound",
        _ => {
            return Err(CaptureError::SystemInvariant(
                "NanoClaw message logical row has an invalid source",
            ));
        }
    };
    let message = NanoClawMessageRow {
        source,
        id: nanoclaw_required_text(&values[1])?,
        seq: nanoclaw_optional_i64(&values[2])?,
        kind: nanoclaw_optional_text(&values[3])?,
        timestamp: nanoclaw_optional_i64(&values[4])?,
        status: nanoclaw_optional_text(&values[5])?,
        in_reply_to: nanoclaw_optional_text(&values[6])?,
        platform_id: nanoclaw_optional_text(&values[7])?,
        channel_type: nanoclaw_optional_text(&values[8])?,
        thread_id: nanoclaw_optional_text(&values[9])?,
        content: nanoclaw_optional_text(&values[10])?,
        trigger: nanoclaw_optional_text(&values[11])?,
        source_session_id: nanoclaw_optional_text(&values[12])?,
        on_wake: nanoclaw_optional_i64(&values[13])?,
    };
    Ok((message, decode_nanoclaw_session(&values[14..])?))
}

pub(super) fn decode_nanoclaw_session(
    values: &[CapturedSqliteValue],
) -> Result<NanoClawSessionRow> {
    if values.len() != 15 {
        return Err(CaptureError::SystemInvariant(
            "NanoClaw session logical row has an invalid value shape",
        ));
    }
    Ok(NanoClawSessionRow {
        id: nanoclaw_required_text(&values[0])?,
        agent_group_id: nanoclaw_required_text(&values[1])?,
        messaging_group_id: nanoclaw_optional_text(&values[2])?,
        thread_id: nanoclaw_optional_text(&values[3])?,
        agent_provider: nanoclaw_optional_text(&values[4])?,
        status: nanoclaw_optional_text(&values[5])?,
        container_status: nanoclaw_optional_text(&values[6])?,
        last_active: nanoclaw_optional_i64(&values[7])?,
        created_at: nanoclaw_optional_i64(&values[8])?,
        agent_group_name: nanoclaw_optional_text(&values[9])?,
        agent_group_folder: nanoclaw_optional_text(&values[10])?,
        messaging_channel_type: nanoclaw_optional_text(&values[11])?,
        messaging_platform_id: nanoclaw_optional_text(&values[12])?,
        messaging_instance: nanoclaw_optional_text(&values[13])?,
        messaging_name: nanoclaw_optional_text(&values[14])?,
    })
}

fn nanoclaw_row_optional_text(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<CapturedSqliteValue> {
    Ok(match row.get::<_, Option<String>>(index)? {
        Some(value) => CapturedSqliteValue::Text(value),
        None => CapturedSqliteValue::Null,
    })
}

fn nanoclaw_row_optional_i64(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<CapturedSqliteValue> {
    Ok(match row.get::<_, Option<i64>>(index)? {
        Some(value) => CapturedSqliteValue::Integer(value),
        None => CapturedSqliteValue::Null,
    })
}

fn nanoclaw_optional_text_value(value: Option<String>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Text)
}

fn nanoclaw_optional_i64_value(value: Option<i64>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Integer)
}

fn nanoclaw_required_text(value: &CapturedSqliteValue) -> Result<String> {
    match value {
        CapturedSqliteValue::Text(value) => Ok(value.clone()),
        _ => Err(CaptureError::SystemInvariant(
            "NanoClaw logical row has an invalid required text value",
        )),
    }
}

fn nanoclaw_optional_text(value: &CapturedSqliteValue) -> Result<Option<String>> {
    match value {
        CapturedSqliteValue::Null => Ok(None),
        CapturedSqliteValue::Text(value) => Ok(Some(value.clone())),
        _ => Err(CaptureError::SystemInvariant(
            "NanoClaw logical row has an invalid optional text value",
        )),
    }
}

fn nanoclaw_optional_i64(value: &CapturedSqliteValue) -> Result<Option<i64>> {
    match value {
        CapturedSqliteValue::Null => Ok(None),
        CapturedSqliteValue::Integer(value) => Ok(Some(*value)),
        _ => Err(CaptureError::SystemInvariant(
            "NanoClaw logical row has an invalid optional integer value",
        )),
    }
}

pub(super) fn nanoclaw_observed_bytes(retained_bytes: i64) -> Result<u64> {
    let payload = u64::try_from(retained_bytes).map_err(|_| {
        CaptureError::InvalidPayload(
            "NanoClaw SQLite retained byte count must be nonnegative".to_owned(),
        )
    })?;
    NANOCLAW_SQLITE_VALUE_OVERHEAD_BYTES
        .checked_add(payload)
        .ok_or(CaptureError::SystemInvariant(
            "NanoClaw SQLite retained byte count overflowed",
        ))
}

pub(super) fn nanoclaw_oversize_limit() -> Result<u64> {
    u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("NanoClaw byte limit exceeds u64"))
}
