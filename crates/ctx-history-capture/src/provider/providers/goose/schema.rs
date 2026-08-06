use std::collections::BTreeSet;

use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_ident, sqlite_table_columns, sqlite_table_exists,
};
use crate::{CaptureError, Result};

pub(super) const GOOSE_MESSAGE_VALUE_COUNT: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct GooseSessionRow {
    pub(super) id: String,
    pub(super) name: Option<String>,
    pub(super) description: Option<String>,
    pub(super) user_set_name: bool,
    pub(super) session_type: Option<String>,
    pub(super) working_dir: Option<String>,
    pub(super) created_at: Option<String>,
    pub(super) updated_at: Option<String>,
    pub(super) extension_data: Option<String>,
    pub(super) total_tokens: Option<i64>,
    pub(super) input_tokens: Option<i64>,
    pub(super) output_tokens: Option<i64>,
    pub(super) accumulated_total_tokens: Option<i64>,
    pub(super) accumulated_input_tokens: Option<i64>,
    pub(super) accumulated_output_tokens: Option<i64>,
    pub(super) accumulated_cost: Option<f64>,
    pub(super) provider_name: Option<String>,
    pub(super) model_config_json: Option<String>,
    pub(super) goose_mode: Option<String>,
    pub(super) archived_at: Option<String>,
    pub(super) project_id: Option<String>,
    pub(super) parent_session_id: Option<String>,
}

