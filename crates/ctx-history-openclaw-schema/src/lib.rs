//! Exact, bounded admission for the current OpenClaw per-agent SQLite schema.

use rusqlite::{Connection, OptionalExtension};
use thiserror::Error;

pub const OPENCLAW_AGENT_SCHEMA_VERSIONS: &[i64] = &[17, 19];
pub const OPENCLAW_AGENT_SQLITE_SOURCE_FORMAT: &str = "openclaw_agent_sqlite";

const MAX_SCHEMA_COLUMNS: i64 = 32;
const MAX_TABLE_INDEXES: i64 = 8;
const MAX_INDEX_COLUMNS: i64 = 4;

#[derive(Debug, Error)]
pub enum OpenClawSchemaError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    Mismatch(String),
}

pub type Result<T> = std::result::Result<T, OpenClawSchemaError>;

#[derive(Clone, Copy)]
struct ColumnSpec {
    name: &'static str,
    declared_type: &'static str,
    not_null: bool,
    default: Option<&'static str>,
    primary_key_position: i64,
}

impl ColumnSpec {
    const fn required(name: &'static str, declared_type: &'static str, pk: i64) -> Self {
        Self {
            name,
            declared_type,
            not_null: true,
            default: None,
            primary_key_position: pk,
        }
    }

    const fn required_default(
        name: &'static str,
        declared_type: &'static str,
        default: &'static str,
    ) -> Self {
        Self {
            name,
            declared_type,
            not_null: true,
            default: Some(default),
            primary_key_position: 0,
        }
    }

    const fn optional(name: &'static str, declared_type: &'static str) -> Self {
        Self {
            name,
            declared_type,
            not_null: false,
            default: None,
            primary_key_position: 0,
        }
    }

