//! Devin schema admission.
//!
//! Devin's history store is undocumented: Cognition publishes no schema and no
//! writer source, and its migrations land often enough that pinning one
//! version would stall imports on every release. Admission is therefore
//! structural, as it is for Kiro, Warp, Firebender, Hermes, ForgeCode, and
//! OpenCode: every table, column, and index the importer depends on is probed,
//! so a migration that removes or reshapes what it reads is refused before
//! projection rather than producing partial history.
//!
//! The migration version is still read, and it feeds the capability digest
//! that seeds the logical fingerprint. A version change therefore rotates the
//! published content digest and forces a full re-scan even when the shape is
//! unchanged, so an upstream migration is never silently absorbed.
//!
//! What this does not catch is a migration that changes the *meaning* of a
//! column without changing its shape. The forest rules this importer depends
//! on — the `parent_node_id` walk, `main_chain_id` as the chain tip, and
//! `metadata.summarized_from` splices — are semantic, so a redefinition there
//! would pass every probe here and surface only as a fingerprint rotation.

use std::collections::BTreeSet;

use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::{
    provider::sqlite::{
        ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
        sqlite_unique_index_for_columns, UniqueIndexProbe,
    },
    CaptureError, Result,
};

const DEVIN_CAPABILITY_DIGEST_DOMAIN: &[u8] = b"ctx-devin-nativepath-capability-v1\0";
const DEVIN_INDEX_PROBE: UniqueIndexProbe<'static> = UniqueIndexProbe {
    provider: "Devin",
    max_rows: 64,
};

const DEVIN_REQUIRED_SESSION_COLUMNS: &[&str] = &[
    "id",
    "working_directory",
    "created_at",
    "last_activity_at",
    "main_chain_id",
    "hidden",
];
const DEVIN_REQUIRED_NODE_COLUMNS: &[&str] = &[
    "row_id",
    "session_id",
    "node_id",
    "parent_node_id",
    "chat_message",
    "created_at",
];
const DEVIN_REQUIRED_TOOL_STATE_COLUMNS: &[&str] = &["session_id", "tool_call_id"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DevinNativeSchema {
    pub(super) schema_version: i64,
    pub(super) capability_digest: String,
    session_columns: BTreeSet<String>,
    node_columns: BTreeSet<String>,
    tool_state_columns: BTreeSet<String>,
}

impl DevinNativeSchema {
    pub(super) fn probe(conn: &Connection) -> Result<Self> {
        let schema_version = devin_schema_version(conn)?;

        let session_columns = devin_table_columns(conn, "sessions")?;
        let node_columns = devin_table_columns(conn, "message_nodes")?;
        let tool_state_columns = devin_table_columns(conn, "tool_call_state")?;
        ensure_sqlite_table_columns(
            &session_columns,
            "Devin sessions table",
            DEVIN_REQUIRED_SESSION_COLUMNS,
        )?;
        ensure_sqlite_table_columns(
            &node_columns,
            "Devin message_nodes table",
            DEVIN_REQUIRED_NODE_COLUMNS,
        )?;
        ensure_sqlite_table_columns(
            &tool_state_columns,
            "Devin tool_call_state table",
            DEVIN_REQUIRED_TOOL_STATE_COLUMNS,
        )?;

        // The forest walk probes one node at a time by (session_id, node_id),
        // so that pair must be a real unique index rather than a convention.
        if sqlite_unique_index_for_columns(
            conn,
            "message_nodes",
            &["session_id", "node_id"],
            &DEVIN_INDEX_PROBE,
        )?
        .is_none()
        {
            return Err(CaptureError::InvalidPayload(
                "Devin message_nodes requires a non-partial ascending UNIQUE BINARY index on (session_id, node_id) for bounded chain traversal"
                    .to_owned(),
            ));
        }
        devin_require_integer_row_id(conn)?;

        let schema_objects = devin_native_schema_objects(conn)?;
        let capability_digest = devin_capability_digest(
            schema_version,
            &session_columns,
            &node_columns,
            &tool_state_columns,
            &schema_objects,
        );
        Ok(Self {
            schema_version,
            capability_digest,
            session_columns,
            node_columns,
            tool_state_columns,
        })
    }

    pub(super) fn session_columns(&self) -> &BTreeSet<String> {
        &self.session_columns
    }

    #[cfg(test)]
    pub(super) fn has_session_column(&self, column: &str) -> bool {
        self.session_columns.contains(column)
    }

    #[cfg(test)]
    pub(super) fn has_node_column(&self, column: &str) -> bool {
        self.node_columns.contains(column)
    }

    #[cfg(test)]
    pub(super) fn has_tool_state_column(&self, column: &str) -> bool {
        self.tool_state_columns.contains(column)
    }
}

/// Reads the highest applied refinery migration.
///
/// Absence is a hard failure, not an unversioned default: without the
/// migration table there is no evidence of which shape the file holds.
fn devin_schema_version(conn: &Connection) -> Result<i64> {
    if !sqlite_table_exists(conn, "refinery_schema_history")? {
        return Err(CaptureError::InvalidPayload(
            "Devin requires the refinery_schema_history table".to_owned(),
        ));
    }
    let columns = sqlite_table_columns(conn, "refinery_schema_history")?;
    ensure_sqlite_table_columns(
        &columns,
        "Devin refinery_schema_history table",
        &["version"],
    )?;
    conn.query_row(
        "select max(version) from refinery_schema_history",
        [],
        |row| row.get::<_, Option<i64>>(0),
    )?
    .ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Devin refinery_schema_history records no applied migration".to_owned(),
        )
    })
}

fn devin_table_columns(conn: &Connection, table: &str) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, table)? {
        return Err(CaptureError::InvalidPayload(format!(
            "Devin requires the {table} table"
        )));
    }
    sqlite_table_columns(conn, table).map_err(CaptureError::from)
}

/// `message_nodes.row_id` is the rowid alias the scratch ordering and payload
/// hydration address rows by, so it has to be a declared INTEGER primary key
/// rather than an ordinary column that happens to be named that way.
fn devin_require_integer_row_id(conn: &Connection) -> Result<()> {
    let mut statement = conn.prepare(
        "select name, type, pk from pragma_table_info('message_nodes') where pk > 0 order by pk",
    )?;
    let keys = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    match keys.as_slice() {
        [(name, kind, _)] if name == "row_id" && kind.eq_ignore_ascii_case("INTEGER") => Ok(()),
        _ => Err(CaptureError::InvalidPayload(
            "Devin message_nodes must declare row_id as its INTEGER primary key".to_owned(),
        )),
    }
}

fn devin_capability_digest(
    schema_version: i64,
    session_columns: &BTreeSet<String>,
    node_columns: &BTreeSet<String>,
    tool_state_columns: &BTreeSet<String>,
    schema_objects: &[(String, String, String)],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(DEVIN_CAPABILITY_DIGEST_DOMAIN);
    hasher.update(schema_version.to_le_bytes());
    for (table, columns) in [
        ("sessions", session_columns),
        ("message_nodes", node_columns),
        ("tool_call_state", tool_state_columns),
    ] {
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

fn devin_native_schema_objects(conn: &Connection) -> Result<Vec<(String, String, String)>> {
    let mut statement = conn.prepare(
        "select type, name, coalesce(sql, '')
         from sqlite_schema
         where type in ('table', 'index')
           and tbl_name in ('sessions', 'message_nodes', 'tool_call_state',
                            'refinery_schema_history')
         order by type, name",
    )?;
    let objects = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(objects)
}
