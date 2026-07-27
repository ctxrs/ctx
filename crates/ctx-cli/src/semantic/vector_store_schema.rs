use std::{collections::HashSet, fmt, fs, path::Path, sync::Mutex, time::Duration as StdDuration};

#[cfg(ctx_sqlite_vec)]
use std::os::raw::c_char;

use anyhow::{Context, Result};
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};

use super::{
    health_search::{
        create_private_dir_all, private_create_new_file, secure_semantic_vector_permissions,
    },
    model_contract::{semantic_model_key, SEMANTIC_DIMENSIONS},
    runtime_limits::SEMANTIC_VECTOR_BUSY_TIMEOUT_MS,
    vector_store::SemanticVectorStore,
};

pub(super) const SEMANTIC_VECTOR_SCHEMA_VERSION: i64 = 6;
pub(super) const SEMANTIC_VECTOR_APPLICATION_ID: i64 = 0x4354_5856; // "CTXV"
pub(super) const SEMANTIC_VECTOR_MODEL_KEY_STATE: &str = "projection_model_key";
pub(super) const SEMANTIC_SQLITE_VEC0_MAX_K: usize = 4_096;
pub(super) const SEMANTIC_SQLITE_VEC0_INITIAL_OVERFETCH_DIVISOR: usize = 4;
pub(super) const SEMANTIC_VECTOR_BACKEND_SQLITE_VEC: &str = "sqlite_vec0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SemanticVectorFailureKind {
    Unavailable,
    StorageConflict,
    ResetRequired,
    NewerSchema,
}

#[derive(Debug)]
pub(super) struct SemanticVectorStoreError {
    pub(super) kind: SemanticVectorFailureKind,
    message: String,
}

impl SemanticVectorStoreError {
    pub(super) fn unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: SemanticVectorFailureKind::Unavailable,
            message: message.into(),
        }
    }

    pub(super) fn storage_conflict(message: impl Into<String>) -> Self {
        Self {
            kind: SemanticVectorFailureKind::StorageConflict,
            message: message.into(),
        }
    }

    pub(super) fn reset_required(message: impl Into<String>) -> Self {
        Self {
            kind: SemanticVectorFailureKind::ResetRequired,
            message: message.into(),
        }
    }

    pub(super) fn newer_schema(found: i64) -> Self {
        Self {
            kind: SemanticVectorFailureKind::NewerSchema,
            message: format!(
                "semantic vector store schema version {found} is newer than supported version {SEMANTIC_VECTOR_SCHEMA_VERSION}"
            ),
        }
    }
}

impl fmt::Display for SemanticVectorStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SemanticVectorStoreError {}

pub(super) fn semantic_vector_failure_kind(
    error: &anyhow::Error,
) -> Option<SemanticVectorFailureKind> {
    error
        .downcast_ref::<SemanticVectorStoreError>()
        .map(|error| error.kind)
}