    const fn optional_default(
        name: &'static str,
        declared_type: &'static str,
        default: &'static str,
    ) -> Self {
        Self {
            name,
            declared_type,
            not_null: false,
            default: Some(default),
            primary_key_position: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IndexColumn {
    name: &'static str,
    descending: bool,
}

impl IndexColumn {
    const fn ascending(name: &'static str) -> Self {
        Self {
            name,
            descending: false,
        }
    }

    const fn descending(name: &'static str) -> Self {
        Self {
            name,
            descending: true,
        }
    }
}

#[derive(Clone, Copy)]
struct IndexSpec {
    name: &'static str,
    unique: bool,
    origin: &'static str,
    partial_predicate: Option<&'static str>,
    columns: &'static [IndexColumn],
}

#[derive(Clone, Copy)]
struct TableSpec {
    name: &'static str,
    columns: &'static [ColumnSpec],
    indexes: &'static [IndexSpec],
}

/// Validates the exact supported v17 or v19 table columns, affinities, nullability, defaults,
/// primary/unique keys, named indexes, and primary ownership claim.
///
/// Every schema enumeration has a fixed upper bound. Other OpenClaw tables are
/// permitted because the agent database contains unrelated product state.
pub fn validate_openclaw_agent(connection: &Connection, expected_agent_id: &str) -> Result<()> {
    let user_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if !OPENCLAW_AGENT_SCHEMA_VERSIONS.contains(&user_version) {
        return mismatch(format!(
            "OpenClaw PRAGMA user_version is {user_version}, expected one of {OPENCLAW_AGENT_SCHEMA_VERSIONS:?}"
        ));
    }
    for table in TABLES {
        let mut expected = *table;
        if user_version == 19 {
            match expected.name {
                "session_windows" => expected.indexes = SESSION_WINDOWS_V19_INDEXES,
                "transcript_event_identities" => expected.indexes = EVENT_IDENTITIES_V19_INDEXES,
                "session_transcript_active_events" => {
                    expected.columns = ACTIVE_EVENTS_V19_COLUMNS;
                    expected.indexes = ACTIVE_EVENTS_V19_INDEXES;
                }
                _ => {}
            }
        }
        validate_table(connection, &expected)?;
    }
    let owner = connection
        .query_row(
            "SELECT role, schema_version, agent_id FROM schema_meta WHERE meta_key = 'primary'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((role, schema_version, agent_id)) = owner else {
        return mismatch("OpenClaw schema_meta has no primary ownership row");
    };
    if role != "agent" || schema_version != user_version {
        return mismatch(format!(
            "OpenClaw primary ownership is role={role:?}, schema_version={schema_version}"
        ));
    }
    if agent_id.as_deref() != Some(expected_agent_id) {
        return mismatch(format!(
            "OpenClaw database owner {:?} does not match path agent {expected_agent_id:?}",
            agent_id.as_deref()
        ));
    }
    Ok(())
}

/// Returns false only for a well-read schema that does not satisfy a supported schema.
/// SQLite/resource errors remain errors so discovery can fail closed.
pub fn matches_openclaw_agent(
    connection: &Connection,
    expected_agent_id: &str,
) -> rusqlite::Result<bool> {
    match validate_openclaw_agent(connection, expected_agent_id) {
        Ok(()) => Ok(true),
        Err(OpenClawSchemaError::Mismatch(_)) => Ok(false),
        Err(OpenClawSchemaError::Sqlite(error)) => Err(error),
    }
}

fn validate_table(connection: &Connection, expected: &TableSpec) -> Result<()> {
    let table_flags = connection
        .query_row(
            "SELECT wr, strict FROM pragma_table_list \
             WHERE schema = 'main' AND name = ?1 AND type = 'table'",
            [expected.name],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    if table_flags != Some((0, 1)) {
        return mismatch(format!(
            "OpenClaw table {:?} is missing, WITHOUT ROWID, or is not STRICT",
            expected.name
        ));
    }

    let mut columns = connection.prepare(
        "SELECT name, type, \"notnull\", dflt_value, pk, hidden \
           FROM pragma_table_xinfo(?1) ORDER BY cid LIMIT ?2",
    )?;
    let actual = columns
        .query_map(
            rusqlite::params![expected.name, MAX_SCHEMA_COLUMNS],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if actual.len() != expected.columns.len() {
        return mismatch(format!(
            "OpenClaw table {:?} has {} columns, expected {}",
            expected.name,
            actual.len(),
            expected.columns.len()
        ));
    }
    for (position, (actual, expected_column)) in
        actual.into_iter().zip(expected.columns).enumerate()
    {
        let (name, declared_type, not_null, default, primary_key_position, hidden) = actual;
        if name != expected_column.name
            || !declared_type.eq_ignore_ascii_case(expected_column.declared_type)
            || sqlite_affinity(&declared_type) != sqlite_affinity(expected_column.declared_type)
            || not_null != expected_column.not_null
            || normalized_sql(default.as_deref()) != normalized_sql(expected_column.default)
            || primary_key_position != expected_column.primary_key_position
            || hidden != 0
        {
            return mismatch(format!(
                "OpenClaw table {:?} column {} ({name:?}) does not match the admitted schema",
                expected.name, position
            ));
        }
    }
    validate_indexes(connection, expected)
}

fn validate_indexes(connection: &Connection, table: &TableSpec) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT name, \"unique\", origin, partial \
           FROM pragma_index_list(?1) ORDER BY name LIMIT ?2",
    )?;
    let actual = statement
        .query_map(rusqlite::params![table.name, MAX_TABLE_INDEXES], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? != 0,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? != 0,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if actual.len() != table.indexes.len() {
        return mismatch(format!(
            "OpenClaw table {:?} has {} indexes, expected {}",
            table.name,
            actual.len(),
            table.indexes.len()
        ));
    }
    for ((name, unique, origin, partial), expected) in actual.into_iter().zip(table.indexes) {
        if name != expected.name
            || unique != expected.unique
            || origin != expected.origin
            || partial != expected.partial_predicate.is_some()
        {
            return mismatch(format!(
                "OpenClaw table {:?} index {name:?} does not match the admitted schema",
                table.name
            ));
        }
        validate_index_columns(connection, table.name, expected)?;
        let sql = connection
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type = 'index' AND name = ?1",
                [expected.name],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        match (expected.partial_predicate, sql.as_deref()) {
            (None, _) => {}
            (Some(predicate), Some(sql))
                if normalized_partial_predicate(sql).as_deref()
                    == normalized_sql(Some(predicate)).as_deref() => {}
            _ => {
                return mismatch(format!(
                    "OpenClaw partial index {:?} predicate does not match the admitted schema",
                    expected.name
                ));
            }
        }
    }
    Ok(())
}

fn validate_index_columns(
    connection: &Connection,
    table: &str,
    expected: &IndexSpec,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT name, desc, coll FROM pragma_index_xinfo(?1) \
           WHERE key = 1 ORDER BY seqno LIMIT ?2",
    )?;
    let actual = statement
        .query_map(rusqlite::params![expected.name, MAX_INDEX_COLUMNS], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, i64>(1)? != 0,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if actual.len() != expected.columns.len() {
        return mismatch(format!(
            "OpenClaw table {table:?} index {:?} has the wrong key width",
            expected.name
        ));
    }
    for ((name, descending, collation), expected_column) in actual.into_iter().zip(expected.columns)
    {
        if name.as_deref() != Some(expected_column.name)
            || descending != expected_column.descending
            || !collation.eq_ignore_ascii_case("BINARY")
        {
            return mismatch(format!(
                "OpenClaw table {table:?} index {:?} key columns do not match the admitted schema",
                expected.name
            ));
        }
    }
    Ok(())
}

fn normalized_partial_predicate(sql: &str) -> Option<String> {
    let normalized = normalized_sql(Some(sql))?;
    normalized
        .split_once(" where ")
        .map(|(_, predicate)| predicate.to_owned())
}

fn normalized_sql(value: Option<&str>) -> Option<String> {
    value.map(|value| {
        value
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase()
    })
}

fn sqlite_affinity(declared_type: &str) -> &'static str {
    let declared_type = declared_type.to_ascii_uppercase();
    if declared_type.contains("INT") {
        "INTEGER"
    } else if declared_type.contains("CHAR")
        || declared_type.contains("CLOB")
        || declared_type.contains("TEXT")
    {
        "TEXT"
    } else if declared_type.contains("BLOB") || declared_type.is_empty() {
        "BLOB"
    } else if declared_type.contains("REAL")
        || declared_type.contains("FLOA")
        || declared_type.contains("DOUB")
    {
        "REAL"
    } else {
        "NUMERIC"
    }
}

fn mismatch<T>(detail: impl Into<String>) -> Result<T> {
    Err(OpenClawSchemaError::Mismatch(detail.into()))
}

const SCHEMA_META_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::required("meta_key", "TEXT", 1),
    ColumnSpec::required("role", "TEXT", 0),
    ColumnSpec::required("schema_version", "INTEGER", 0),
    ColumnSpec::optional("agent_id", "TEXT"),
    ColumnSpec::optional("app_version", "TEXT"),
    ColumnSpec::required("created_at", "INTEGER", 0),
    ColumnSpec::required("updated_at", "INTEGER", 0),
];
const SESSION_WINDOWS_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::required("session_id", "TEXT", 1),
    ColumnSpec::required("session_key", "TEXT", 0),
    ColumnSpec::optional("previous_session_id", "TEXT"),
    ColumnSpec::optional("reason", "TEXT"),
    ColumnSpec::required_default("session_scope", "TEXT", "'conversation'"),
    ColumnSpec::required("created_at", "INTEGER", 0),
    ColumnSpec::required("updated_at", "INTEGER", 0),
    ColumnSpec::optional_default("transcript_updated_at", "INTEGER", "NULL"),
    ColumnSpec::optional_default("transcript_observed_at", "INTEGER", "NULL"),
    ColumnSpec::required_default("session_entry_provenance", "INTEGER", "0"),
    ColumnSpec::required_default("acp_owned", "INTEGER", "0"),
    ColumnSpec::optional("plugin_owner_id", "TEXT"),
    ColumnSpec::optional("hook_external_content_source", "TEXT"),
    ColumnSpec::optional("started_at", "INTEGER"),
    ColumnSpec::optional("ended_at", "INTEGER"),
    ColumnSpec::optional("status", "TEXT"),
    ColumnSpec::optional("chat_type", "TEXT"),
    ColumnSpec::optional("channel", "TEXT"),
    ColumnSpec::optional("account_id", "TEXT"),
    ColumnSpec::optional("primary_conversation_id", "TEXT"),
    ColumnSpec::optional("model_provider", "TEXT"),
    ColumnSpec::optional("model", "TEXT"),
    ColumnSpec::optional("agent_harness_id", "TEXT"),
    ColumnSpec::optional("parent_session_key", "TEXT"),
    ColumnSpec::optional("spawned_by", "TEXT"),
    ColumnSpec::optional("display_name", "TEXT"),
];
const TRANSCRIPT_EVENTS_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::required("session_id", "TEXT", 1),
    ColumnSpec::required("seq", "INTEGER", 2),
    ColumnSpec::required("event_json", "TEXT", 0),
    ColumnSpec::required("created_at", "INTEGER", 0),
];
const TRANSCRIPT_ARCHIVES_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::required("session_id", "TEXT", 1),
    ColumnSpec::required("generation", "TEXT", 2),
    ColumnSpec::required("session_key", "TEXT", 0),
    ColumnSpec::required("reason", "TEXT", 0),
    ColumnSpec::required("encoding", "TEXT", 0),
    ColumnSpec::required("archive_blob", "BLOB", 0),
    ColumnSpec::required("archive_sha256", "TEXT", 0),
    ColumnSpec::required("archive_name", "TEXT", 0),
    ColumnSpec::required("created_at", "INTEGER", 0),
    ColumnSpec::optional("published_at", "INTEGER"),
    ColumnSpec::required_default("publish_attempts", "INTEGER", "0"),
    ColumnSpec::optional("last_publish_attempt_at", "INTEGER"),
    ColumnSpec::optional("last_publish_error", "TEXT"),
];
const EVENT_IDENTITIES_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::required("session_id", "TEXT", 1),
    ColumnSpec::required("event_id", "TEXT", 2),
    ColumnSpec::required("seq", "INTEGER", 0),
    ColumnSpec::optional("event_type", "TEXT"),
    ColumnSpec::optional("parent_id", "TEXT"),
    ColumnSpec::optional("message_idempotency_key", "TEXT"),
    ColumnSpec::required("created_at", "INTEGER", 0),
];
const INDEX_STATE_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::required("session_id", "TEXT", 1),
    ColumnSpec::required("indexed_seq", "INTEGER", 0),
    ColumnSpec::optional("leaf_event_id", "TEXT"),
    ColumnSpec::required_default("needs_rebuild", "INTEGER", "0"),
    ColumnSpec::required_default("active_event_count", "INTEGER", "0"),
    ColumnSpec::required_default("active_message_count", "INTEGER", "0"),
    ColumnSpec::required("updated_at", "INTEGER", 0),
];
const ACTIVE_EVENTS_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec::required("session_id", "TEXT", 1),
    ColumnSpec::required("active_position", "INTEGER", 2),
    ColumnSpec::required("event_seq", "INTEGER", 0),
    ColumnSpec::optional("message_position", "INTEGER"),
];

