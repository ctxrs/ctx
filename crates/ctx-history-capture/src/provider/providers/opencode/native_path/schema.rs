use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{CaptureError, Result};

use super::model::OpenCodeNativeSchemaFamily;
use crate::provider::providers::opencode::OpenCodeSqliteDialect;

const MAX_NATIVE_IDENTITY_BYTES: i64 = 4 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenCodeNativeSchema {
    pub(super) family: OpenCodeNativeSchemaFamily,
    pub(super) capability_digest: String,
    pub(super) user_version: i64,
    pub(super) schema_version: i64,
    pub(super) session_columns: BTreeSet<String>,
    pub(super) event_has_type: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ColumnCapability {
    name: String,
    declared_type: String,
    not_null: bool,
    primary_key_ordinal: i64,
}

impl OpenCodeNativeSchema {
    pub(super) fn probe(conn: &Connection, dialect: &OpenCodeSqliteDialect) -> Result<Self> {
        let tables = sqlite_tables(conn)?;
        if !tables.contains("session") {
            return Err(CaptureError::InvalidPayload(format!(
                "{} NativePath requires the session table",
                dialect.display_name
            )));
        }
        let session = table_capabilities(conn, "session")?;
        require_identity_column(&session, "session", "id")?;
        require_integer_column(&session, "session", "time_created")?;
        require_integer_column(&session, "session", "time_updated")?;
        validate_session_rows(conn, session.contains_key("parent_id"))?;

        let session_message = if tables.contains("session_message") {
            let columns = table_capabilities(conn, "session_message")?;
            require_message_columns(&columns, "session_message")?;
            let family = if columns.contains_key("seq") {
                require_integer_column(&columns, "session_message", "seq")?;
                OpenCodeNativeSchemaFamily::SessionMessageSeq
            } else {
                OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq
            };
            Some((
                family,
                columns.contains_key("type"),
                table_has_rows(conn, "session_message")?,
            ))
        } else {
            None
        };
        let session_entry = if tables.contains("session_entry") {
            let columns = table_capabilities(conn, "session_entry")?;
            require_message_columns(&columns, "session_entry")?;
            Some((
                OpenCodeNativeSchemaFamily::SessionEntry,
                columns.contains_key("type"),
                table_has_rows(conn, "session_entry")?,
            ))
        } else {
            None
        };
        let message = if tables.contains("message") {
            let messages = table_capabilities(conn, "message")?;
            require_message_columns(&messages, "message")?;
            Some((
                messages.contains_key("type"),
                table_has_rows(conn, "message")?,
            ))
        } else {
            None
        };
        let part = if tables.contains("part") {
            let parts = table_capabilities(conn, "part")?;
            require_identity_column(&parts, "part", "id")?;
            require_text_column(&parts, "part", "message_id")?;
            require_text_column(&parts, "part", "session_id")?;
            require_integer_column(&parts, "part", "time_created")?;
            require_integer_column(&parts, "part", "time_updated")?;
            require_text_column(&parts, "part", "data")?;
            Some((parts.contains_key("type"), table_has_rows(conn, "part")?))
        } else {
            None
        };
        let message_part_join = if message.is_some() && part.is_some() {
            conn.query_row(
                "select exists(
                     select 1 from message m
                     join part p on p.message_id = m.id
                     limit 1
                 )",
                [],
                |row| row.get::<_, i64>(0),
            )? != 0
        } else {
            false
        };

        // Match the established importer precedence. Populated current families win first,
        // then an explicitly present empty current table remains authoritative over legacy
        // tables. A message+part route is authoritative only when its join is populated;
        // otherwise populated legacy messages must remain visible.
        let (family, event_has_type) = if let Some((family, has_type, true)) = session_message {
            (family, has_type)
        } else if let Some((family, has_type, true)) = session_entry {
            (family, has_type)
        } else if let Some((family, has_type, _)) = session_message {
            (family, has_type)
        } else if message_part_join {
            (
                OpenCodeNativeSchemaFamily::MessagePart,
                part.map(|(has_type, _)| has_type).unwrap_or(false),
            )
        } else if let Some((has_type, true)) = message {
            (OpenCodeNativeSchemaFamily::LegacyMessage, has_type)
        } else if let Some((family, has_type, _)) = session_entry {
            (family, has_type)
        } else if message.is_some() && part.is_some() {
            (
                OpenCodeNativeSchemaFamily::MessagePart,
                part.map(|(has_type, _)| has_type).unwrap_or(false),
            )
        } else if let Some((has_type, _)) = message {
            (OpenCodeNativeSchemaFamily::LegacyMessage, has_type)
        } else {
            return Err(CaptureError::InvalidPayload(format!(
                "{} NativePath found no explicitly supported message schema family",
                dialect.display_name
            )));
        };

        validate_native_ordering_rows(conn, family)?;
        let user_version = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let schema_version = conn.pragma_query_value(None, "schema_version", |row| row.get(0))?;
        let capability_digest = capability_digest(conn, user_version, family)?;
        Ok(Self {
            family,
            capability_digest,
            user_version,
            schema_version,
            session_columns: session.keys().cloned().collect(),
            event_has_type,
        })
    }
}

