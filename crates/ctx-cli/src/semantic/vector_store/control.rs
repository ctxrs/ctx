use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration as StdDuration,
};

use anyhow::{Context, Result};
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};

use crate::semantic::{
    health_search::{
        create_private_dir_all, secure_private_file_permissions, secure_semantic_vector_permissions,
    },
    model_contract::semantic_model_contract_descriptor,
    runtime_limits::SEMANTIC_VECTOR_BUSY_TIMEOUT_MS,
    vector_store_schema::SemanticVectorStoreError,
};

pub(super) const CONTROL_FILE: &str = "state.sqlite";
const CONTROL_APPLICATION_ID: i64 = 0x4354_584D; // "CTXM"
const CONTROL_SCHEMA_VERSION: i64 = 5;
const MODEL_CONTRACT_STATE: &str = "projection_model_contract";
pub(super) const FULL_REBUILD_STATE: &str = "projection_full_rebuild_v1";

pub(in crate::semantic) fn open_writable(root: &Path) -> Result<Connection> {
    validate_root(root, true)?;
    let path = control_path(root);
    validate_control_file(&path)?;
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .with_context(|| format!("open semantic control metadata {}", path.display()))?;
    connection.busy_timeout(StdDuration::from_millis(SEMANTIC_VECTOR_BUSY_TIMEOUT_MS))?;
    prepare_schema(&connection)?;
    connection.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA secure_delete = ON;
        "#,
    )?;
    secure_private_file_permissions(&path)?;
    secure_semantic_vector_permissions(&path)?;
    Ok(connection)
}

pub(in crate::semantic) fn open_read_only(root: &Path) -> Result<Option<Connection>> {
    if !root.exists() {
        return Ok(None);
    }
    validate_root(root, false)?;
    let path = control_path(root);
    if !path.exists() {
        return Ok(None);
    }
    validate_control_file(&path)?;
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .with_context(|| format!("open semantic control metadata {}", path.display()))?;
    connection.busy_timeout(StdDuration::from_millis(SEMANTIC_VECTOR_BUSY_TIMEOUT_MS))?;
    validate_schema(&connection)?;
    Ok(Some(connection))
}

fn control_path(root: &Path) -> PathBuf {
    root.join(CONTROL_FILE)
}

fn validate_root(root: &Path, create: bool) -> Result<()> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(SemanticVectorStoreError::unavailable(format!(
                "refusing semantic vector root symlink or non-directory {}",
                root.display()
            ))
            .into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            create_private_dir_all(root)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect semantic vector root {}", root.display()));
        }
    }
    if create {
        create_private_dir_all(root)?;
    }
    Ok(())
}

fn validate_control_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(SemanticVectorStoreError::unavailable(format!(
                "refusing semantic control metadata symlink or non-file {}",
                path.display()
            ))
            .into())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("inspect semantic control metadata {}", path.display())),
    }
}

fn prepare_schema(connection: &Connection) -> Result<()> {
    let application_id = pragma_i64(connection, "application_id")?;
    let schema_version = pragma_i64(connection, "user_version")?;
    if application_id == CONTROL_APPLICATION_ID
        && (1..CONTROL_SCHEMA_VERSION).contains(&schema_version)
    {
        let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Exclusive)?;
        transaction.execute_batch(
            r#"
            DROP TABLE IF EXISTS semantic_dirty_events;
            DROP TABLE IF EXISTS semantic_index_stats;
            DROP TABLE IF EXISTS semantic_maintenance_state;
            DROP TABLE IF EXISTS semantic_source_documents;
            DROP TABLE IF EXISTS semantic_source_receipts;
            "#,
        )?;
        create_schema(&transaction, true)?;
        transaction.commit()?;
        return Ok(());
    }
    if application_id == 0 && schema_version == 0 && user_table_count(connection)? == 0 {
        let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Exclusive)?;
        create_schema(&transaction, false)?;
        transaction.commit()?;
        return Ok(());
    }
    validate_schema(connection)?;
    let stored_contract = connection
        .query_row(
            "SELECT value FROM semantic_maintenance_state WHERE key = ?1",
            [MODEL_CONTRACT_STATE],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let expected_contract = semantic_model_contract_descriptor();
    if stored_contract.as_deref() != Some(expected_contract.as_str()) {
        let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Exclusive)?;
        transaction.execute("DELETE FROM semantic_dirty_events", [])?;
        transaction.execute("DELETE FROM semantic_maintenance_state", [])?;
        transaction.execute(
            "UPDATE semantic_index_stats SET dirty_items = 0 WHERE id = 1",
            [],
        )?;
        transaction.execute(
            "INSERT INTO semantic_maintenance_state(key, value) VALUES (?1, ?2)",
            params![MODEL_CONTRACT_STATE, expected_contract],
        )?;
        transaction.execute(
            "INSERT INTO semantic_maintenance_state(key, value) VALUES (?1, 'true')",
            [FULL_REBUILD_STATE],
        )?;
        transaction.commit()?;
    }
    Ok(())
}