const ASC_META_KEY: &[IndexColumn] = &[IndexColumn::ascending("meta_key")];
const ASC_SESSION_ID: &[IndexColumn] = &[IndexColumn::ascending("session_id")];
const SESSION_SEQ: &[IndexColumn] = &[
    IndexColumn::ascending("session_id"),
    IndexColumn::ascending("seq"),
];
const SESSION_GENERATION: &[IndexColumn] = &[
    IndexColumn::ascending("session_id"),
    IndexColumn::ascending("generation"),
];
const ASC_ARCHIVE_NAME: &[IndexColumn] = &[IndexColumn::ascending("archive_name")];
const SESSION_EVENT_ID: &[IndexColumn] = &[
    IndexColumn::ascending("session_id"),
    IndexColumn::ascending("event_id"),
];
const SESSION_POSITION: &[IndexColumn] = &[
    IndexColumn::ascending("session_id"),
    IndexColumn::ascending("active_position"),
];
const SESSION_EVENT_SEQ: &[IndexColumn] = &[
    IndexColumn::ascending("session_id"),
    IndexColumn::ascending("event_seq"),
];
const SESSION_MESSAGE_POSITION: &[IndexColumn] = &[
    IndexColumn::ascending("session_id"),
    IndexColumn::ascending("message_position"),
];
const WINDOW_UPDATED: &[IndexColumn] = &[
    IndexColumn::descending("updated_at"),
    IndexColumn::ascending("session_id"),
];
const WINDOW_CREATED: &[IndexColumn] = &[
    IndexColumn::descending("created_at"),
    IndexColumn::ascending("session_id"),
];
const WINDOW_CONVERSATION: &[IndexColumn] = &[
    IndexColumn::ascending("primary_conversation_id"),
    IndexColumn::descending("updated_at"),
    IndexColumn::ascending("session_id"),
];
const ARCHIVE_ORDER: &[IndexColumn] = &[
    IndexColumn::ascending("created_at"),
    IndexColumn::ascending("session_id"),
    IndexColumn::ascending("generation"),
];
const SESSION_IDEMPOTENCY: &[IndexColumn] = &[
    IndexColumn::ascending("session_id"),
    IndexColumn::ascending("message_idempotency_key"),
];
const SESSION_PARENT: &[IndexColumn] = &[
    IndexColumn::ascending("session_id"),
    IndexColumn::ascending("parent_id"),
];
const EVENT_SEQUENCE: &[IndexColumn] = &[
    IndexColumn::ascending("session_id"),
    IndexColumn::ascending("event_type"),
    IndexColumn::descending("seq"),
];

