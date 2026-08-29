use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    time::Duration as StdDuration,
};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior};
use url::Url;

use crate::{
    private_fs::{
        create_private_dir_all, secure_private_file_permissions, secure_semantic_vector_permissions,
    },
    vector_store_schema::SemanticVectorStoreError,
};

pub(super) const CONTROL_FILE: &str = "state.sqlite";
const SEMANTIC_VECTOR_BUSY_TIMEOUT_MS: u64 = 30_000;
const CONTROL_APPLICATION_ID: i64 = 0x4354_584D; // "CTXM"
const CONTROL_SCHEMA_VERSION: i64 = 6;
pub(super) const FULL_REBUILD_STATE: &str = "projection_full_rebuild_v1";

pub(crate) fn open_writable(root: &Path) -> Result<Connection> {
    validate_root(root, true)?;
    let root = canonical_control_root(root)?;
    let path = control_path(&root);
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

pub(crate) fn open_read_only(root: &Path) -> Result<Option<Connection>> {
    if !root.exists() {
        return Ok(None);
    }
    validate_root(root, false)?;
    let root = canonical_control_root(root)?;
    let path = control_path(&root);
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

pub(crate) fn preflight_writable_compatibility(root: &Path) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    validate_root(root, false)?;
    let root = canonical_control_root(root)?;
    let path = control_path(&root);
    if !path.exists() {
        return Ok(());
    }
    validate_control_file(&path)?;
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .with_context(|| format!("preflight semantic control metadata {}", path.display()))?;
    connection.busy_timeout(StdDuration::from_millis(SEMANTIC_VECTOR_BUSY_TIMEOUT_MS))?;
    let application_id = pragma_i64(&connection, "application_id")?;
    let schema_version = pragma_i64(&connection, "user_version")?;
    if application_id == CONTROL_APPLICATION_ID
        && (1..=CONTROL_SCHEMA_VERSION).contains(&schema_version)
    {
        return Ok(());
    }
    if application_id == 0 && schema_version == 0 && user_table_count(&connection)? == 0 {
        return Ok(());
    }
    validate_schema(&connection)
}

/// Opens only a completed main-database snapshot. WAL state is deliberately
/// refused: immutable SQLite opens ignore a WAL and could otherwise observe a
/// stale acknowledgement, while a normal read-only WAL open may touch the SHM
/// family on some VFSes. The caller can safely fall back to lexical search.
pub(crate) fn open_passive_snapshot(root: &Path) -> Result<Option<Connection>> {
    if !passive_control_exists(root)? {
        return Ok(None);
    }
    let path = control_path(root);
    refuse_passive_sidecar(&path, OsStr::new("-wal"), "WAL")?;
    refuse_passive_sidecar(&path, OsStr::new("-journal"), "rollback journal")?;
    let mut uri = Url::from_file_path(&path).map_err(|()| {
        SemanticVectorStoreError::passive_snapshot_unavailable(format!(
            "semantic control metadata path cannot be represented as a file URI: {}",
            path.display()
        ))
    })?;
    uri.query_pairs_mut()
        .append_pair("mode", "ro")
        .append_pair("immutable", "1");
    let connection = Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .with_context(|| format!("open passive semantic control metadata {}", path.display()))?;
    connection.execute_batch("PRAGMA query_only = ON;")?;
    validate_schema(&connection)?;
    Ok(Some(connection))
}

/// Resolves only parent components so SQLite receives a canonical absolute
/// path on platforms whose NOFOLLOW VFS rejects `/var`-style parent symlinks.
/// The semantic root and final database component remain unresolved and are
/// separately rejected if either is itself a symlink.
pub(crate) fn passive_snapshot_root(root: &Path) -> Result<Option<PathBuf>> {
    let name = root.file_name().ok_or_else(|| {
        SemanticVectorStoreError::passive_snapshot_unavailable(format!(
            "semantic vector root has no final path component: {}",
            root.display()
        ))
    })?;
    let parent = root
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = match fs::canonicalize(parent) {
        Ok(parent) => parent,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("resolve semantic vector parent {}", parent.display()));
        }
    };
    let resolved = parent.join(name);
    if passive_control_exists(&resolved)? {
        Ok(Some(resolved))
    } else {
        Ok(None)
    }
}

