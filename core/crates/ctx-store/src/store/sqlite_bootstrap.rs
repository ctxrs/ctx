use super::migration_repairs::{
    repair_historical_tool_order_seq_migration_version,
    repair_partial_session_subagent_archival_migration,
};
use super::*;

const SESSION_REASONING_EFFORT_MIGRATION_VERSION: i64 = 46;
const TOOL_DISPLAY_FIELDS_MIGRATION_VERSION: i64 = 47;
const TOOL_DISPLAY_FIELDS_MIGRATION_DESCRIPTION: &str = "tool display fields";
const WORKSPACE_MESSAGE_INDEX_MIGRATION_VERSION: i64 = 51;
const WORKSPACE_MESSAGE_INDEX_MIGRATION_DESCRIPTION: &str = "drop workspace message index";

static STORE_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

impl Store {
    pub async fn open_sqlite(path: impl AsRef<Path>, max_connections: Option<u32>) -> Result<Self> {
        let path = path.as_ref();
        let path_str = path.to_string_lossy();
        if path_str != ":memory:" {
            if let Some(parent) = path.parent() {
                ctx_fs::permissions::ensure_private_dir(parent).await?;
            }
            if !path.exists() {
                ctx_fs::permissions::write_private_file_atomic(path, b"").await?;
            } else {
                ctx_fs::permissions::harden_private_file_if_exists(path).await?;
            }
        }
        let sqlite_url = format!("sqlite://{}", path.to_string_lossy());
        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections.unwrap_or(5))
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    sqlx::query("PRAGMA busy_timeout = 5000")
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query("PRAGMA foreign_keys = ON")
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query("PRAGMA synchronous = NORMAL")
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query("PRAGMA secure_delete = ON")
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .connect(&sqlite_url)
            .await?;
        repair_historical_tool_order_seq_migration_version(&pool).await?;
        ensure_sqlite_journal_mode_wal(&pool).await?;
        repair_duplicate_tool_display_migration_version(&pool).await?;
        repair_workspace_message_index_migration_versions(&pool).await?;
        repair_partial_session_subagent_archival_migration(&pool).await?;
        STORE_MIGRATOR.run(&pool).await?;
        if path_str != ":memory:" {
            ctx_fs::permissions::harden_sqlite_file_family(path).await?;
        }
        let event_log = Arc::new(EventLogRuntime::load(&pool).await?);
        let active_head_projection = Arc::new(ActiveHeadProjectionRuntime::new());
        let store = Self {
            pool,
            sqlite_path: (path_str != ":memory:").then(|| path.to_path_buf()),
            event_log,
            active_head_projection,
            write_gate: Arc::new(Mutex::new(())),
            _lease_guard: None,
        };
        store.event_log.start_persister(store.clone())?;
        store
            .active_head_projection
            .start_projector(store.clone())?;
        Ok(store)
    }
}

async fn repair_duplicate_tool_display_migration_version(pool: &Pool<Sqlite>) -> Result<()> {
    let migrations_table_exists = sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations')",
    )
    .fetch_one(pool)
    .await?;
    if migrations_table_exists == 0 {
        return Ok(());
    }

    let tool_display_version = sqlx::query_scalar::<_, i64>(
        "SELECT version FROM _sqlx_migrations WHERE description = ? LIMIT 1",
    )
    .bind(TOOL_DISPLAY_FIELDS_MIGRATION_DESCRIPTION)
    .fetch_optional(pool)
    .await?;

    if tool_display_version != Some(SESSION_REASONING_EFFORT_MIGRATION_VERSION) {
        return Ok(());
    }

    let renamed_version_exists = sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version = ?)",
    )
    .bind(TOOL_DISPLAY_FIELDS_MIGRATION_VERSION)
    .fetch_one(pool)
    .await?;

    if renamed_version_exists == 0 {
        sqlx::query(
            "UPDATE _sqlx_migrations SET version = ? WHERE version = ? AND description = ?",
        )
        .bind(TOOL_DISPLAY_FIELDS_MIGRATION_VERSION)
        .bind(SESSION_REASONING_EFFORT_MIGRATION_VERSION)
        .bind(TOOL_DISPLAY_FIELDS_MIGRATION_DESCRIPTION)
        .execute(pool)
        .await?;
    } else {
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version = ? AND description = ?")
            .bind(SESSION_REASONING_EFFORT_MIGRATION_VERSION)
            .bind(TOOL_DISPLAY_FIELDS_MIGRATION_DESCRIPTION)
            .execute(pool)
            .await?;
    }

    Ok(())
}

async fn ensure_sqlite_journal_mode_wal(pool: &Pool<Sqlite>) -> Result<()> {
    let current_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(pool)
        .await?;
    if current_mode.eq_ignore_ascii_case("wal") {
        return Ok(());
    }

    let _: String = sqlx::query_scalar("PRAGMA journal_mode = WAL")
        .fetch_one(pool)
        .await?;
    Ok(())
}

async fn repair_workspace_message_index_migration_versions(pool: &Pool<Sqlite>) -> Result<()> {
    let migrations_table_exists = sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations')",
    )
    .fetch_one(pool)
    .await?;
    if migrations_table_exists == 0 {
        return Ok(());
    }

    let rows =
        sqlx::query("SELECT version FROM _sqlx_migrations WHERE description = ? ORDER BY version")
            .bind(WORKSPACE_MESSAGE_INDEX_MIGRATION_DESCRIPTION)
            .fetch_all(pool)
            .await?;

    for row in rows {
        let version: i64 = row.try_get("version")?;
        if version == WORKSPACE_MESSAGE_INDEX_MIGRATION_VERSION {
            continue;
        }

        let repaired_version_exists = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version = ?)",
        )
        .bind(WORKSPACE_MESSAGE_INDEX_MIGRATION_VERSION)
        .fetch_one(pool)
        .await?;

        if repaired_version_exists == 0 {
            sqlx::query(
                "UPDATE _sqlx_migrations SET version = ? WHERE version = ? AND description = ?",
            )
            .bind(WORKSPACE_MESSAGE_INDEX_MIGRATION_VERSION)
            .bind(version)
            .bind(WORKSPACE_MESSAGE_INDEX_MIGRATION_DESCRIPTION)
            .execute(pool)
            .await?;
        } else {
            sqlx::query("DELETE FROM _sqlx_migrations WHERE version = ? AND description = ?")
                .bind(version)
                .bind(WORKSPACE_MESSAGE_INDEX_MIGRATION_DESCRIPTION)
                .execute(pool)
                .await?;
        }
    }

    Ok(())
}