impl SemanticVectorStore {
    pub(super) fn open(path: &Path) -> Result<Self> {
        register_sqlite_vec_auto_extension()?;
        if let Some(parent) = path.parent() {
            create_private_dir_all(parent)?;
        }
        validate_semantic_path(path)?;
        if checked_file_identity(path)?.is_none() {
            match private_create_new_file(path) {
                Ok(file) => drop(file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("create semantic vector store {}", path.display())
                    });
                }
            }
        }
        let identity = checked_file_identity(path)?.ok_or_else(|| {
            SemanticVectorStoreError::unavailable(
                "semantic vector store disappeared before SQLite open",
            )
        })?;
        run_before_sqlite_open_hook(path)?;
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|error| semantic_open_error(path, error))?;
        require_same_file(path, &identity)?;
        connection.busy_timeout(StdDuration::from_millis(SEMANTIC_VECTOR_BUSY_TIMEOUT_MS))?;
        let mut store = Self { conn: connection };
        store.prepare_writable(path, &identity)?;
        require_same_file(path, &identity)?;
        store.conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA secure_delete = ON;
            "#,
        )?;
        require_same_file(path, &identity)?;
        secure_semantic_vector_permissions(path)?;
        Ok(store)
    }

    pub(super) fn open_read_only(path: &Path) -> Result<Option<Self>> {
        validate_semantic_path(path)?;
        let Some(identity) = checked_file_identity(path)? else {
            return Ok(None);
        };
        register_sqlite_vec_auto_extension()?;
        run_before_sqlite_open_hook(path)?;
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|error| semantic_open_error(path, error))?;
        require_same_file(path, &identity)?;
        connection.busy_timeout(StdDuration::from_millis(SEMANTIC_VECTOR_BUSY_TIMEOUT_MS))?;
        let store = Self { conn: connection };
        let application_id =
            semantic_vector_application_id(&store.conn).map_err(semantic_inspection_error)?;
        let user_version =
            semantic_vector_user_version(&store.conn).map_err(semantic_inspection_error)?;
        if application_id == 0
            && matches!(user_version, 3 | 5)
            && recognized_legacy_schema(&store.conn, user_version)?
        {
            return Err(SemanticVectorStoreError::reset_required(format!(
                "recognized legacy semantic vector store v{user_version} requires a writable v6 rebuild"
            ))
            .into());
        }
        store.validate_v6_schema(application_id, user_version)?;
        require_same_file(path, &identity)?;
        Ok(Some(store))
    }

    fn prepare_writable(&mut self, path: &Path, identity: &FileIdentity) -> Result<()> {
        let application_id =
            semantic_vector_application_id(&self.conn).map_err(semantic_inspection_error)?;
        let user_version =
            semantic_vector_user_version(&self.conn).map_err(semantic_inspection_error)?;
        if application_id == SEMANTIC_VECTOR_APPLICATION_ID {
            self.validate_v6_schema(application_id, user_version)?;
            return Ok(());
        }
        if application_id == 0
            && matches!(user_version, 3 | 5)
            && recognized_legacy_schema(&self.conn, user_version)?
        {
            require_same_file(path, identity)?;
            self.conn.execute_batch("PRAGMA secure_delete = ON;")?;
            reset_legacy_to_v6(&mut self.conn, path, identity, user_version)?;
            return self.validate_v6_schema(
                SEMANTIC_VECTOR_APPLICATION_ID,
                SEMANTIC_VECTOR_SCHEMA_VERSION,
            );
        }
        if application_id == 0 && user_version == 0 && sqlite_user_table_count(&self.conn)? == 0 {
            require_same_file(path, identity)?;
            self.conn.execute_batch("PRAGMA secure_delete = ON;")?;
            let transaction = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Exclusive)?;
            require_same_file(path, identity)?;
            if semantic_vector_application_id(&transaction)? != 0
                || semantic_vector_user_version(&transaction)? != 0
                || sqlite_user_table_count(&transaction)? != 0
            {
                return Err(SemanticVectorStoreError::storage_conflict(
                    "semantic vector store changed before v6 creation",
                )
                .into());
            }
            create_v6_schema(&transaction)?;
            transaction.commit()?;
            return self.validate_v6_schema(
                SEMANTIC_VECTOR_APPLICATION_ID,
                SEMANTIC_VECTOR_SCHEMA_VERSION,
            );
        }
        Err(SemanticVectorStoreError::storage_conflict(
            "refusing to replace an unrecognized SQLite database at the semantic sidecar path",
        )
        .into())
    }

    fn validate_v6_schema(&self, application_id: i64, user_version: i64) -> Result<()> {
        if application_id != SEMANTIC_VECTOR_APPLICATION_ID {
            return Err(SemanticVectorStoreError::storage_conflict(
                "unrecognized SQLite application id at the semantic sidecar path",
            )
            .into());
        }
        if user_version > SEMANTIC_VECTOR_SCHEMA_VERSION {
            return Err(SemanticVectorStoreError::newer_schema(user_version).into());
        }
        if user_version != SEMANTIC_VECTOR_SCHEMA_VERSION {
            return Err(SemanticVectorStoreError::reset_required(format!(
                "semantic vector store has schema version {user_version}; manual v6 rebuild required"
            ))
            .into());
        }
        ensure_sqlite_vec_runtime(&self.conn)?;
        semantic_schema_inspection_result(validate_v6_objects(&self.conn))?;
        let model_key = self
            .conn
            .query_row(
                "SELECT value FROM semantic_maintenance_state WHERE key = ?1",
                [SEMANTIC_VECTOR_MODEL_KEY_STATE],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(semantic_inspection_error)?;
        if model_key.as_deref() != Some(semantic_model_key()) {
            return Err(SemanticVectorStoreError::reset_required(
                "semantic vector store model does not match this build; manual rebuild required",
            )
            .into());
        }
        let counts = self
            .conn
            .query_row(
                "SELECT embedded_items, embedded_chunks, dirty_items
                 FROM semantic_index_stats WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(semantic_inspection_error)?;
        if !matches!(counts, Some((items, chunks, dirty)) if items >= 0 && chunks >= items && dirty >= 0)
        {
            return Err(SemanticVectorStoreError::reset_required(
                "semantic vector store has invalid v6 cached counts; manual rebuild required",
            )
            .into());
        }
        Ok(())
    }
}

fn create_v6_schema(transaction: &Transaction<'_>) -> Result<()> {
    transaction
        .execute_batch(&format!(
            r#"
            CREATE TABLE event_embedding_chunks (
                chunk_id INTEGER PRIMARY KEY,
                event_id TEXT NOT NULL,
                event_seq INTEGER NOT NULL,
                chunk_index INTEGER NOT NULL,
                source_text_sha256 TEXT NOT NULL,
                start_char INTEGER NOT NULL CHECK(start_char >= 0),
                end_char INTEGER NOT NULL CHECK(end_char >= start_char),
                UNIQUE(event_id, chunk_index)
            );
            CREATE INDEX idx_event_embedding_chunks_event
                ON event_embedding_chunks(event_id);
            CREATE INDEX idx_event_embedding_chunks_prune_anchor
                ON event_embedding_chunks(event_seq DESC, event_id DESC)
                WHERE chunk_index = 0;
            CREATE VIRTUAL TABLE event_embedding_vec0
                USING vec0(embedding float[{SEMANTIC_DIMENSIONS}] distance_metric=cosine);
            CREATE TABLE semantic_index_stats (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                embedded_items INTEGER NOT NULL CHECK(embedded_items >= 0),
                embedded_chunks INTEGER NOT NULL CHECK(embedded_chunks >= 0),
                dirty_items INTEGER NOT NULL CHECK(dirty_items >= 0)
            );
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
            "#
        ))
        .map_err(|error| semantic_vec_error("create v6 sqlite-vec schema", error))?;
    transaction.execute(
        "INSERT INTO semantic_maintenance_state(key, value) VALUES (?1, ?2)",
        params![SEMANTIC_VECTOR_MODEL_KEY_STATE, semantic_model_key()],
    )?;
    transaction.execute(
        "INSERT INTO semantic_index_stats
         (id, embedded_items, embedded_chunks, dirty_items) VALUES (1, 0, 0, 0)",
        [],
    )?;
    transaction.pragma_update(None, "application_id", SEMANTIC_VECTOR_APPLICATION_ID)?;
    transaction.pragma_update(None, "user_version", SEMANTIC_VECTOR_SCHEMA_VERSION)?;
    Ok(())
}

fn reset_legacy_to_v6(
    connection: &mut Connection,
    path: &Path,
    identity: &FileIdentity,
    expected_version: i64,
) -> Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Exclusive)?;
    require_same_file(path, identity)?;
    if semantic_vector_application_id(&transaction)? != 0
        || semantic_vector_user_version(&transaction)? != expected_version
        || !recognized_legacy_schema(&transaction, expected_version)?
    {
        return Err(SemanticVectorStoreError::storage_conflict(
            "legacy semantic sidecar changed before its v6 reset",
        )
        .into());
    }
    transaction.execute_batch(
        r#"
        DROP TABLE IF EXISTS event_embedding_vec0;
        DROP TABLE IF EXISTS event_embedding_vec0_meta;
        DROP TABLE IF EXISTS semantic_maintenance_state;
        DROP TABLE semantic_dirty_events;
        DROP TABLE semantic_index_stats;
        DROP TABLE event_embedding_chunks;
        DROP TABLE event_embeddings;
        DROP TABLE embedding_models;
        "#,
    )?;
    create_v6_schema(&transaction)?;
    transaction.commit()?;
    Ok(())
}

