use std::collections::BTreeSet;

use ctx_history_core::CaptureProvider;
use rusqlite::Connection;

use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
};
use crate::{
    CaptureError, Result, KILO_SQLITE_SOURCE_FORMAT, MIMOCODE_SQLITE_SOURCE_FORMAT,
    OPENCODE_SQLITE_SOURCE_FORMAT,
};

#[derive(Debug, Clone)]
pub(crate) struct OpenCodeMessageRow {
    pub(crate) id: String,
    pub(crate) entry_type: String,
}
#[derive(Debug, Clone)]
pub(crate) struct OpenCodeSqliteDialect {
    pub(crate) provider: CaptureProvider,
    pub(crate) display_name: &'static str,
    pub(crate) source_format: &'static str,
    pub(crate) session_message_time_created_field: &'static str,
    pub(crate) event_time_created_field: &'static str,
}

pub(crate) const OPENCODE_SQLITE_DIALECT: OpenCodeSqliteDialect = OpenCodeSqliteDialect {
    provider: CaptureProvider::OpenCode,
    display_name: "OpenCode",
    source_format: OPENCODE_SQLITE_SOURCE_FORMAT,
    session_message_time_created_field: "OpenCode session_message time_created",
    event_time_created_field: "OpenCode event time.created",
};

pub(crate) const KILO_SQLITE_DIALECT: OpenCodeSqliteDialect = OpenCodeSqliteDialect {
    provider: CaptureProvider::Kilo,
    display_name: "Kilo",
    source_format: KILO_SQLITE_SOURCE_FORMAT,
    session_message_time_created_field: "Kilo session_message time_created",
    event_time_created_field: "Kilo event time.created",
};

pub(crate) const MIMOCODE_SQLITE_DIALECT: OpenCodeSqliteDialect = OpenCodeSqliteDialect {
    provider: CaptureProvider::MiMoCode,
    display_name: "MiMo Code",
    source_format: MIMOCODE_SQLITE_SOURCE_FORMAT,
    session_message_time_created_field: "MiMo Code session_message time_created",
    event_time_created_field: "MiMo Code event time.created",
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenCodeCapturedShape {
    SessionMessage,
    SessionEntry,
    MessagePart,
    Message,
}

impl OpenCodeCapturedShape {
    pub(crate) fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(Self::SessionMessage),
            2 => Ok(Self::SessionEntry),
            3 => Ok(Self::MessagePart),
            4 => Ok(Self::Message),
            _ => Err(CaptureError::InvalidPayload(
                "OpenCode locator has an unknown captured shape".to_owned(),
            )),
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

    pub(super) fn hydration_sql(&self, shape: OpenCodeCapturedShape) -> String {
        let projected_data = if shape == OpenCodeCapturedShape::MessagePart {
            self.part_data.clone()
        } else {
            self.message_data.clone()
        };
        let (message_data, part_data) = if shape == OpenCodeCapturedShape::MessagePart {
            (self.message_data.as_str(), projected_data.as_str())
        } else {
            (projected_data.as_str(), self.part_data.as_str())
        };
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
            message_data = message_data,
            part_data = part_data,
            part_id = self.part_id,
            part_type = self.part_type,
            source_table = shape.label(),
            from_clause = self.from_clause,
            alias = self.source_alias,
        )
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