fn sqlite_tables(conn: &Connection) -> Result<BTreeSet<String>> {
    let mut statement = conn.prepare(
        "select name from sqlite_schema
         where type = 'table' and name not like 'sqlite_%'
         order by name",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(CaptureError::from)
}

fn table_has_rows(conn: &Connection, table: &str) -> Result<bool> {
    let sql = format!("select exists(select 1 from {table} limit 1)");
    Ok(conn.query_row(&sql, [], |row| row.get::<_, i64>(0))? != 0)
}

fn table_capabilities(
    conn: &Connection,
    table: &str,
) -> Result<BTreeMap<String, ColumnCapability>> {
    let sql = format!("pragma table_info(\"{}\")", table.replace('"', "\"\""));
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok(ColumnCapability {
            name: row.get(1)?,
            declared_type: row.get::<_, String>(2)?.trim().to_ascii_uppercase(),
            not_null: row.get::<_, i64>(3)? != 0,
            primary_key_ordinal: row.get(5)?,
        })
    })?;
    let mut columns = BTreeMap::new();
    for row in rows {
        let capability = row?;
        columns.insert(capability.name.clone(), capability);
    }
    Ok(columns)
}

fn require_message_columns(
    columns: &BTreeMap<String, ColumnCapability>,
    table: &str,
) -> Result<()> {
    require_identity_column(columns, table, "id")?;
    require_text_column(columns, table, "session_id")?;
    require_integer_column(columns, table, "time_created")?;
    require_integer_column(columns, table, "time_updated")?;
    require_text_column(columns, table, "data")
}

fn require_identity_column(
    columns: &BTreeMap<String, ColumnCapability>,
    table: &str,
    column: &str,
) -> Result<()> {
    let capability = require_text_column_value(columns, table, column)?;
    if capability.primary_key_ordinal != 1 {
        return Err(CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {table}.{column} must remain the single-column primary identity"
        )));
    }
    Ok(())
}

fn require_text_column(
    columns: &BTreeMap<String, ColumnCapability>,
    table: &str,
    column: &str,
) -> Result<()> {
    require_text_column_value(columns, table, column).map(|_| ())
}

fn require_text_column_value<'a>(
    columns: &'a BTreeMap<String, ColumnCapability>,
    table: &str,
    column: &str,
) -> Result<&'a ColumnCapability> {
    let capability = columns.get(column).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {table} is missing required column {column}"
        ))
    })?;
    if capability.declared_type != "TEXT" {
        return Err(CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {table}.{column} changed identity/content type from TEXT"
        )));
    }
    Ok(capability)
}

fn require_integer_column(
    columns: &BTreeMap<String, ColumnCapability>,
    table: &str,
    column: &str,
) -> Result<()> {
    let capability = columns.get(column).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {table} is missing required ordering column {column}"
        ))
    })?;
    if capability.declared_type != "INTEGER" {
        return Err(CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {table}.{column} changed ordering type from INTEGER"
        )));
    }
    Ok(())
}