fn validate_v6_objects(connection: &Connection) -> Result<()> {
    let expected_tables = names(&[
        "event_embedding_chunks",
        "event_embedding_vec0",
        "event_embedding_vec0_info",
        "event_embedding_vec0_chunks",
        "event_embedding_vec0_rowids",
        "event_embedding_vec0_vector_chunks00",
        "semantic_index_stats",
        "semantic_dirty_events",
        "semantic_maintenance_state",
    ])
    .into_iter()
    .collect::<HashSet<_>>();
    let actual_tables = sqlite_user_tables(connection)?
        .into_iter()
        .collect::<HashSet<_>>();
    let vec_sql = format!(
        "CREATE VIRTUAL TABLE event_embedding_vec0 USING vec0(embedding float[{SEMANTIC_DIMENSIONS}] distance_metric=cosine)"
    );
    let valid = actual_tables == expected_tables
        && table_columns(connection, "event_embedding_chunks")?
            == names(&[
                "chunk_id",
                "event_id",
                "event_seq",
                "chunk_index",
                "source_text_sha256",
                "start_char",
                "end_char",
            ])
        && table_columns(connection, "semantic_index_stats")?
            == names(&["id", "embedded_items", "embedded_chunks", "dirty_items"])
        && table_columns(connection, "semantic_dirty_events")?
            == names(&[
                "event_id",
                "queued_at_ms",
                "priority_seq",
                "reason",
                "attempts",
            ])
        && table_columns(connection, "semantic_maintenance_state")? == names(&["key", "value"])
        && schema_sql_matches(connection, "table", "event_embedding_vec0", &vec_sql)?;
    if valid {
        Ok(())
    } else {
        Err(SemanticVectorStoreError::reset_required(
            "semantic vector store does not exactly match v6; manual rebuild required",
        )
        .into())
    }
}

