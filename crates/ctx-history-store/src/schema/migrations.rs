use rusqlite::Connection;

use crate::schema::ddl::{
    ensure_columns, table_exists, table_has_column, CATALOG_SESSION_IMPORT_STATE_COLUMNS,
    CREATE_TABLES_SQL, HISTORY_RECORD_COLUMNS,
};
use crate::schema::indexes::INDEXES_SQL;
use crate::schema::views::{
    create_stable_sql_views, drop_stable_sql_views, stable_sql_views_exist,
};
use crate::search::projections::rebuild_search_projection;
use crate::{Result, StoreError};

pub(crate) fn run_migrations(conn: &Connection, user_version: i64) -> Result<()> {
    if user_version < 1 {
        migrate_to_v1(conn)?;
    }
    if user_version < 2 {
        migrate_to_v2(conn)?;
    }
    if user_version < 3 {
        migrate_to_v3(conn)?;
    }
    if user_version < 4 {
        migrate_to_v4(conn)?;
    }
    if user_version < 5 {
        migrate_to_v5(conn)?;
    }
    if user_version < 6 {
        migrate_to_v6(conn)?;
    }
    if user_version < 7 {
        migrate_to_v7(conn)?;
    }
    if user_version < 8 {
        migrate_to_v8(conn)?;
    }
    if user_version < 9 {
        migrate_to_v9(conn)?;
    }
    if user_version < 10 {
        migrate_to_v10(conn)?;
    }
    if user_version < 11 {
        migrate_to_v11(conn)?;
    }
    if user_version < 12 {
        migrate_to_v12(conn)?;
    }
    if user_version < 13 {
        migrate_to_v13(conn)?;
    }
    if user_version < 14 {
        migrate_to_v14(conn)?;
    }
    if user_version < 15 {
        migrate_to_v15(conn)?;
    }
    if user_version < 16 {
        migrate_to_v16(conn)?;
    }
    if user_version < 42 {
        migrate_to_v42(conn)?;
    }
    Ok(())
}

