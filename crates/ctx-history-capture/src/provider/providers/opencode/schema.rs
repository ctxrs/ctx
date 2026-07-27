use std::collections::BTreeSet;

use ctx_history_core::CaptureProvider;
use rusqlite::Connection;
use serde_json::Value;

use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
};
use crate::{
    CaptureError, Result, KILO_SQLITE_SOURCE_FORMAT, MIMOCODE_SQLITE_SOURCE_FORMAT,
    OPENCODE_SQLITE_SOURCE_FORMAT,
};

pub(super) const OPENCODE_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 24 * 5 + 12 * 9;
pub(super) const OPENCODE_SESSION_PARENT_OVERHEAD_BYTES: u64 = 6 * 5 + 8 * 9;
pub(super) const OPENCODE_MESSAGE_PART_OVERHEAD_BYTES: u64 = 5 * 5 + 4 * 9;

#[derive(Debug, Clone)]
pub(crate) struct OpenCodeSessionRow {
    pub(crate) id: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) title: String,
    pub(crate) directory: String,
    pub(crate) model: Option<String>,
    pub(crate) agent: Option<String>,
    pub(crate) time_created: i64,
    pub(crate) time_updated: i64,
    pub(crate) tokens_input: i64,
    pub(crate) tokens_output: i64,
    pub(crate) tokens_reasoning: i64,
    pub(crate) tokens_cache_read: i64,
    pub(crate) tokens_cache_write: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct OpenCodeMessageRow {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) entry_type: String,
    pub(crate) seq: i64,
    pub(crate) time_created: i64,
    pub(crate) time_updated: i64,
}

pub(crate) fn parse_json_object_string(value: Option<&str>) -> Value {
    value
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .unwrap_or(Value::Null)
}

pub(crate) struct OpenCodeSqliteDialect {
    pub(crate) provider: CaptureProvider,
    pub(crate) display_name: &'static str,
    pub(crate) source_format: &'static str,
    pub(crate) session_time_created_field: &'static str,
    pub(crate) session_message_time_created_field: &'static str,
    pub(crate) event_time_created_field: &'static str,
}

pub(crate) const OPENCODE_SQLITE_DIALECT: OpenCodeSqliteDialect = OpenCodeSqliteDialect {
    provider: CaptureProvider::OpenCode,
    display_name: "OpenCode",
    source_format: OPENCODE_SQLITE_SOURCE_FORMAT,
    session_time_created_field: "OpenCode session time_created",
    session_message_time_created_field: "OpenCode session_message time_created",
    event_time_created_field: "OpenCode event time.created",
};

pub(crate) const KILO_SQLITE_DIALECT: OpenCodeSqliteDialect = OpenCodeSqliteDialect {
    provider: CaptureProvider::Kilo,
    display_name: "Kilo",
    source_format: KILO_SQLITE_SOURCE_FORMAT,
    session_time_created_field: "Kilo session time_created",
    session_message_time_created_field: "Kilo session_message time_created",
    event_time_created_field: "Kilo event time.created",
};