fn recognized_legacy_schema(connection: &Connection, version: i64) -> Result<bool> {
    if !matches!(version, 3 | 5) {
        return Ok(false);
    }
    let base = names(&[
        "embedding_models",
        "event_embeddings",
        "event_embedding_chunks",
        "semantic_index_stats",
        "semantic_dirty_events",
    ]);
    let mut allowed = base.iter().cloned().collect::<HashSet<_>>();
    if version == 5 {
        allowed.extend(names(&[
            "semantic_maintenance_state",
            "event_embedding_vec0",
            "event_embedding_vec0_meta",
            "event_embedding_vec0_info",
            "event_embedding_vec0_chunks",
            "event_embedding_vec0_rowids",
            "event_embedding_vec0_vector_chunks00",
        ]));
    }
    let tables = sqlite_user_tables(connection)?;
    if !base.iter().all(|table| tables.contains(table))
        || tables.iter().any(|table| !allowed.contains(table))
    {
        return Ok(false);
    }
    let signatures = [
        ("embedding_models", "model_key,backend,model_id,dimensions,distance,normalized,created_at_ms"),
        ("event_embeddings", "event_id,model_key,history_record_id,session_id,event_seq,text_sha256,preview_text,dimensions,embedding_f32,embedded_at_ms"),
        ("event_embedding_chunks", "event_id,model_key,history_record_id,session_id,event_seq,chunk_index,chunk_count,source_text_sha256,chunk_text_sha256,chunk_text,start_char,end_char,dimensions,embedding_f32,embedded_at_ms"),
        ("semantic_index_stats", "model_key,embedded_items,embedded_chunks,updated_at_ms"),
        ("semantic_dirty_events", "event_id,model_key,queued_at_ms,priority_seq,reason,attempts"),
    ];
    for (table, expected) in signatures {
        if table_columns(connection, table)?.join(",") != expected {
            return Ok(false);
        }
    }
    let vec_tables = tables
        .iter()
        .filter(|table| table.starts_with("event_embedding_vec0"))
        .count();
    Ok(version != 5 || matches!(vec_tables, 0 | 6))
}

fn ensure_sqlite_vec_runtime(connection: &Connection) -> Result<()> {
    connection
        .query_row("SELECT vec_version()", [], |row| row.get::<_, String>(0))
        .map(|_| ())
        .map_err(|error| semantic_vec_error("sqlite-vec runtime is unavailable", error))
}