fn migrate_to_v1(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(conn, "history_records", HISTORY_RECORD_COLUMNS)?;
        backfill_legacy_tables(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 1;")?;
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

fn migrate_to_v2(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(conn, "history_records", HISTORY_RECORD_COLUMNS)?;
        backfill_legacy_tables(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 2;")?;
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

fn migrate_to_v3(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(conn, "history_records", HISTORY_RECORD_COLUMNS)?;
        backfill_legacy_tables(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 3;")?;
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

fn migrate_to_v4(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        rebuild_capture_sources_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 4;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v5(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(
            conn,
            "catalog_sessions",
            CATALOG_SESSION_IMPORT_STATE_COLUMNS,
        )?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 5;")?;
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

fn migrate_to_v6(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(
            conn,
            "catalog_sessions",
            CATALOG_SESSION_IMPORT_STATE_COLUMNS,
        )?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 6;")?;
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

fn migrate_to_v7(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 7;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v8(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        drop_legacy_history_record_indexes(conn)?;
        rename_table_if_exists(conn, "work_record_links", "history_record_links")?;
        rename_table_if_exists(conn, "work_record_tags", "history_record_tags")?;
        rename_table_if_exists(conn, "work_records", "history_records")?;
        for table in ["sessions", "runs", "events", "summaries", "files_touched"] {
            rename_column_if_exists(conn, table, "work_record_id", "history_record_id")?;
        }
        rename_column_if_exists(
            conn,
            "history_record_links",
            "work_record_id",
            "history_record_id",
        )?;
        rename_column_if_exists(
            conn,
            "history_record_tags",
            "work_record_id",
            "history_record_id",
        )?;
        rewrite_history_table_names(conn, "sync_outbox", "local_table")?;
        rewrite_history_table_names(conn, "audit_log", "target_table")?;
        drop_fts_table_if_column_exists(conn, "event_search", "work_record_id")?;
        drop_fts_table_if_column_exists(conn, "artifact_search", "work_record_id")?;
        conn.execute_batch(CREATE_TABLES_SQL)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 8;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v9(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 9;")?;
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

fn migrate_to_v10(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 10;")?;
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

fn migrate_to_v11(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        rebuild_search_projection(conn)?;
        conn.execute_batch("PRAGMA user_version = 11;")?;
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

fn migrate_to_v12(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        invalidate_provider_import_indexes(conn)?;
        rebuild_search_projection(conn)?;
        conn.execute_batch("PRAGMA user_version = 12;")?;
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

fn migrate_to_v13(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 13;")?;
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

fn migrate_to_v14(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(
            conn,
            "catalog_sessions",
            CATALOG_SESSION_IMPORT_STATE_COLUMNS,
        )?;
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        backfill_catalog_session_import_checkpoints(conn)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 14;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v15(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 15;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v16(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 16;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v42(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 42;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn invalidate_provider_import_indexes(conn: &Connection) -> Result<()> {
    if table_exists(conn, "catalog_sessions")? {
        conn.execute(
            r#"
            UPDATE catalog_sessions
            SET indexed_at_ms = NULL,
                indexed_file_size_bytes = NULL,
                indexed_file_modified_at_ms = NULL,
                indexed_status = 'pending',
                indexed_error = NULL,
                indexed_event_count = NULL
            WHERE indexed_status = 'indexed'
            "#,
            [],
        )?;
    }
    if table_exists(conn, "source_import_files")? {
        conn.execute(
            r#"
            UPDATE source_import_files
            SET indexed_at_ms = NULL,
                indexed_file_size_bytes = NULL,
                indexed_file_modified_at_ms = NULL,
                indexed_status = 'pending',
                indexed_error = NULL
            WHERE indexed_status = 'indexed'
            "#,
            [],
        )?;
    }
    Ok(())
}

fn backfill_catalog_session_import_checkpoints(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "catalog_sessions")? {
        return Ok(());
    }
    conn.execute(
        r#"
        UPDATE catalog_sessions
        SET last_imported_at_ms = indexed_at_ms,
            last_imported_file_size_bytes = indexed_file_size_bytes,
            last_imported_file_modified_at_ms = indexed_file_modified_at_ms,
            last_imported_event_count = indexed_event_count
        WHERE last_imported_file_size_bytes IS NULL
          AND indexed_file_size_bytes IS NOT NULL
        "#,
        [],
    )?;
    Ok(())
}

fn drop_legacy_history_record_indexes(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DROP INDEX IF EXISTS idx_work_records_primary_vcs_workspace_id;
        DROP INDEX IF EXISTS idx_work_records_source_id;
        DROP INDEX IF EXISTS idx_work_records_last_activity_at_ms;
        DROP INDEX IF EXISTS idx_work_records_created_at;
        DROP INDEX IF EXISTS idx_sessions_work_record_id;
        DROP INDEX IF EXISTS idx_runs_work_record_started_at_ms;
        DROP INDEX IF EXISTS idx_runs_work_record_id;
        DROP INDEX IF EXISTS idx_events_work_record_occurred_at_ms;
        DROP INDEX IF EXISTS idx_events_work_record_id;
        DROP INDEX IF EXISTS idx_work_record_links_work_record_id;
        DROP INDEX IF EXISTS idx_work_record_links_source_id;
        DROP INDEX IF EXISTS idx_summaries_work_record_id;
        DROP INDEX IF EXISTS idx_files_touched_work_record_id;
        DROP INDEX IF EXISTS idx_work_record_tags_tag_id;
        DROP INDEX IF EXISTS idx_work_record_tags_source_id;
        "#,
    )?;
    Ok(())
}

fn rename_table_if_exists(conn: &Connection, old: &str, new: &str) -> Result<()> {
    if table_exists(conn, old)? && !table_exists(conn, new)? {
        conn.execute(&format!("ALTER TABLE {old} RENAME TO {new}"), [])?;
    }
    Ok(())
}

fn rename_column_if_exists(conn: &Connection, table: &str, old: &str, new: &str) -> Result<()> {
    if table_exists(conn, table)?
        && table_has_column(conn, table, old)?
        && !table_has_column(conn, table, new)?
    {
        conn.execute(
            &format!("ALTER TABLE {table} RENAME COLUMN {old} TO {new}"),
            [],
        )?;
    }
    Ok(())
}

fn rewrite_history_table_names(conn: &Connection, table: &str, column: &str) -> Result<()> {
    if !table_exists(conn, table)? || !table_has_column(conn, table, column)? {
        return Ok(());
    }
    conn.execute(
        &format!(
            "UPDATE {table}
             SET {column} = CASE {column}
                WHEN 'work_records' THEN 'history_records'
                WHEN 'work_record_links' THEN 'history_record_links'
                WHEN 'work_record_tags' THEN 'history_record_tags'
                ELSE {column}
             END
             WHERE {column} IN ('work_records', 'work_record_links', 'work_record_tags')"
        ),
        [],
    )?;
    Ok(())
}

fn drop_fts_table_if_column_exists(conn: &Connection, table: &str, column: &str) -> Result<()> {
    if table_exists(conn, table)? && table_has_column(conn, table, column)? {
        conn.execute(&format!("DROP TABLE {table}"), [])?;
    }
    Ok(())
}

pub(crate) fn rebuild_capture_sources_provider_check(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "capture_sources")? {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        return Ok(());
    }

    let recreate_views = stable_sql_views_exist(conn)?;
    if recreate_views {
        drop_stable_sql_views(conn)?;
    }
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS capture_sources_new;
        CREATE TABLE capture_sources_new (
            id TEXT PRIMARY KEY NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('provider_import', 'provider_hook', 'direct_cli', 'manual')),

            provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'tabnine', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'rovodev', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'lingma', 'qoder', 'warp', 'codebuddy', 'auggie', 'firebender', 'junie', 'trae', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown')),

            machine_id TEXT NOT NULL,
            process_id INTEGER,
            cwd TEXT,
            raw_source_path TEXT,
            external_session_id TEXT,
            started_at_ms INTEGER NOT NULL,
            ended_at_ms INTEGER,
            fidelity TEXT NOT NULL CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
            visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
            sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
            sync_version INTEGER NOT NULL DEFAULT 0,
            metadata_json TEXT NOT NULL DEFAULT '{}'
        );
        INSERT INTO capture_sources_new
        (id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json)
        SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json
        FROM capture_sources;
        DROP TABLE capture_sources;
        ALTER TABLE capture_sources_new RENAME TO capture_sources;
        "#,
    )?;
    if recreate_views {
        create_stable_sql_views(conn)?;
    }
    Ok(())
}

pub(crate) fn rebuild_catalog_sessions_provider_check(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "catalog_sessions")? {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        return Ok(());
    }

    let recreate_views = stable_sql_views_exist(conn)?;
    if recreate_views {
        drop_stable_sql_views(conn)?;
    }
    ensure_columns(
        conn,
        "catalog_sessions",
        CATALOG_SESSION_IMPORT_STATE_COLUMNS,
    )?;
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS catalog_sessions_new;
        CREATE TABLE catalog_sessions_new (
            source_path TEXT PRIMARY KEY NOT NULL,

            provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'tabnine', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'rovodev', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'lingma', 'qoder', 'warp', 'codebuddy', 'auggie', 'firebender', 'junie', 'trae', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown')),

            source_format TEXT NOT NULL,
            source_root TEXT NOT NULL,
            external_session_id TEXT,
            parent_external_session_id TEXT,
            agent_type TEXT NOT NULL CHECK (agent_type IN ('primary', 'subagent', 'agent_team_member', 'reviewer', 'implementer', 'unknown')),
            role_hint TEXT,
            external_agent_id TEXT,
            cwd TEXT,
            session_started_at_ms INTEGER,
            file_size_bytes INTEGER NOT NULL,
            file_modified_at_ms INTEGER NOT NULL,
            cataloged_at_ms INTEGER NOT NULL,
            is_stale INTEGER NOT NULL DEFAULT 0,
            indexed_at_ms INTEGER,
            indexed_file_size_bytes INTEGER,
            indexed_file_modified_at_ms INTEGER,
            indexed_status TEXT NOT NULL DEFAULT 'pending' CHECK (indexed_status IN ('pending', 'indexed', 'failed')),
            indexed_error TEXT,
            indexed_event_count INTEGER,
            last_imported_at_ms INTEGER,
            last_imported_file_size_bytes INTEGER,
            last_imported_file_modified_at_ms INTEGER,
            last_imported_file_sha256 TEXT,
            last_imported_event_count INTEGER,
            metadata_json TEXT NOT NULL DEFAULT '{}'
        );
        INSERT INTO catalog_sessions_new
        (source_path, provider, source_format, source_root, external_session_id, parent_external_session_id, agent_type, role_hint, external_agent_id, cwd, session_started_at_ms, file_size_bytes, file_modified_at_ms, cataloged_at_ms, is_stale, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status, indexed_error, indexed_event_count, last_imported_at_ms, last_imported_file_size_bytes, last_imported_file_modified_at_ms, last_imported_file_sha256, last_imported_event_count, metadata_json)
        SELECT source_path, provider, source_format, source_root, external_session_id, parent_external_session_id, agent_type, role_hint, external_agent_id, cwd, session_started_at_ms, file_size_bytes, file_modified_at_ms, cataloged_at_ms, is_stale, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status, indexed_error, indexed_event_count, last_imported_at_ms, last_imported_file_size_bytes, last_imported_file_modified_at_ms, last_imported_file_sha256, last_imported_event_count, metadata_json
        FROM catalog_sessions;
        DROP TABLE catalog_sessions;
        ALTER TABLE catalog_sessions_new RENAME TO catalog_sessions;
        "#,
    )?;
    if recreate_views {
        create_stable_sql_views(conn)?;
    }
    Ok(())
}

fn rebuild_source_import_files_provider_check(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "source_import_files")? {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        return Ok(());
    }

    let recreate_views = stable_sql_views_exist(conn)?;
    if recreate_views {
        drop_stable_sql_views(conn)?;
    }
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS source_import_files_new;
        CREATE TABLE source_import_files_new (

            provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'tabnine', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'rovodev', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'lingma', 'qoder', 'warp', 'codebuddy', 'auggie', 'firebender', 'junie', 'trae', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown')),

            source_format TEXT NOT NULL,
            source_root TEXT NOT NULL,
            source_path TEXT NOT NULL,
            file_size_bytes INTEGER NOT NULL,
            file_modified_at_ms INTEGER NOT NULL,
            observed_at_ms INTEGER NOT NULL,
            is_stale INTEGER NOT NULL DEFAULT 0,
            indexed_at_ms INTEGER,
            indexed_file_size_bytes INTEGER,
            indexed_file_modified_at_ms INTEGER,
            indexed_status TEXT NOT NULL DEFAULT 'pending' CHECK (indexed_status IN ('pending', 'indexed', 'failed')),
            indexed_error TEXT,
            metadata_json TEXT NOT NULL DEFAULT '{}',
            PRIMARY KEY (provider, source_root, source_path)
        );
        INSERT INTO source_import_files_new
        (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms, is_stale, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status, indexed_error, metadata_json)
        SELECT provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms, is_stale, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status, indexed_error, metadata_json
        FROM source_import_files;
        DROP TABLE source_import_files;
        ALTER TABLE source_import_files_new RENAME TO source_import_files;
        "#,
    )?;
    if recreate_views {
        create_stable_sql_views(conn)?;
    }
    Ok(())
}

fn backfill_legacy_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        UPDATE history_records
        SET summary = body
        WHERE summary IS NULL;

        UPDATE history_records
        SET created_at_ms = COALESCE(CAST(strftime('%s', created_at) AS INTEGER) * 1000, created_at_ms)
        WHERE created_at_ms = 0 AND created_at IS NOT NULL;

        UPDATE history_records
        SET updated_at_ms = COALESCE(CAST(strftime('%s', updated_at) AS INTEGER) * 1000, updated_at_ms)
        WHERE updated_at_ms = 0 AND updated_at IS NOT NULL;

        UPDATE history_records
        SET started_at_ms = created_at_ms
        WHERE started_at_ms IS NULL AND created_at_ms != 0;

        UPDATE history_records
        SET last_activity_at_ms = CASE
            WHEN updated_at_ms != 0 THEN updated_at_ms
            WHEN created_at_ms != 0 THEN created_at_ms
            ELSE last_activity_at_ms
        END
        WHERE last_activity_at_ms = 0;
        "#,
    )?;
    Ok(())
}