fn create_schema(transaction: &Transaction<'_>, requires_full_rebuild: bool) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE semantic_index_stats (
            id INTEGER PRIMARY KEY CHECK(id = 1),
            dirty_items INTEGER NOT NULL CHECK(dirty_items >= 0)
        );
        INSERT INTO semantic_index_stats(id, dirty_items) VALUES (1, 0);
        CREATE TABLE semantic_dirty_events (
            event_id TEXT PRIMARY KEY,
            queued_at_ms INTEGER NOT NULL,
            priority_seq INTEGER,
            reason TEXT NOT NULL,
            attempts INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX idx_semantic_dirty_events_priority
            ON semantic_dirty_events(priority_seq DESC, queued_at_ms ASC, event_id ASC);
        CREATE TABLE semantic_maintenance_state (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        "#,
    )?;
    transaction.execute(
        "INSERT INTO semantic_maintenance_state(key, value) VALUES (?1, ?2)",
        params![MODEL_CONTRACT_STATE, semantic_model_contract_descriptor()],
    )?;
    if requires_full_rebuild {
        transaction.execute(
            "INSERT INTO semantic_maintenance_state(key, value) VALUES (?1, 'true')",
            [FULL_REBUILD_STATE],
        )?;
    }
    transaction.pragma_update(None, "application_id", CONTROL_APPLICATION_ID)?;
    transaction.pragma_update(None, "user_version", CONTROL_SCHEMA_VERSION)?;
    Ok(())
}

fn validate_schema(connection: &Connection) -> Result<()> {
    let application_id = pragma_i64(connection, "application_id")?;
    let schema_version = pragma_i64(connection, "user_version")?;
    if application_id != CONTROL_APPLICATION_ID {
        return Err(SemanticVectorStoreError::storage_conflict(
            "unrecognized semantic control metadata application id",
        )
        .into());
    }
    if schema_version > CONTROL_SCHEMA_VERSION {
        return Err(SemanticVectorStoreError::newer_schema(schema_version).into());
    }
    if schema_version != CONTROL_SCHEMA_VERSION {
        return Err(SemanticVectorStoreError::reset_required(format!(
            "semantic control metadata has unsupported schema version {schema_version}"
        ))
        .into());
    }
    let expected_tables = [
        "semantic_dirty_events",
        "semantic_index_stats",
        "semantic_maintenance_state",
    ];
    for table in expected_tables {
        let exists = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            return Err(SemanticVectorStoreError::reset_required(format!(
                "semantic control metadata is missing {table}"
            ))
            .into());
        }
    }
    let dirty = connection
        .query_row(
            "SELECT dirty_items FROM semantic_index_stats WHERE id = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if !matches!(dirty, Some(value) if value >= 0) {
        return Err(SemanticVectorStoreError::reset_required(
            "semantic control metadata has invalid dirty counts",
        )
        .into());
    }
    Ok(())
}

fn pragma_i64(connection: &Connection, name: &str) -> Result<i64> {
    let sql = format!("PRAGMA {name}");
    connection
        .query_row(&sql, [], |row| row.get(0))
        .map_err(Into::into)
}

fn user_table_count(connection: &Connection) -> Result<usize> {
    let count = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    usize::try_from(count).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    const V5_TABLES: [&str; 3] = [
        "semantic_dirty_events",
        "semantic_index_stats",
        "semantic_maintenance_state",
    ];

    fn user_tables(connection: &Connection) -> Result<Vec<String>> {
        let mut statement = connection.prepare(
            "SELECT name FROM sqlite_schema
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )?;
        let tables = statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(tables)
    }

    fn create_v4_fixture(root: &Path) -> Result<()> {
        let connection = Connection::open(control_path(root))?;
        connection.execute_batch(
            r#"
            CREATE TABLE semantic_index_stats (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                dirty_items INTEGER NOT NULL CHECK(dirty_items >= 0)
            );
            INSERT INTO semantic_index_stats(id, dirty_items) VALUES (1, 7);
            CREATE TABLE semantic_dirty_events (
                event_id TEXT PRIMARY KEY,
                queued_at_ms INTEGER NOT NULL,
                priority_seq INTEGER,
                reason TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX idx_semantic_dirty_events_priority
                ON semantic_dirty_events(priority_seq DESC, queued_at_ms ASC, event_id ASC);
            CREATE TABLE semantic_maintenance_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE semantic_source_receipts (
                source_identity_digest TEXT PRIMARY KEY,
                indexed_documents INTEGER NOT NULL CHECK(indexed_documents >= 0),
                semantic_eligible_documents INTEGER NOT NULL
                    CHECK(semantic_eligible_documents >= 0),
                core_record_accumulator TEXT NOT NULL,
                contract_fingerprint TEXT NOT NULL,
                semantic_policy_fingerprint TEXT NOT NULL,
                owned_event_count INTEGER NOT NULL CHECK(owned_event_count >= 0),
                owned_event_ids_hash TEXT NOT NULL
            );
            INSERT INTO semantic_source_receipts VALUES (
                'legacy-source', 3, 2, 'legacy-accumulator',
                'legacy-contract', 'legacy-policy', 2, 'legacy-events'
            );
            "#,
        )?;
        connection.pragma_update(None, "application_id", CONTROL_APPLICATION_ID)?;
        connection.pragma_update(None, "user_version", 4)?;
        Ok(())
    }

    #[test]
    fn fresh_control_database_has_no_obsolete_source_receipts_table() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let connection = open_writable(temporary.path())?;
        assert_eq!(user_tables(&connection)?, V5_TABLES);
        Ok(())
    }

    #[test]
    fn v4_upgrade_removes_obsolete_source_receipts_transactionally() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        create_v4_fixture(temporary.path())?;

        let connection = open_writable(temporary.path())?;
        assert_eq!(pragma_i64(&connection, "user_version")?, 5);
        assert_eq!(user_tables(&connection)?, V5_TABLES);
        assert_eq!(user_table_count(&connection)?, V5_TABLES.len());
        assert_eq!(
            connection.query_row(
                "SELECT value FROM semantic_maintenance_state WHERE key = ?1",
                [FULL_REBUILD_STATE],
                |row| row.get::<_, String>(0),
            )?,
            "true"
        );
        Ok(())
    }
}
