//! Released migrations through v16 and their legacy table-shape helpers.

use rusqlite::Connection;

use crate::schema::ddl::{
    ensure_columns, table_exists, table_has_column, CATALOG_SESSION_IMPORT_STATE_COLUMNS,
    CREATE_TABLES_SQL, HISTORY_RECORD_COLUMNS,
};
use crate::schema::indexes::INDEXES_SQL;
use crate::schema::provider_checks::{
    rebuild_capture_sources_provider_check, rebuild_catalog_sessions_provider_check,
    rebuild_source_import_files_provider_check,
};
use crate::schema::views::{
    create_stable_sql_views, drop_stable_sql_views, stable_sql_views_exist,
};
use crate::search::projections::rebuild_search_projection;
use crate::{Result, StoreError};

pub(super) fn migrate_to_v1(conn: &Connection) -> Result<()> {
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

pub(super) fn migrate_to_v2(conn: &Connection) -> Result<()> {
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

pub(super) fn migrate_to_v3(conn: &Connection) -> Result<()> {
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

pub(super) fn migrate_to_v4(conn: &Connection) -> Result<()> {
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

pub(super) fn migrate_to_v5(conn: &Connection) -> Result<()> {
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

pub(super) fn migrate_to_v6(conn: &Connection) -> Result<()> {
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

pub(super) fn migrate_to_v7(conn: &Connection) -> Result<()> {
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

pub(super) fn migrate_to_v8(conn: &Connection) -> Result<()> {
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

pub(super) fn migrate_to_v9(conn: &Connection) -> Result<()> {
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

pub(super) fn migrate_to_v10(conn: &Connection) -> Result<()> {
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

pub(super) fn migrate_to_v11(conn: &Connection) -> Result<()> {
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

pub(super) fn migrate_to_v12(conn: &Connection) -> Result<()> {
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

pub(super) fn migrate_to_v13(conn: &Connection) -> Result<()> {
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

pub(super) fn migrate_to_v14(conn: &Connection) -> Result<()> {
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

pub(super) fn migrate_to_v15(conn: &Connection) -> Result<()> {
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

pub(super) fn migrate_to_v16(conn: &Connection) -> Result<()> {
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