const SCHEMA_META_INDEXES: &[IndexSpec] = &[IndexSpec {
    name: "sqlite_autoindex_schema_meta_1",
    unique: true,
    origin: "pk",
    partial_predicate: None,
    columns: ASC_META_KEY,
}];
const SESSION_WINDOWS_INDEXES: &[IndexSpec] = &[
    IndexSpec {
        name: "idx_agent_session_windows_conversation",
        unique: false,
        origin: "c",
        partial_predicate: Some("primary_conversation_id IS NOT NULL"),
        columns: WINDOW_CONVERSATION,
    },
    IndexSpec {
        name: "idx_agent_session_windows_created_at",
        unique: false,
        origin: "c",
        partial_predicate: None,
        columns: WINDOW_CREATED,
    },
    IndexSpec {
        name: "idx_agent_session_windows_updated_at",
        unique: false,
        origin: "c",
        partial_predicate: None,
        columns: WINDOW_UPDATED,
    },
    IndexSpec {
        name: "sqlite_autoindex_session_windows_1",
        unique: true,
        origin: "pk",
        partial_predicate: None,
        columns: ASC_SESSION_ID,
    },
];
const TRANSCRIPT_EVENTS_INDEXES: &[IndexSpec] = &[IndexSpec {
    name: "sqlite_autoindex_transcript_events_1",
    unique: true,
    origin: "pk",
    partial_predicate: None,
    columns: SESSION_SEQ,
}];
const TRANSCRIPT_ARCHIVES_INDEXES: &[IndexSpec] = &[
    IndexSpec {
        name: "idx_agent_session_transcript_archives_pending",
        unique: false,
        origin: "c",
        partial_predicate: Some("published_at IS NULL"),
        columns: ARCHIVE_ORDER,
    },
    IndexSpec {
        name: "idx_agent_session_transcript_archives_retention",
        unique: false,
        origin: "c",
        partial_predicate: None,
        columns: ARCHIVE_ORDER,
    },
    IndexSpec {
        name: "sqlite_autoindex_session_transcript_archives_1",
        unique: true,
        origin: "u",
        partial_predicate: None,
        columns: ASC_ARCHIVE_NAME,
    },
    IndexSpec {
        name: "sqlite_autoindex_session_transcript_archives_2",
        unique: true,
        origin: "pk",
        partial_predicate: None,
        columns: SESSION_GENERATION,
    },
];
const EVENT_IDENTITIES_INDEXES: &[IndexSpec] = &[
    IndexSpec {
        name: "idx_agent_transcript_event_parent",
        unique: false,
        origin: "c",
        partial_predicate: Some("parent_id IS NOT NULL"),
        columns: SESSION_PARENT,
    },
    IndexSpec {
        name: "idx_agent_transcript_event_sequence",
        unique: false,
        origin: "c",
        partial_predicate: None,
        columns: EVENT_SEQUENCE,
    },
    IndexSpec {
        name: "idx_agent_transcript_message_idempotency",
        unique: true,
        origin: "c",
        partial_predicate: Some("message_idempotency_key IS NOT NULL"),
        columns: SESSION_IDEMPOTENCY,
    },
    IndexSpec {
        name: "sqlite_autoindex_transcript_event_identities_1",
        unique: true,
        origin: "pk",
        partial_predicate: None,
        columns: SESSION_EVENT_ID,
    },
];
const INDEX_STATE_INDEXES: &[IndexSpec] = &[IndexSpec {
    name: "sqlite_autoindex_session_transcript_index_state_1",
    unique: true,
    origin: "pk",
    partial_predicate: None,
    columns: ASC_SESSION_ID,
}];
const ACTIVE_EVENTS_INDEXES: &[IndexSpec] = &[
    IndexSpec {
        name: "idx_agent_transcript_active_event_seq",
        unique: true,
        origin: "c",
        partial_predicate: None,
        columns: SESSION_EVENT_SEQ,
    },
    IndexSpec {
        name: "idx_agent_transcript_active_messages",
        unique: true,
        origin: "c",
        partial_predicate: Some("message_position IS NOT NULL"),
        columns: SESSION_MESSAGE_POSITION,
    },
    IndexSpec {
        name: "sqlite_autoindex_session_transcript_active_events_1",
        unique: true,
        origin: "pk",
        partial_predicate: None,
        columns: SESSION_POSITION,
    },
];

