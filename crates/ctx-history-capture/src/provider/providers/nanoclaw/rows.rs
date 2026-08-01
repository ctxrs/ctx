use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
};
use crate::{CaptureError, Result};

use super::position::{NanoClawFrontier, NanoClawMessageSource};

const NANOCLAW_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 64 * 32;
pub(super) const NANOCLAW_NATIVE_MAX_RECORD_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    storage_classes: Vec<String>,
}

impl NanoClawSessionCandidate {
    pub(super) fn observed_bytes(&self) -> Result<u64> {
        nanoclaw_observed_bytes(self.retained_bytes)
    }

    pub(super) fn rejection_reason(&self) -> Option<&'static str> {
        let storage = self.storage_classes.as_slice();
        if storage.len() != 15 {
            return Some("NanoClaw session row has an invalid preflight shape");
        }
        let required_text = |kind: &str| matches!(kind, "integer" | "real" | "text");
        let optional_text = |kind: &str| matches!(kind, "null" | "text");
        let optional_timestamp = |kind: &str| matches!(kind, "null" | "integer" | "real" | "text");
        if !required_text(&storage[0]) || !required_text(&storage[1]) {
            Some("NanoClaw session identifier has an unsupported SQLite storage class")
        } else if storage[2..7].iter().any(|kind| !optional_text(kind))
            || storage[9..].iter().any(|kind| !optional_text(kind))
        {
            Some("NanoClaw session text field has an unsupported SQLite storage class")
        } else if storage[7..9].iter().any(|kind| !optional_timestamp(kind)) {
            Some("NanoClaw session timestamp has an unsupported SQLite storage class")
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct NanoClawMessageCandidate {
    pub(super) source: NanoClawMessageSource,
    pub(super) rowid: i64,
    pub(super) timestamp: i64,
    pub(super) seq: i64,
    retained_bytes: i64,
    storage_classes: Vec<String>,
}

impl NanoClawMessageCandidate {
    pub(super) fn observed_bytes(&self, session_bytes: u64) -> Result<u64> {
        nanoclaw_observed_bytes(self.retained_bytes)?
            .checked_add(session_bytes)
            .ok_or(CaptureError::SystemInvariant(
                "NanoClaw joined logical-row byte count overflowed",
            ))
    }

    pub(super) fn rejection_reason(&self) -> Option<&'static str> {
        let storage = self.storage_classes.as_slice();
        if storage.len() != 13 {
            return Some("NanoClaw message row has an invalid preflight shape");
        }
        let required_text = |kind: &str| matches!(kind, "integer" | "real" | "text");
        let optional_text = |kind: &str| matches!(kind, "null" | "text");
        let optional_timestamp = |kind: &str| matches!(kind, "null" | "integer" | "real" | "text");
        let optional_castable_text =
            |kind: &str| matches!(kind, "null" | "integer" | "real" | "text");
        if !required_text(&storage[0]) {
            Some("NanoClaw message identifier has an unsupported SQLite storage class")
        } else if !matches!(storage[1].as_str(), "null" | "integer") {
            Some("NanoClaw message seq has an unsupported SQLite storage class")
        } else if !optional_text(&storage[2])
            || storage[4..10].iter().any(|kind| !optional_text(kind))
            || !optional_text(&storage[11])
        {
            Some("NanoClaw message text field has an unsupported SQLite storage class")
        } else if !optional_timestamp(&storage[3]) {
            Some("NanoClaw message timestamp has an unsupported SQLite storage class")
        } else if !optional_castable_text(&storage[10]) {
            Some("NanoClaw message trigger has an unsupported SQLite storage class")
        } else if !matches!(storage[12].as_str(), "null" | "integer") {
            Some("NanoClaw message on_wake has an unsupported SQLite storage class")
        } else {
            None
        }
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
        nanoclaw_required_text_projection("s.id"),
        nanoclaw_required_text_projection("s.agent_group_id"),
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

fn nanoclaw_required_text_projection(qualified: &str) -> String {
    format!(
        "CASE WHEN typeof({qualified}) IN ('integer', 'real', 'text') \
         THEN CAST({qualified} AS TEXT) ELSE '' END"
    )
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
    let projection = nanoclaw_session_projection(conn, columns)?;
    let retained = nanoclaw_retained_length_expr(&projection);
    let storage_classes = nanoclaw_session_storage_classes(columns, &projection).join(", ");
    let (has_after, after_rowid) = after_rowid.map_or((0_i64, 0_i64), |rowid| (1, rowid));
    conn.query_row(
        &format!(
            "select s.rowid, {retained}, {storage_classes} from sessions s \
             where (?1 = 0 or s.rowid > ?2) order by s.rowid limit 1"
        ),
        [has_after, after_rowid],
        |row| {
            Ok(NanoClawSessionCandidate {
                rowid: row.get(0)?,
                retained_bytes: row.get(1)?,
                storage_classes: (2..17)
                    .map(|index| row.get(index))
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            })
        },
    )
    .optional()
    .map_err(CaptureError::from)
}

fn nanoclaw_session_storage_classes(
    columns: &BTreeSet<String>,
    projection: &[String],
) -> Vec<String> {
    projection
        .iter()
        .enumerate()
        .map(|(index, expression)| match index {
            0 => "typeof(s.id)".to_owned(),
            1 => "typeof(s.agent_group_id)".to_owned(),
            7 => nanoclaw_qualified_type(columns, "s", "last_active"),
            8 => nanoclaw_qualified_type(columns, "s", "created_at"),
            _ => format!("typeof({expression})"),
        })
        .collect()
}

pub(super) fn nanoclaw_hydrate_native_session(
    conn: &Connection,
    columns: &BTreeSet<String>,
    rowid: i64,
) -> Result<NanoClawSessionRow> {
    let projection = nanoclaw_session_projection(conn, columns)?.join(", ");
    conn.query_row(
        &format!("select {projection} from sessions s where s.rowid = ?1"),
        [rowid],
        |row| nanoclaw_session_from_row(row, 0),
    )
    .map_err(CaptureError::from)
}

fn nanoclaw_session_from_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<NanoClawSessionRow> {
    Ok(NanoClawSessionRow {
        id: row.get(offset)?,
        agent_group_id: row.get(offset + 1)?,
        messaging_group_id: row.get(offset + 2)?,
        thread_id: row.get(offset + 3)?,
        agent_provider: row.get(offset + 4)?,
        status: row.get(offset + 5)?,
        container_status: row.get(offset + 6)?,
        last_active: row.get(offset + 7)?,
        created_at: row.get(offset + 8)?,
        agent_group_name: row.get(offset + 9)?,
        agent_group_folder: row.get(offset + 10)?,
        messaging_channel_type: row.get(offset + 11)?,
        messaging_platform_id: row.get(offset + 12)?,
        messaging_instance: row.get(offset + 13)?,
        messaging_name: row.get(offset + 14)?,
    })
}

pub(super) fn nanoclaw_message_after(
    conn: &Connection,
    columns: &BTreeSet<String>,
    source: NanoClawMessageSource,
    keyset: NanoClawFrontier,
) -> Result<NanoClawMessageAfter> {
    let timestamp = nanoclaw_message_timestamp_expr(columns, "m");
    let seq = nanoclaw_message_seq_sort_expr(columns, "m");
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
    let seq = nanoclaw_message_seq_sort_expr(columns, "m");
    let projection = nanoclaw_message_projection(source, columns);
    let retained = nanoclaw_retained_length_expr(&projection);
    let storage_classes = nanoclaw_message_storage_classes(source, columns, &projection).join(", ");
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
            "select m.rowid, coalesce({timestamp}, 0), {seq}, {retained}, {storage_classes} \
             from {table} m where ?1 = 0 \
                or coalesce({timestamp}, 0) > ?2 \
                or (coalesce({timestamp}, 0) = ?2 and {seq} > ?3) \
                or (coalesce({timestamp}, 0) = ?2 and {seq} = ?3 and ( \
                    ?4 < ?5 or (?4 = ?5 and ( \
                        CAST(m.id AS TEXT) > (select CAST(a.id AS TEXT) from {table} a where a.rowid = ?6) \
                        or (CAST(m.id AS TEXT) = (select CAST(a.id AS TEXT) from {table} a where a.rowid = ?6) \
                            and m.rowid > ?6) \
                    )) \
                )) \
             order by coalesce({timestamp}, 0), {seq}, CAST(m.id AS TEXT), m.rowid \
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
                storage_classes: (4..17)
                    .map(|index| row.get(index))
                    .collect::<rusqlite::Result<Vec<_>>>()?,
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

fn nanoclaw_message_storage_classes(
    source: NanoClawMessageSource,
    columns: &BTreeSet<String>,
    projection: &[String],
) -> Vec<String> {
    let raw = [
        nanoclaw_qualified_type(columns, "m", "id"),
        nanoclaw_qualified_type(columns, "m", "seq"),
        nanoclaw_qualified_type(columns, "m", "kind"),
        nanoclaw_qualified_type(columns, "m", "timestamp"),
        if source == NanoClawMessageSource::Inbound {
            nanoclaw_qualified_type(columns, "m", "status")
        } else {
            "typeof(NULL)".to_owned()
        },
        if source == NanoClawMessageSource::Outbound {
            nanoclaw_qualified_type(columns, "m", "in_reply_to")
        } else {
            "typeof(NULL)".to_owned()
        },
        nanoclaw_qualified_type(columns, "m", "platform_id"),
        nanoclaw_qualified_type(columns, "m", "channel_type"),
        nanoclaw_qualified_type(columns, "m", "thread_id"),
        nanoclaw_qualified_type(columns, "m", "content"),
        if source == NanoClawMessageSource::Inbound {
            nanoclaw_qualified_type(columns, "m", "trigger")
        } else {
            "typeof(NULL)".to_owned()
        },
        if source == NanoClawMessageSource::Inbound {
            nanoclaw_qualified_type(columns, "m", "source_session_id")
        } else {
            "typeof(NULL)".to_owned()
        },
        if source == NanoClawMessageSource::Inbound {
            nanoclaw_qualified_type(columns, "m", "on_wake")
        } else {
            "typeof(NULL)".to_owned()
        },
    ];
    debug_assert_eq!(projection.len(), raw.len());
    raw.into_iter().collect()
}

fn nanoclaw_qualified_type(columns: &BTreeSet<String>, alias: &str, column: &str) -> String {
    if columns.contains(column) {
        format!("typeof({alias}.{column})")
    } else {
        "typeof(NULL)".to_owned()
    }
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

fn nanoclaw_message_seq_sort_expr(columns: &BTreeSet<String>, alias: &str) -> String {
    if columns.contains("seq") {
        format!("CASE WHEN typeof({alias}.seq) = 'integer' THEN {alias}.seq ELSE 0 END")
    } else {
        "0".to_owned()
    }
}

pub(super) fn nanoclaw_hydrate_native_message(
    conn: &Connection,
    columns: &BTreeSet<String>,
    source: NanoClawMessageSource,
    rowid: i64,
) -> Result<NanoClawMessageRow> {
    let projection = nanoclaw_message_projection(source, columns).join(", ");
    conn.query_row(
        &format!(
            "select {projection} from {} m where m.rowid = ?1",
            source.table()
        ),
        [rowid],
        |row| nanoclaw_message_from_row(row, 0, source),
    )
    .map_err(CaptureError::from)
}

fn nanoclaw_message_from_row(
    row: &Row<'_>,
    offset: usize,
    source: NanoClawMessageSource,
) -> rusqlite::Result<NanoClawMessageRow> {
    Ok(NanoClawMessageRow {
        source: source.label(),
        id: row.get(offset)?,
        seq: row.get(offset + 1)?,
        kind: row.get(offset + 2)?,
        timestamp: row.get(offset + 3)?,
        status: row.get(offset + 4)?,
        in_reply_to: row.get(offset + 5)?,
        platform_id: row.get(offset + 6)?,
        channel_type: row.get(offset + 7)?,
        thread_id: row.get(offset + 8)?,
        content: row.get(offset + 9)?,
        trigger: row.get(offset + 10)?,
        source_session_id: row.get(offset + 11)?,
        on_wake: row.get(offset + 12)?,
    })
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