fn validate_native_ordering_rows(
    conn: &Connection,
    family: OpenCodeNativeSchemaFamily,
) -> Result<()> {
    let table = family.event_table();
    let identity_invalid = format!(
        "select exists(
             select 1 from {table}
             where typeof(id) <> 'text' or trim(id) = ''
                or octet_length(id) > {MAX_NATIVE_IDENTITY_BYTES}
                or typeof(session_id) <> 'text' or trim(session_id) = ''
                or octet_length(session_id) > {MAX_NATIVE_IDENTITY_BYTES}
         )"
    );
    if conn.query_row(&identity_invalid, [], |row| row.get::<_, i64>(0))? != 0 {
        return Err(CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {table} contains an unsafe native identity/order key"
        )));
    }
    if family == OpenCodeNativeSchemaFamily::MessagePart {
        let relationship_key_invalid: i64 = conn.query_row(
            &format!(
                "select exists(
                     select 1 from part
                     where typeof(message_id) <> 'text' or trim(message_id) = ''
                        or octet_length(message_id) > {MAX_NATIVE_IDENTITY_BYTES}
                 )"
            ),
            [],
            |row| row.get(0),
        )?;
        if relationship_key_invalid != 0 {
            return Err(CaptureError::InvalidPayload(
                "OpenCode NativePath part.message_id is not a safe native relationship key"
                    .to_owned(),
            ));
        }
        let parent_order_invalid: i64 = conn.query_row(
            &format!(
                "select exists(
                     select 1 from message
                     where typeof(id) <> 'text' or trim(id) = ''
                        or octet_length(id) > {MAX_NATIVE_IDENTITY_BYTES}
                        or typeof(session_id) <> 'text' or trim(session_id) = ''
                        or octet_length(session_id) > {MAX_NATIVE_IDENTITY_BYTES}
                        or typeof(time_created) <> 'integer'
                        or typeof(time_updated) <> 'integer'
                 )"
            ),
            [],
            |row| row.get(0),
        )?;
        if parent_order_invalid != 0 {
            return Err(CaptureError::InvalidPayload(
                "OpenCode NativePath message parent identity/order rows are unsafe".to_owned(),
            ));
        }
    }
    let ordering_invalid = match family {
        OpenCodeNativeSchemaFamily::SessionMessageSeq => format!(
            "select exists(
                 select 1 from {table}
                 where typeof(seq) <> 'integer' or seq < 0
                    or typeof(time_created) <> 'integer'
                    or typeof(time_updated) <> 'integer'
             )"
        ),
        OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq
        | OpenCodeNativeSchemaFamily::SessionEntry
        | OpenCodeNativeSchemaFamily::LegacyMessage
        | OpenCodeNativeSchemaFamily::MessagePart => format!(
            "select exists(
                 select 1 from {table}
                 where typeof(time_created) <> 'integer'
                    or typeof(time_updated) <> 'integer'
             )"
        ),
    };
    if conn.query_row(&ordering_invalid, [], |row| row.get::<_, i64>(0))? != 0 {
        return Err(CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {table} contains a non-integer native ordering value"
        )));
    }
    if family == OpenCodeNativeSchemaFamily::SessionMessageSeq {
        let duplicate_order: i64 = conn.query_row(
            "select exists(
                 select 1 from session_message
                 group by session_id, seq
                 having count(*) > 1
             )",
            [],
            |row| row.get(0),
        )?;
        if duplicate_order != 0 {
            return Err(CaptureError::InvalidPayload(
                "OpenCode NativePath explicit session_message sequence is not unique".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_session_rows(conn: &Connection, has_parent_id: bool) -> Result<()> {
    let parent_invalid = if has_parent_id {
        format!(
            " or (parent_id is not null and (
                 typeof(parent_id) <> 'text'
                 or octet_length(parent_id) > {MAX_NATIVE_IDENTITY_BYTES}
             ))"
        )
    } else {
        String::new()
    };
    let invalid: i64 = conn.query_row(
        &format!(
            "select exists(
                 select 1 from session
                 where typeof(id) <> 'text' or trim(id) = ''
                    or octet_length(id) > {MAX_NATIVE_IDENTITY_BYTES}
                    or typeof(time_created) <> 'integer'
                    or typeof(time_updated) <> 'integer'
                    {parent_invalid}
             )"
        ),
        [],
        |row| row.get(0),
    )?;
    if invalid != 0 {
        return Err(CaptureError::InvalidPayload(
            "OpenCode NativePath session identity/order rows are unsafe".to_owned(),
        ));
    }
    Ok(())
}

fn capability_digest(
    conn: &Connection,
    user_version: i64,
    selected_family: OpenCodeNativeSchemaFamily,
) -> Result<String> {
    let table_names = sqlite_tables(conn)?;
    let mut tables = BTreeMap::new();
    for table in &table_names {
        let sql = format!("pragma table_info(\"{}\")", table.replace('"', "\"\""));
        let mut statement = conn.prepare(&sql)?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        tables.insert(table.clone(), columns);
    }
    let candidate_families = ["session_message", "session_entry", "message", "part"]
        .into_iter()
        .filter(|table| table_names.contains(*table))
        .collect::<Vec<_>>();
    let mut populated = BTreeMap::new();
    for table in &candidate_families {
        populated.insert(*table, table_has_rows(conn, table)?);
    }
    let message_part_join = if table_names.contains("message") && table_names.contains("part") {
        conn.query_row(
            "select exists(
                 select 1 from message m
                 join part p on p.message_id = m.id
                 limit 1
             )",
            [],
            |row| row.get::<_, i64>(0),
        )? != 0
    } else {
        false
    };
    let capabilities = json!({
        "candidate_families": candidate_families,
        "message_part_join": message_part_join,
        "populated": populated,
        "selected_family": selected_family.label(),
        "session_message_seq": tables
            .get("session_message")
            .is_some_and(|columns| columns.iter().any(|column| column == "seq")),
        "tables": tables,
        "user_version": user_version,
    });
    let canonical = serde_json::to_vec(&capabilities).map_err(|error| {
        CaptureError::InvalidPayload(format!(
            "OpenCode capability digest serialization failed: {error}"
        ))
    })?;
    let mut hasher = Sha256::new();
    hasher.update(canonical);
    hasher.update(b"\n");
    Ok(hex_digest(hasher.finalize().into()))
}

pub(super) fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