// v19 keeps the transcript payload/ownership columns and adds these exact
// query indexes plus the nullable model-context projection flag. Context
// eligibility is not history visibility: all active events remain importable.
const ACTIVE_EVENTS_V19_COLUMNS: &[ColumnSpec] = &[
    ACTIVE_EVENTS_COLUMNS[0],
    ACTIVE_EVENTS_COLUMNS[1],
    ACTIVE_EVENTS_COLUMNS[2],
    ACTIVE_EVENTS_COLUMNS[3],
    ColumnSpec::optional("context_eligible", "INTEGER"),
];
const SESSION_WINDOWS_V19_INDEXES: &[IndexSpec] = &[
    SESSION_WINDOWS_INDEXES[0],
    SESSION_WINDOWS_INDEXES[1],
    IndexSpec {
        name: "idx_agent_session_windows_session_key",
        unique: false,
        origin: "c",
        partial_predicate: None,
        columns: &[
            IndexColumn::ascending("session_key"),
            IndexColumn::descending("updated_at"),
            IndexColumn::ascending("session_id"),
        ],
    },
    SESSION_WINDOWS_INDEXES[2],
    SESSION_WINDOWS_INDEXES[3],
];
const EVENT_IDENTITIES_V19_INDEXES: &[IndexSpec] = &[
    IndexSpec {
        name: "idx_agent_transcript_event_identity_sequence",
        unique: false,
        origin: "c",
        partial_predicate: None,
        columns: SESSION_SEQ,
    },
    EVENT_IDENTITIES_INDEXES[0],
    EVENT_IDENTITIES_INDEXES[1],
    EVENT_IDENTITIES_INDEXES[2],
    EVENT_IDENTITIES_INDEXES[3],
];
const ACTIVE_EVENTS_V19_INDEXES: &[IndexSpec] = &[
    ACTIVE_EVENTS_INDEXES[0],
    ACTIVE_EVENTS_INDEXES[1],
    IndexSpec {
        name: "idx_agent_transcript_context_pending",
        unique: false,
        origin: "c",
        partial_predicate: Some("context_eligible IS NULL"),
        columns: ASC_SESSION_ID,
    },
    ACTIVE_EVENTS_INDEXES[2],
];