pub(crate) const MIMOCODE_SQLITE_DIALECT: OpenCodeSqliteDialect = OpenCodeSqliteDialect {
    provider: CaptureProvider::MiMoCode,
    display_name: "MiMo Code",
    source_format: MIMOCODE_SQLITE_SOURCE_FORMAT,
    session_time_created_field: "MiMo Code session time_created",
    session_message_time_created_field: "MiMo Code session_message time_created",
    event_time_created_field: "MiMo Code event time.created",
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OpenCodeCapturedShape {
    SessionMessage,
    SessionEntry,
    MessagePart,
    Message,
}

impl OpenCodeCapturedShape {
    pub(super) fn tag(self) -> u8 {
        match self {
            Self::SessionMessage => 1,
            Self::SessionEntry => 2,
            Self::MessagePart => 3,
            Self::Message => 4,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::SessionMessage => "session_message",
            Self::SessionEntry => "session_entry",
            Self::MessagePart => "message+part",
            Self::Message => "message",
        }
    }
}

pub(super) fn opencode_captured_shape(
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<OpenCodeCapturedShape> {
    if !sqlite_table_exists(conn, "session")? {
        return Err(CaptureError::InvalidPayload(format!(
            "{} SQLite database is missing required session table",
            dialect.display_name
        )));
    }
    ensure_sqlite_table_columns(
        &sqlite_table_columns(conn, "session")?,
        &format!("{} SQLite session table", dialect.display_name),
        &["id"],
    )?;
    let has_session_message = sqlite_table_exists(conn, "session_message")?;
    if has_session_message {
        ensure_sqlite_table_columns(
            &sqlite_table_columns(conn, "session_message")?,
            &format!("{} SQLite session_message table", dialect.display_name),
            &["id", "session_id", "data"],
        )?;
        if opencode_table_has_rows(conn, "session_message")? {
            return Ok(OpenCodeCapturedShape::SessionMessage);
        }
    }
    let has_session_entry = sqlite_table_exists(conn, "session_entry")?;
    if has_session_entry {
        ensure_sqlite_table_columns(
            &sqlite_table_columns(conn, "session_entry")?,
            &format!("{} SQLite session_entry table", dialect.display_name),
            &[
                "id",
                "session_id",
                "type",
                "time_created",
                "time_updated",
                "data",
            ],
        )?;
        if opencode_table_has_rows(conn, "session_entry")? {
            return Ok(OpenCodeCapturedShape::SessionEntry);
        }
    }
    // `session_message` is the authoritative current schema even while empty. A populated
    // `session_entry` sibling may represent the same generation, but legacy message tables must
    // not override an explicitly present current-schema table.
    if has_session_message {
        return Ok(OpenCodeCapturedShape::SessionMessage);
    }
    let has_message = sqlite_table_exists(conn, "message")?;
    let has_part = sqlite_table_exists(conn, "part")?;
    if has_message && has_part {
        ensure_sqlite_table_columns(
            &sqlite_table_columns(conn, "message")?,
            &format!("{} SQLite message table", dialect.display_name),
            &["id", "session_id", "time_created", "time_updated", "data"],
        )?;
        ensure_sqlite_table_columns(
            &sqlite_table_columns(conn, "part")?,
            &format!("{} SQLite part table", dialect.display_name),
            &[
                "id",
                "message_id",
                "session_id",
                "time_created",
                "time_updated",
                "data",
            ],
        )?;
        if conn.query_row(
            "select exists(select 1 from message m join part p on p.message_id = m.id limit 1)",
            [],
            |row| row.get::<_, i64>(0),
        )? != 0
        {
            return Ok(OpenCodeCapturedShape::MessagePart);
        }
    }
    if has_message {
        ensure_sqlite_table_columns(
            &sqlite_table_columns(conn, "message")?,
            &format!("{} SQLite message table", dialect.display_name),
            &["id", "session_id", "time_created", "time_updated", "data"],
        )?;
        if opencode_table_has_rows(conn, "message")? {
            return Ok(OpenCodeCapturedShape::Message);
        }
    }
    if has_session_entry {
        return Ok(OpenCodeCapturedShape::SessionEntry);
    }
    if has_message && has_part {
        return Ok(OpenCodeCapturedShape::MessagePart);
    }
    if has_message {
        return Ok(OpenCodeCapturedShape::Message);
    }
    Err(CaptureError::InvalidPayload(format!(
        "{} SQLite database contains no supported message table",
        dialect.display_name
    )))
}

pub(super) fn opencode_table_has_rows(conn: &Connection, table: &str) -> Result<bool> {
    let sql = format!("select exists(select 1 from {table} limit 1)");
    Ok(conn.query_row(&sql, [], |row| row.get::<_, i64>(0))? != 0)
}

pub(super) struct OpenCodeRowSql {
    pub(super) source_alias: &'static str,
    pub(super) candidate_from_clause: String,
    pub(super) candidate_text: Vec<String>,
    pub(super) from_clause: String,
    pub(super) message_id: String,
    pub(super) session_id: String,
    pub(super) entry_type: String,
    pub(super) seq_present: String,
    pub(super) seq: String,
    pub(super) time_created: String,
    pub(super) time_updated: String,
    pub(super) message_data: String,
    pub(super) part_data: String,
    pub(super) part_id: String,
    pub(super) part_type: String,
}

impl OpenCodeRowSql {
    pub(super) fn for_shape(conn: &Connection, shape: OpenCodeCapturedShape) -> Result<Self> {
        match shape {
            OpenCodeCapturedShape::SessionMessage => {
                let columns = sqlite_table_columns(conn, "session_message")?;
                let seq_present = if columns.contains("seq") { "1" } else { "0" };
                Ok(Self {
                    source_alias: "x",
                    candidate_from_clause: "session_message x".to_owned(),
                    candidate_text: vec![
                        "x.id".to_owned(),
                        "x.session_id".to_owned(),
                        opencode_qualified_optional(&columns, "x", "type", "'message'"),
                        "x.data".to_owned(),
                    ],
                    from_clause: "session_message x".to_owned(),
                    message_id: "cast(x.id as text)".to_owned(),
                    session_id: "cast(x.session_id as text)".to_owned(),
                    entry_type: opencode_qualified_optional(&columns, "x", "type", "'message'"),
                    seq_present: seq_present.to_owned(),
                    seq: opencode_qualified_optional(&columns, "x", "seq", "0"),
                    time_created: opencode_qualified_optional(&columns, "x", "time_created", "0"),
                    time_updated: opencode_qualified_optional(&columns, "x", "time_updated", "0"),
                    message_data: "cast(x.data as text)".to_owned(),
                    part_data: "''".to_owned(),
                    part_id: "''".to_owned(),
                    part_type: "''".to_owned(),
                })
            }
            OpenCodeCapturedShape::SessionEntry => Ok(Self {
                source_alias: "x",
                candidate_from_clause: "session_entry x".to_owned(),
                candidate_text: vec![
                    "x.id".to_owned(),
                    "x.session_id".to_owned(),
                    "x.type".to_owned(),
                    "x.data".to_owned(),
                ],
                from_clause: "session_entry x".to_owned(),
                message_id: "cast(x.id as text)".to_owned(),
                session_id: "cast(x.session_id as text)".to_owned(),
                entry_type: "cast(x.type as text)".to_owned(),
                seq_present: "0".to_owned(),
                seq: "0".to_owned(),
                time_created: "cast(x.time_created as integer)".to_owned(),
                time_updated: "cast(x.time_updated as integer)".to_owned(),
                message_data: "cast(x.data as text)".to_owned(),
                part_data: "''".to_owned(),
                part_id: "''".to_owned(),
                part_type: "''".to_owned(),
            }),
            OpenCodeCapturedShape::MessagePart => {
                let part_columns = sqlite_table_columns(conn, "part")?;
                Ok(Self {
                    source_alias: "x",
                    candidate_from_clause: "part x".to_owned(),
                    candidate_text: vec![
                        "x.id".to_owned(),
                        "x.message_id".to_owned(),
                        "x.session_id".to_owned(),
                        "x.data".to_owned(),
                        opencode_qualified_optional(&part_columns, "x", "type", "''"),
                    ],
                    from_clause: "part x".to_owned(),
                    message_id: "cast(x.message_id as text)".to_owned(),
                    session_id: "cast(x.session_id as text)".to_owned(),
                    entry_type: "''".to_owned(),
                    seq_present: "0".to_owned(),
                    seq: "0".to_owned(),
                    time_created: "cast(x.time_created as integer)".to_owned(),
                    time_updated: "cast(x.time_updated as integer)".to_owned(),
                    message_data: "''".to_owned(),
                    part_data: "cast(x.data as text)".to_owned(),
                    part_id: "cast(x.id as text)".to_owned(),
                    part_type: opencode_qualified_optional(&part_columns, "x", "type", "''"),
                })
            }
            OpenCodeCapturedShape::Message => Ok(Self {
                source_alias: "x",
                candidate_from_clause: "message x".to_owned(),
                candidate_text: vec![
                    "x.id".to_owned(),
                    "x.session_id".to_owned(),
                    "x.data".to_owned(),
                ],
                from_clause: "message x".to_owned(),
                message_id: "cast(x.id as text)".to_owned(),
                session_id: "cast(x.session_id as text)".to_owned(),
                entry_type: "'message'".to_owned(),
                seq_present: "0".to_owned(),
                seq: "0".to_owned(),
                time_created: "cast(x.time_created as integer)".to_owned(),
                time_updated: "cast(x.time_updated as integer)".to_owned(),
                message_data: "cast(x.data as text)".to_owned(),
                part_data: "''".to_owned(),
                part_id: "''".to_owned(),
                part_type: "''".to_owned(),
            }),
        }
    }

    pub(super) fn candidate_sql(&self, seek: OpenCodeRowidSeek) -> String {
        let retained_text = self
            .candidate_text
            .iter()
            .map(String::as_str)
            .map(|expr| format!("coalesce(octet_length({expr}), 0)"))
            .collect::<Vec<_>>()
            .join(" + ");
        let overhead = if self.candidate_from_clause == "part x" {
            OPENCODE_MESSAGE_PART_OVERHEAD_BYTES
        } else {
            OPENCODE_SQLITE_VALUE_OVERHEAD_BYTES
        };
        format!(
            "select {alias}.rowid, {overhead} + {retained_text} \
             from {from_clause} where {alias}.rowid {comparison} ?1 \
             order by {alias}.rowid limit 1",
            alias = self.source_alias,
            from_clause = self.candidate_from_clause,
            comparison = seek.comparison(),
        )
    }

    pub(super) fn hydration_sql(&self, shape: OpenCodeCapturedShape) -> String {
        format!(
            "select coalesce(cast({message_id} as text), ''), \
                    coalesce(cast({source_session_id} as text), ''), \
                    coalesce(cast({entry_type} as text), ''), cast({seq_present} as integer), \
                    cast({seq} as integer), cast({time_created} as integer), \
                    cast({time_updated} as integer), \
                    coalesce(cast({message_data} as text), ''), \
                    coalesce(cast({part_data} as text), ''), \
                    coalesce(cast({part_id} as text), ''), \
                    coalesce(cast({part_type} as text), ''), '{source_table}' \
             from {from_clause} where {alias}.rowid = ?1",
            message_id = self.message_id,
            source_session_id = self.session_id,
            entry_type = self.entry_type,
            seq_present = self.seq_present,
            seq = self.seq,
            time_created = self.time_created,
            time_updated = self.time_updated,
            message_data = self.message_data,
            part_data = self.part_data,
            part_id = self.part_id,
            part_type = self.part_type,
            source_table = shape.label(),
            from_clause = self.from_clause,
            alias = self.source_alias,
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum OpenCodeRowidSeek {
    First,
    Next,
}

impl OpenCodeRowidSeek {
    pub(super) fn comparison(self) -> &'static str {
        match self {
            Self::First => ">=",
            Self::Next => ">",
        }
    }

    pub(super) fn bound(self, rowid: i64) -> i64 {
        match self {
            Self::First => i64::MIN,
            Self::Next => rowid,
        }
    }
}

pub(super) fn opencode_session_candidate_sql(
    retained_text: &str,
    seek: OpenCodeRowidSeek,
) -> String {
    format!(
        "select s.rowid, {OPENCODE_SESSION_PARENT_OVERHEAD_BYTES} + {retained_text} \
         from session s where s.rowid {} ?1 order by s.rowid limit 1",
        seek.comparison(),
    )
}

pub(super) fn opencode_session_retained_text(session: &OpenCodeSessionSql) -> String {
    [
        session.id.as_str(),
        session.parent_id.as_str(),
        session.title.as_str(),
        session.directory.as_str(),
        session.model.as_str(),
        session.agent.as_str(),
    ]
    .into_iter()
    .map(|expr| format!("coalesce(octet_length({expr}), 0)"))
    .collect::<Vec<_>>()
    .join(" + ")
}

pub(super) fn opencode_session_hydration_sql(session: &OpenCodeSessionSql) -> String {
    format!(
        "select coalesce(cast({id} as text), ''), \
                        coalesce(cast({parent_id} as text), ''), \
                        coalesce(cast({title} as text), ''), \
                        coalesce(cast({directory} as text), ''), \
                        coalesce(cast({model} as text), ''), \
                        coalesce(cast({agent} as text), ''), \
                        cast({time_created} as integer), cast({time_updated} as integer), \
                        cast({tokens_input} as integer), cast({tokens_output} as integer), \
                        cast({tokens_reasoning} as integer), \
                        cast({tokens_cache_read} as integer), \
                        cast({tokens_cache_write} as integer) \
         from session s where s.rowid = ?1",
        id = session.id,
        parent_id = session.parent_id,
        title = session.title,
        directory = session.directory,
        model = session.model,
        agent = session.agent,
        time_created = session.time_created,
        time_updated = session.time_updated,
        tokens_input = session.tokens_input,
        tokens_output = session.tokens_output,
        tokens_reasoning = session.tokens_reasoning,
        tokens_cache_read = session.tokens_cache_read,
        tokens_cache_write = session.tokens_cache_write,
    )
}

pub(super) fn opencode_session_id_lookup_index(conn: &Connection) -> Result<String> {
    let mut indexes = conn.prepare("pragma index_list('session')")?;
    let rows = indexes.query_map([], |row| {
        Ok((
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)? != 0,
            row.get::<_, i64>(4)? != 0,
        ))
    })?;
    for row in rows {
        let (name, unique, partial) = row?;
        if !unique || partial {
            continue;
        }
        let mut columns = conn
            .prepare("select name, desc, coll, key from pragma_index_xinfo(?1) order by seqno")?;
        let key_columns = columns
            .query_map([name.as_str()], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .filter_map(|row| match row {
                Ok((column, descending, collation, key)) if key != 0 => {
                    Some(Ok((column, descending, collation)))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if key_columns.len() == 1
            && key_columns[0].0.as_deref() == Some("id")
            && key_columns[0].1 == 0
            && key_columns[0]
                .2
                .as_deref()
                .is_some_and(|collation| collation.eq_ignore_ascii_case("binary"))
        {
            return Ok(name);
        }
    }
    Err(CaptureError::InvalidPayload(
        "OpenCode SQLite session.id requires a non-partial ascending UNIQUE BINARY index"
            .to_owned(),
    ))
}

pub(super) struct OpenCodeSessionSql {
    pub(super) id: String,
    pub(super) parent_id: String,
    pub(super) title: String,
    pub(super) directory: String,
    pub(super) model: String,
    pub(super) agent: String,
    pub(super) time_created: String,
    pub(super) time_updated: String,
    pub(super) tokens_input: String,
    pub(super) tokens_output: String,
    pub(super) tokens_reasoning: String,
    pub(super) tokens_cache_read: String,
    pub(super) tokens_cache_write: String,
}

impl OpenCodeSessionSql {
    pub(super) fn new(conn: &Connection) -> Result<Self> {
        let columns = sqlite_table_columns(conn, "session")?;
        let id = "s.id".to_owned();
        let title = if columns.contains("title") {
            "s.title".to_owned()
        } else if columns.contains("slug") {
            "s.slug".to_owned()
        } else {
            id.clone()
        };
        let time_created = opencode_qualified_optional(&columns, "s", "time_created", "0");
        Ok(Self {
            id,
            parent_id: opencode_qualified_optional(&columns, "s", "parent_id", "NULL"),
            title,
            directory: opencode_qualified_optional(&columns, "s", "directory", "''"),
            model: opencode_qualified_optional(&columns, "s", "model", "NULL"),
            agent: opencode_qualified_optional(&columns, "s", "agent", "NULL"),
            time_created: time_created.clone(),
            time_updated: opencode_qualified_optional(&columns, "s", "time_updated", &time_created),
            tokens_input: opencode_qualified_optional(&columns, "s", "tokens_input", "0"),
            tokens_output: opencode_qualified_optional(&columns, "s", "tokens_output", "0"),
            tokens_reasoning: opencode_qualified_optional(&columns, "s", "tokens_reasoning", "0"),
            tokens_cache_read: opencode_qualified_optional(&columns, "s", "tokens_cache_read", "0"),
            tokens_cache_write: opencode_qualified_optional(
                &columns,
                "s",
                "tokens_cache_write",
                "0",
            ),
        })
    }
}

pub(super) fn opencode_qualified_optional(
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