pub(super) fn goose_session_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, "sessions")? {
        return Err(CaptureError::InvalidPayload(
            "Goose sessions.db is missing required sessions table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "sessions")?;
    ensure_sqlite_table_columns(&columns, "Goose sessions table", &["id"])?;
    Ok(columns)
}

pub(super) fn goose_message_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, "messages")? {
        return Err(CaptureError::InvalidPayload(
            "Goose sessions.db is missing required messages table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "messages")?;
    ensure_sqlite_table_columns(
        &columns,
        "Goose messages table",
        &["session_id", "role", "content_json"],
    )?;
    Ok(columns)
}

fn goose_qualified_optional_column(
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

struct GooseSqlFieldExpressions {
    hydration: String,
}

impl GooseSqlFieldExpressions {
    fn same(expression: String) -> Self {
        Self {
            hydration: expression,
        }
    }
}

pub(super) struct GooseSqlExpressions {
    pub(super) hydration: Vec<String>,
}

fn goose_sql_expressions<const N: usize>(
    fields: [GooseSqlFieldExpressions; N],
) -> GooseSqlExpressions {
    GooseSqlExpressions {
        hydration: fields.into_iter().map(|field| field.hydration).collect(),
    }
}

fn goose_optional_field(
    columns: &BTreeSet<String>,
    alias: &str,
    column: &str,
    fallback: &str,
) -> GooseSqlFieldExpressions {
    GooseSqlFieldExpressions::same(goose_qualified_optional_column(
        columns, alias, column, fallback,
    ))
}

pub(super) fn goose_session_expressions(
    columns: &BTreeSet<String>,
    alias: &str,
) -> GooseSqlExpressions {
    goose_sql_expressions([
        GooseSqlFieldExpressions::same(format!("CAST({alias}.id AS TEXT)")),
        goose_optional_field(columns, alias, "name", "NULL"),
        goose_optional_field(columns, alias, "description", "NULL"),
        GooseSqlFieldExpressions::same(if columns.contains("user_set_name") {
            format!("coalesce({alias}.user_set_name, 0)")
        } else {
            "0".to_owned()
        }),
        goose_optional_field(columns, alias, "session_type", "NULL"),
        goose_optional_field(columns, alias, "working_dir", "NULL"),
        goose_optional_field(columns, alias, "created_at", "NULL"),
        goose_optional_field(columns, alias, "updated_at", "NULL"),
        goose_optional_field(columns, alias, "extension_data", "NULL"),
        goose_optional_field(columns, alias, "total_tokens", "NULL"),
        goose_optional_field(columns, alias, "input_tokens", "NULL"),
        goose_optional_field(columns, alias, "output_tokens", "NULL"),
        goose_optional_field(columns, alias, "accumulated_total_tokens", "NULL"),
        goose_optional_field(columns, alias, "accumulated_input_tokens", "NULL"),
        goose_optional_field(columns, alias, "accumulated_output_tokens", "NULL"),
        GooseSqlFieldExpressions::same(if columns.contains("accumulated_cost") {
            format!("cast({alias}.accumulated_cost as real)")
        } else {
            "NULL".to_owned()
        }),
        goose_optional_field(columns, alias, "provider_name", "NULL"),
        goose_optional_field(columns, alias, "model_config_json", "NULL"),
        goose_optional_field(columns, alias, "goose_mode", "NULL"),
        goose_optional_field(columns, alias, "archived_at", "NULL"),
        goose_optional_field(columns, alias, "project_id", "NULL"),
        goose_optional_field(columns, alias, "parent_session_id", "NULL"),
    ])
}

pub(super) fn goose_schema_version(conn: &Connection) -> Result<Option<i64>> {
    if !sqlite_table_exists(conn, "schema_version")? {
        return Ok(None);
    }
    let columns = sqlite_table_columns(conn, "schema_version")?;
    let version_column = if columns.contains("version") {
        "version"
    } else if columns.contains("id") {
        "id"
    } else {
        return Ok(None);
    };
    let sql = format!("select max({version_column}) from schema_version");
    conn.query_row(&sql, [], |row| row.get::<_, Option<i64>>(0))
        .map_err(CaptureError::from)
}

const GOOSE_NATIVE_SCHEMA_VERSION: i64 = 15;
const GOOSE_CAPABILITY_DIGEST_DOMAIN: &[u8] = b"ctx-goose-nativepath-capability-v1\0";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GooseNativeSchema {
    pub(super) user_version: i64,
    pub(super) schema_version: i64,
    pub(super) capability_digest: String,
    session_columns: BTreeSet<String>,
    message_columns: BTreeSet<String>,
}

impl GooseNativeSchema {
    pub(super) fn probe(conn: &Connection) -> Result<Self> {
        let session_columns = goose_session_columns(conn)?;
        let message_columns = goose_message_columns(conn)?;
        ensure_sqlite_table_columns(&session_columns, "Goose NativePath sessions table", &["id"])?;
        ensure_sqlite_table_columns(
            &message_columns,
            "Goose NativePath messages table",
            &["id", "message_id", "session_id", "role", "content_json"],
        )?;
        goose_require_native_primary_key(conn, "sessions", "id", None)?;
        goose_require_native_primary_key(conn, "messages", "id", Some("INTEGER"))?;
        let schema_version = goose_schema_version(conn)?.ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Goose NativePath requires the schema_version table".to_owned(),
            )
        })?;
        if schema_version != GOOSE_NATIVE_SCHEMA_VERSION {
            let unsupported = u32::try_from(schema_version).map_err(|_| {
                CaptureError::InvalidPayload(format!(
                    "Goose NativePath schema version {schema_version} is outside the supported version domain"
                ))
            })?;
            return Err(CaptureError::UnsupportedSchemaVersion(unsupported));
        }
        let user_version =
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
        let schema_objects = goose_native_schema_objects(conn)?;
        let capability_digest = goose_capability_digest(
            user_version,
            schema_version,
            &session_columns,
            &message_columns,
            &schema_objects,
        );
        Ok(Self {
            user_version,
            schema_version,
            capability_digest,
            session_columns,
            message_columns,
        })
    }

    pub(super) fn session_hydration_expressions(&self, alias: &str) -> Vec<String> {
        goose_session_expressions(&self.session_columns, alias).hydration
    }

    pub(super) fn message_id_expression(&self, alias: &str) -> String {
        goose_qualified_optional_column(&self.message_columns, alias, "message_id", "NULL")
    }

    pub(super) fn message_created_timestamp_expression(&self, alias: &str) -> String {
        goose_qualified_optional_column(&self.message_columns, alias, "created_timestamp", "NULL")
    }

    pub(super) fn message_timestamp_expression(&self, alias: &str) -> String {
        goose_qualified_optional_column(&self.message_columns, alias, "timestamp", "NULL")
    }

    pub(super) fn message_tokens_expression(&self, alias: &str) -> String {
        goose_qualified_optional_column(&self.message_columns, alias, "tokens", "NULL")
    }

    pub(super) fn message_metadata_expression(&self, alias: &str) -> String {
        goose_qualified_optional_column(&self.message_columns, alias, "metadata_json", "NULL")
    }

    pub(super) fn session_storage_class_predicate(&self, alias: &str) -> String {
        let mut predicates = vec![format!("typeof({alias}.id) = 'text'")];
        for column in [
            "name",
            "description",
            "session_type",
            "working_dir",
            "created_at",
            "updated_at",
            "extension_data",
            "provider_name",
            "model_config_json",
            "goose_mode",
            "archived_at",
            "project_id",
            "parent_session_id",
        ] {
            if self.session_columns.contains(column) {
                predicates.push(format!("typeof({alias}.{column}) in ('null', 'text')"));
            }
        }
        for column in [
            "user_set_name",
            "total_tokens",
            "input_tokens",
            "output_tokens",
            "accumulated_total_tokens",
            "accumulated_input_tokens",
            "accumulated_output_tokens",
        ] {
            if self.session_columns.contains(column) {
                predicates.push(format!("typeof({alias}.{column}) in ('null', 'integer')"));
            }
        }
        if self.session_columns.contains("accumulated_cost") {
            predicates.push(format!(
                "typeof({alias}.accumulated_cost) in ('null', 'integer', 'real')"
            ));
        }
        predicates.join(" and ")
    }

    pub(super) fn message_storage_class_predicate(&self, alias: &str) -> String {
        let mut predicates = vec![
            format!("typeof({alias}.id) = 'integer'"),
            format!("typeof({alias}.session_id) = 'text'"),
            format!("typeof({alias}.role) = 'text'"),
            format!("typeof({alias}.content_json) = 'text'"),
        ];
        for column in ["message_id", "timestamp", "tokens", "metadata_json"] {
            if self.message_columns.contains(column) {
                predicates.push(format!("typeof({alias}.{column}) in ('null', 'text')"));
            }
        }
        if self.message_columns.contains("created_timestamp") {
            predicates.push(format!(
                "typeof({alias}.created_timestamp) in ('null', 'integer')"
            ));
        }
        predicates.join(" and ")
    }
}

fn goose_capability_digest(
    user_version: i64,
    schema_version: i64,
    session_columns: &BTreeSet<String>,
    message_columns: &BTreeSet<String>,
    schema_objects: &[(String, String, String)],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(GOOSE_CAPABILITY_DIGEST_DOMAIN);
    hasher.update(user_version.to_le_bytes());
    hasher.update(schema_version.to_le_bytes());
    for (table, columns) in [("sessions", session_columns), ("messages", message_columns)] {
        hasher.update((table.len() as u64).to_le_bytes());
        hasher.update(table.as_bytes());
        for column in columns {
            hasher.update((column.len() as u64).to_le_bytes());
            hasher.update(column.as_bytes());
        }
    }
    for (object_type, name, sql) in schema_objects {
        for field in [object_type, name, sql] {
            hasher.update((field.len() as u64).to_le_bytes());
            hasher.update(field.as_bytes());
        }
    }
    let digest: [u8; 32] = hasher.finalize().into();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn goose_native_schema_objects(conn: &Connection) -> Result<Vec<(String, String, String)>> {
    let mut statement = conn.prepare(
        "select type, name, coalesce(sql, '')
         from sqlite_schema
         where type in ('table', 'index')
           and (tbl_name in ('sessions', 'messages', 'schema_version')
                or name in ('sessions', 'messages', 'schema_version'))
         order by type, name",
    )?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

fn goose_require_native_primary_key(
    conn: &Connection,
    table: &str,
    column: &str,
    expected_declared_type: Option<&str>,
) -> Result<()> {
    let mut statement = conn.prepare(&format!("pragma table_info({})", sqlite_ident(table)))?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    let columns = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    let Some((_, declared_type, primary_key_ordinal)) =
        columns.into_iter().find(|(name, _, _)| name == column)
    else {
        return Err(CaptureError::InvalidPayload(format!(
            "Goose NativePath {table}.{column} is missing"
        )));
    };
    if primary_key_ordinal <= 0 {
        return Err(CaptureError::InvalidPayload(format!(
            "Goose NativePath requires {table}.{column} to be a primary key"
        )));
    }
    if let Some(expected) = expected_declared_type {
        let actual = declared_type.trim().to_ascii_uppercase();
        if actual != expected {
            return Err(CaptureError::InvalidPayload(format!(
                "Goose NativePath requires {table}.{column} to be declared {expected}, found {declared_type}"
            )));
        }
    }
    Ok(())
}