const TABLES: &[TableSpec] = &[
    TableSpec {
        name: "schema_meta",
        columns: SCHEMA_META_COLUMNS,
        indexes: SCHEMA_META_INDEXES,
    },
    TableSpec {
        name: "session_windows",
        columns: SESSION_WINDOWS_COLUMNS,
        indexes: SESSION_WINDOWS_INDEXES,
    },
    TableSpec {
        name: "transcript_events",
        columns: TRANSCRIPT_EVENTS_COLUMNS,
        indexes: TRANSCRIPT_EVENTS_INDEXES,
    },
    TableSpec {
        name: "session_transcript_archives",
        columns: TRANSCRIPT_ARCHIVES_COLUMNS,
        indexes: TRANSCRIPT_ARCHIVES_INDEXES,
    },
    TableSpec {
        name: "transcript_event_identities",
        columns: EVENT_IDENTITIES_COLUMNS,
        indexes: EVENT_IDENTITIES_INDEXES,
    },
    TableSpec {
        name: "session_transcript_index_state",
        columns: INDEX_STATE_COLUMNS,
        indexes: INDEX_STATE_INDEXES,
    },
    TableSpec {
        name: "session_transcript_active_events",
        columns: ACTIVE_EVENTS_COLUMNS,
        indexes: ACTIVE_EVENTS_INDEXES,
    },
];