pub(crate) fn passive_control_exists(root: &Path) -> Result<bool> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(
                SemanticVectorStoreError::passive_snapshot_unavailable(format!(
                    "refusing semantic vector root symlink or non-directory {}",
                    root.display()
                ))
                .into(),
            );
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect semantic vector root {}", root.display()));
        }
    }
    let path = control_path(root);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            SemanticVectorStoreError::passive_snapshot_unavailable(format!(
                "refusing semantic control metadata symlink or non-file {}",
                path.display()
            ))
            .into(),
        ),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("inspect semantic control metadata {}", path.display())),
    }
}

fn refuse_passive_sidecar(path: &Path, suffix: &OsStr, kind: &str) -> Result<()> {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    let sidecar = PathBuf::from(sidecar);
    match fs::symlink_metadata(&sidecar) {
        Ok(_) => Err(SemanticVectorStoreError::passive_snapshot_unavailable(format!(
            "semantic control {kind} {} is present; a passive immutable snapshot would not be exact",
            sidecar.display()
        ))
        .into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SemanticVectorStoreError::passive_snapshot_unavailable(format!(
            "semantic control {kind} {} could not be inspected: {error}",
            sidecar.display()
        ))
        .into()),
    }
}

fn control_path(root: &Path) -> PathBuf {
    root.join(CONTROL_FILE)
}

/// Resolves parent components while preserving no-follow checks for the
/// semantic root and final database component.
fn canonical_control_root(root: &Path) -> Result<PathBuf> {
    let name = root.file_name().ok_or_else(|| {
        SemanticVectorStoreError::unavailable(format!(
            "semantic vector root has no final path component: {}",
            root.display()
        ))
    })?;
    let parent = root
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent)
        .with_context(|| format!("resolve semantic vector parent {}", parent.display()))?;
    let resolved = parent.join(name);
    validate_root(&resolved, false)?;
    Ok(resolved)
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

    const V6_TABLES: [&str; 3] = [
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
        assert_eq!(user_tables(&connection)?, V6_TABLES);
        assert_eq!(
            connection.query_row(
                "SELECT COUNT(*) FROM semantic_maintenance_state
                 WHERE key = 'projection_model_contract'",
                [],
                |row| row.get::<_, u64>(0),
            )?,
            0,
            "the mutable control database must not carry model identity"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_control_opens_resolve_parent_symlinks_but_reject_the_database_symlink() -> Result<()>
    {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir()?;
        let real_parent = temporary.path().join("real-parent");
        fs::create_dir(&real_parent)?;
        let alias_parent = temporary.path().join("alias-parent");
        symlink(&real_parent, &alias_parent)?;
        let alias_root = alias_parent.join("semantic");
        drop(open_writable(&alias_root)?);
        assert!(open_read_only(&alias_root)?.is_some());
        assert!(real_parent.join("semantic/state.sqlite").is_file());

        let database = real_parent.join("semantic/state.sqlite");
        let real_database = real_parent.join("semantic/state.sqlite.real");
        fs::rename(&database, &real_database)?;
        symlink(&real_database, &database)?;
        assert!(open_read_only(&alias_root).is_err());
        Ok(())
    }

    #[test]
    fn v4_upgrade_removes_obsolete_source_receipts_transactionally() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        create_v4_fixture(temporary.path())?;

        let connection = open_writable(temporary.path())?;
        assert_eq!(pragma_i64(&connection, "user_version")?, 6);
        assert_eq!(user_tables(&connection)?, V6_TABLES);
        assert_eq!(user_table_count(&connection)?, V6_TABLES.len());
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

    #[test]
    fn v5_filter_unaware_acknowledgement_is_discarded_for_rebuild() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let connection = Connection::open(control_path(temporary.path()))?;
        connection.execute_batch(
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
            CREATE TABLE semantic_maintenance_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            INSERT INTO semantic_maintenance_state(key, value)
                VALUES ('core_semantic_acknowledgement_v1',
                        '{"semantic_documents":2,"projected_documents":1}');
            "#,
        )?;
        connection.pragma_update(None, "application_id", CONTROL_APPLICATION_ID)?;
        connection.pragma_update(None, "user_version", 5)?;
        drop(connection);

        let connection = open_writable(temporary.path())?;
        assert_eq!(pragma_i64(&connection, "user_version")?, 6);
        assert_eq!(user_tables(&connection)?, V6_TABLES);
        assert_eq!(
            connection.query_row(
                "SELECT COUNT(*) FROM semantic_maintenance_state WHERE key = ?1",
                ["core_semantic_acknowledgement_v1"],
                |row| row.get::<_, u64>(0),
            )?,
            0
        );
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