fn semantic_vector_user_version(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row("PRAGMA user_version", [], |row| row.get(0))
}

fn semantic_vector_application_id(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row("PRAGMA application_id", [], |row| row.get(0))
}

fn sqlite_user_table_count(connection: &Connection) -> Result<usize> {
    Ok(sqlite_user_tables(connection)?.len())
}

fn sqlite_user_tables(connection: &Connection) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT name FROM sqlite_schema
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )?;
    let rows = statement.query_map([], |row| row.get(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn table_columns(connection: &Connection, table: &str) -> Result<Vec<String>> {
    let escaped = table.replace('"', "\"\"");
    let mut statement = connection.prepare(&format!("PRAGMA table_info(\"{escaped}\")"))?;
    let rows = statement.query_map([], |row| row.get(1))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn schema_sql_matches(
    connection: &Connection,
    kind: &str,
    name: &str,
    expected: &str,
) -> Result<bool> {
    let actual = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type = ?1 AND name = ?2",
            params![kind, name],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(actual.is_some_and(|actual| normalize_sql(&actual) == normalize_sql(expected)))
}

fn normalize_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FileIdentity {
    #[cfg(unix)]
    Unix(u64, u64),
    #[cfg(windows)]
    Windows(u64, [u8; 16]),
    #[cfg(not(any(unix, windows)))]
    Metadata(u64, u64),
}

fn validate_semantic_path(path: &Path) -> Result<()> {
    checked_file_identity(path)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut value = path.as_os_str().to_os_string();
        value.push(suffix);
        checked_file_identity(Path::new(&value))?;
    }
    Ok(())
}

fn checked_file_identity(path: &Path) -> Result<Option<FileIdentity>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect semantic sidecar {}", path.display()))
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(SemanticVectorStoreError::unavailable(format!(
            "refusing semantic sidecar symlink or non-file target {}",
            path.display()
        ))
        .into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Ok(Some(FileIdentity::Unix(metadata.dev(), metadata.ino())))
    }
    #[cfg(windows)]
    {
        use std::{
            mem::size_of,
            os::windows::{
                fs::{MetadataExt as _, OpenOptionsExt as _},
                io::AsRawHandle as _,
            },
        };
        use windows_sys::Win32::Storage::FileSystem::{
            FileIdInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ID_INFO,
            FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        let file = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .with_context(|| format!("open semantic sidecar identity {}", path.display()))?;
        let opened = file
            .metadata()
            .with_context(|| format!("inspect semantic sidecar identity {}", path.display()))?;
        if !opened.is_file() || opened.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(SemanticVectorStoreError::unavailable(format!(
                "refusing semantic sidecar reparse point or non-file target {}",
                path.display()
            ))
            .into());
        }
        let mut identity = FILE_ID_INFO::default();
        // SAFETY: the file owns a live handle and identity is a valid output buffer.
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                (&mut identity as *mut FILE_ID_INFO).cast(),
                size_of::<FILE_ID_INFO>() as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("read semantic sidecar identity {}", path.display()));
        }
        Ok(Some(FileIdentity::Windows(
            identity.VolumeSerialNumber,
            identity.FileId.Identifier,
        )))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|value| value.as_nanos() as u64)
            .unwrap_or_default();
        Ok(Some(FileIdentity::Metadata(metadata.len(), modified)))
    }
}

fn require_same_file(path: &Path, expected: &FileIdentity) -> Result<()> {
    if checked_file_identity(path)?.as_ref() == Some(expected) {
        Ok(())
    } else {
        Err(SemanticVectorStoreError::unavailable(
            "semantic vector store identity changed during open or schema inspection",
        )
        .into())
    }
}

fn semantic_open_error(path: &Path, error: rusqlite::Error) -> anyhow::Error {
    match sqlite_error_code(&error) {
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase) => {
            SemanticVectorStoreError::reset_required(format!(
                "semantic vector store cannot be safely opened; manual rebuild required for {}: {error}",
                path.display()
            ))
            .into()
        }
        _ => anyhow::Error::new(error)
            .context(format!("open semantic vector store {}", path.display())),
    }
}