#[cfg(feature = "test-support")]
pub mod test_support {
    /// Minimal official v17 table/index surface needed by transcript import.
    pub const OPENCLAW_AGENT_V17_MINIMAL_SCHEMA: &str = r#"
PRAGMA user_version=17;
CREATE TABLE schema_meta (
  meta_key TEXT NOT NULL PRIMARY KEY, role TEXT NOT NULL, schema_version INTEGER NOT NULL,
  agent_id TEXT, app_version TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
) STRICT;
CREATE TABLE session_windows (
  session_id TEXT NOT NULL PRIMARY KEY, session_key TEXT NOT NULL, previous_session_id TEXT,
  reason TEXT, session_scope TEXT NOT NULL DEFAULT 'conversation', created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL, transcript_updated_at INTEGER DEFAULT NULL,
  transcript_observed_at INTEGER DEFAULT NULL,
  session_entry_provenance INTEGER NOT NULL DEFAULT 0, acp_owned INTEGER NOT NULL DEFAULT 0,
  plugin_owner_id TEXT, hook_external_content_source TEXT, started_at INTEGER, ended_at INTEGER,
  status TEXT, chat_type TEXT, channel TEXT, account_id TEXT, primary_conversation_id TEXT,
  model_provider TEXT, model TEXT, agent_harness_id TEXT, parent_session_key TEXT, spawned_by TEXT,
  display_name TEXT
) STRICT;
CREATE INDEX idx_agent_session_windows_updated_at
  ON session_windows(updated_at DESC, session_id);
CREATE INDEX idx_agent_session_windows_created_at
  ON session_windows(created_at DESC, session_id);
CREATE INDEX idx_agent_session_windows_conversation
  ON session_windows(primary_conversation_id, updated_at DESC, session_id)
  WHERE primary_conversation_id IS NOT NULL;
CREATE TABLE transcript_events (
  session_id TEXT NOT NULL, seq INTEGER NOT NULL, event_json TEXT NOT NULL,
  created_at INTEGER NOT NULL, PRIMARY KEY (session_id, seq)
) STRICT;
CREATE TABLE session_transcript_archives (
  session_id TEXT NOT NULL, generation TEXT NOT NULL, session_key TEXT NOT NULL,
  reason TEXT NOT NULL, encoding TEXT NOT NULL, archive_blob BLOB NOT NULL,
  archive_sha256 TEXT NOT NULL, archive_name TEXT NOT NULL UNIQUE, created_at INTEGER NOT NULL,
  published_at INTEGER, publish_attempts INTEGER NOT NULL DEFAULT 0,
  last_publish_attempt_at INTEGER, last_publish_error TEXT,
  PRIMARY KEY (session_id, generation)
) STRICT;
CREATE INDEX idx_agent_session_transcript_archives_pending
  ON session_transcript_archives(created_at, session_id, generation)
  WHERE published_at IS NULL;
CREATE INDEX idx_agent_session_transcript_archives_retention
  ON session_transcript_archives(created_at, session_id, generation);
CREATE TABLE transcript_event_identities (
  session_id TEXT NOT NULL, event_id TEXT NOT NULL, seq INTEGER NOT NULL, event_type TEXT,
  parent_id TEXT, message_idempotency_key TEXT, created_at INTEGER NOT NULL,
  PRIMARY KEY (session_id, event_id)
) STRICT;
CREATE UNIQUE INDEX idx_agent_transcript_message_idempotency
  ON transcript_event_identities(session_id, message_idempotency_key)
  WHERE message_idempotency_key IS NOT NULL;
CREATE INDEX idx_agent_transcript_event_parent
  ON transcript_event_identities(session_id, parent_id)
  WHERE parent_id IS NOT NULL;
CREATE INDEX idx_agent_transcript_event_sequence
  ON transcript_event_identities(session_id, event_type, seq DESC);
CREATE TABLE session_transcript_index_state (
  session_id TEXT NOT NULL PRIMARY KEY, indexed_seq INTEGER NOT NULL, leaf_event_id TEXT,
  needs_rebuild INTEGER NOT NULL DEFAULT 0, active_event_count INTEGER NOT NULL DEFAULT 0,
  active_message_count INTEGER NOT NULL DEFAULT 0, updated_at INTEGER NOT NULL
) STRICT;
CREATE TABLE session_transcript_active_events (
  session_id TEXT NOT NULL, active_position INTEGER NOT NULL, event_seq INTEGER NOT NULL,
  message_position INTEGER, PRIMARY KEY (session_id, active_position)
) STRICT;
CREATE UNIQUE INDEX idx_agent_transcript_active_event_seq
  ON session_transcript_active_events(session_id, event_seq);
CREATE UNIQUE INDEX idx_agent_transcript_active_messages
  ON session_transcript_active_events(session_id, message_position)
  WHERE message_position IS NOT NULL;
"#;
}
