use rusqlite::Connection;

use crate::schema::ddl::CREATE_TABLES_SQL;
use crate::{Result, StoreError};

mod maintenance;
mod maintenance_events;
mod maintenance_sessions;

pub(super) const PROVIDER_SESSION_REPAIR_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS provider_session_repair_state (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    state_json TEXT NOT NULL,
    completed INTEGER NOT NULL DEFAULT 0 CHECK (completed IN (0, 1)),
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS provider_session_repair_group_sessions (
    session_id TEXT PRIMARY KEY NOT NULL,
    source_id TEXT NOT NULL,
    raw_source_path TEXT,
    source_format TEXT,
    source_identity TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    component_id TEXT
);

CREATE INDEX IF NOT EXISTS idx_provider_session_repair_group_component
ON provider_session_repair_group_sessions(component_id, session_id);

CREATE INDEX IF NOT EXISTS idx_provider_session_repair_group_created
ON provider_session_repair_group_sessions(created_at_ms, session_id);

CREATE INDEX IF NOT EXISTS idx_provider_session_repair_group_identity
ON provider_session_repair_group_sessions(source_identity, session_id);

CREATE INDEX IF NOT EXISTS idx_provider_session_repair_group_path
ON provider_session_repair_group_sessions(raw_source_path, session_id);

CREATE TABLE IF NOT EXISTS provider_session_repair_component_keys (
    key_kind TEXT NOT NULL CHECK (key_kind IN ('identity', 'path')),
    key_value TEXT NOT NULL,
    scan_cursor_session_id TEXT,
    complete INTEGER NOT NULL DEFAULT 0 CHECK (complete IN (0, 1)),
    PRIMARY KEY (key_kind, key_value)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS provider_session_repair_events (
    event_id TEXT PRIMARY KEY NOT NULL,
    seq INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    provider_index TEXT,
    provider_hash TEXT
);

CREATE INDEX IF NOT EXISTS idx_provider_session_repair_events_order
ON provider_session_repair_events(seq, event_id);

CREATE TABLE IF NOT EXISTS provider_session_repair_event_identities (
    provider_index TEXT NOT NULL,
    provider_hash TEXT NOT NULL,
    event_id TEXT NOT NULL,
    PRIMARY KEY (provider_index, provider_hash)
) WITHOUT ROWID;
"#;

// Removal plan: once ctx intentionally requires an on-disk schema newer than
// every store that could have reached v46, remove this module, its state tables,
// and the v47 dispatch. The provider-session write fence remains permanent.
pub(super) fn migrate_to_v47(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        conn.execute_batch(PROVIDER_SESSION_REPAIR_SCHEMA_SQL)?;
        conn.execute(
            r#"
            INSERT OR IGNORE INTO provider_session_repair_state
            (singleton, state_json, completed, updated_at_ms)
            VALUES (1, '{"phase":"discover"}', 0, unixepoch('subsec') * 1000)
            "#,
            [],
        )?;
        conn.execute_batch("PRAGMA user_version = 47;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            Err(err)
        }
    }
}