fn semantic_inspection_error(error: rusqlite::Error) -> anyhow::Error {
    match sqlite_error_code(&error) {
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase) => {
            SemanticVectorStoreError::reset_required(format!(
                "semantic vector store ownership cannot be safely confirmed; manual rebuild required: {error}"
            ))
            .into()
        }
        _ => error.into(),
    }
}

pub(super) fn semantic_owned_sidecar_result<T>(result: Result<T>) -> Result<T> {
    result.map_err(|error| match semantic_sqlite_error_code(&error) {
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase) => {
            SemanticVectorStoreError::reset_required(format!(
                "semantic vector store operation failed; manual rebuild required: {error:#}"
            ))
            .into()
        }
        _ => error,
    })
}

fn semantic_schema_inspection_result<T>(result: Result<T>) -> Result<T> {
    result.map_err(|error| match semantic_sqlite_error_code(&error) {
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase) => {
            SemanticVectorStoreError::reset_required(format!(
                "semantic vector store ownership cannot be safely confirmed; manual rebuild required: {error:#}"
            ))
            .into()
        }
        _ => error,
    })
}

pub(super) fn semantic_sqlite_error_code(error: &anyhow::Error) -> Option<rusqlite::ErrorCode> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<rusqlite::Error>()
            .and_then(sqlite_error_code)
    })
}

fn sqlite_error_code(error: &rusqlite::Error) -> Option<rusqlite::ErrorCode> {
    let rusqlite::Error::SqliteFailure(failure, _) = error else {
        return None;
    };
    Some(failure.code)
}

fn semantic_vec_error(operation: &str, error: rusqlite::Error) -> anyhow::Error {
    match sqlite_error_code(&error) {
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase) => {
            SemanticVectorStoreError::reset_required(format!(
                "{operation}; manual rebuild required: {error}"
            ))
            .into()
        }
        _ => SemanticVectorStoreError::unavailable(format!("{operation}: {error}")).into(),
    }
}

#[cfg(ctx_sqlite_vec)]
fn register_sqlite_vec_auto_extension() -> Result<()> {
    static REGISTERED: Mutex<bool> = Mutex::new(false);
    let mut registered = REGISTERED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *registered {
        return Ok(());
    }
    let rc = unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(
            sqlite_vec::sqlite3_vec_init as *const ()
        )))
    };
    if rc != rusqlite::ffi::SQLITE_OK {
        return Err(SemanticVectorStoreError::unavailable(format!(
            "sqlite-vec registration failed with SQLite status {rc}"
        ))
        .into());
    }
    *registered = true;
    Ok(())
}

#[cfg(not(ctx_sqlite_vec))]
fn register_sqlite_vec_auto_extension() -> Result<()> {
    Err(SemanticVectorStoreError::unavailable("sqlite-vec is not available in this build").into())
}

#[cfg(test)]
type BeforeSqliteOpenHook = Box<dyn FnOnce(&Path) -> Result<()>>;

#[cfg(test)]
thread_local! {
    static BEFORE_SQLITE_OPEN_HOOK: std::cell::RefCell<Option<BeforeSqliteOpenHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn set_semantic_before_sqlite_open_hook(
    hook: impl FnOnce(&Path) -> Result<()> + 'static,
) {
    BEFORE_SQLITE_OPEN_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
fn run_before_sqlite_open_hook(path: &Path) -> Result<()> {
    let hook = BEFORE_SQLITE_OPEN_HOOK.with(|slot| slot.borrow_mut().take());
    hook.map_or(Ok(()), |hook| hook(path))
}

#[cfg(not(test))]
fn run_before_sqlite_open_hook(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(all(test, windows))]
mod windows_file_identity_tests {
    use super::*;

    #[test]
    fn file_identity_is_stable_across_in_place_writes_and_changes_on_replacement() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("vectors.sqlite");
        let displaced = temporary.path().join("displaced.sqlite");
        fs::write(&path, b"initial")?;
        let initial = checked_file_identity(&path)?.expect("identity");

        fs::write(&path, b"longer in-place contents")?;
        assert_eq!(checked_file_identity(&path)?.as_ref(), Some(&initial));

        fs::rename(&path, &displaced)?;
        fs::write(&path, b"replacement")?;
        assert_ne!(checked_file_identity(&path)?.as_ref(), Some(&initial));
        Ok(())
    }
}
