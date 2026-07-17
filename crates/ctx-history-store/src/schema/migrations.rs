use rusqlite::Connection;

mod v47_provider_session_repair;
#[cfg(test)]
mod v47_provider_session_repair_tests;

use crate::schema::ddl::{
    ensure_columns, table_exists, table_has_column, CAPTURE_SOURCE_IDENTITY_COLUMNS,
    CATALOG_SESSION_IMPORT_STATE_COLUMNS, CREATE_TABLES_SQL, HISTORY_RECORD_COLUMNS,
    IMPORT_INVENTORY_CHECKPOINT_TABLES_SQL, LEGACY_CATALOG_IMPORT_REVISION_COLUMNS,
    LEGACY_SOURCE_IMPORT_REVISION_COLUMNS, SOURCE_IMPORT_FILE_STATE_COLUMNS,
};
use crate::schema::fts::{create_fts_tables_if_supported, drop_fts_table_if_exists};
use crate::schema::import_pending_work::install_import_pending_work_invariants;
use crate::schema::indexes::{
    FRESH_STORE_OPTIMIZED_INDEXES_SQL, IMPORT_PENDING_WORK_SELECTION_INDEX_SQL,
};
use crate::schema::provider_session_identity::{
    backfill_capture_source_identity_columns, prepare_provider_session_migrations,
    restore_invariants_after_capture_source_rebuild, suspend_invariants_for_capture_source_rebuild,
    FRESH_STORE_PROVIDER_SESSION_UNIQUE_INDEX_SQL,
};
use crate::schema::rebuild::{
    rebuild_table_from_current_schema, rebuild_v44_current_schema_tables,
};
use crate::schema::scriptgram::migrate_to_v45;
use crate::schema::views::{
    create_stable_sql_views, drop_stable_sql_views, stable_sql_views_exist,
};
use crate::schema::writer_fence::install_schema_writer_fence;
use crate::search::projections::{
    mark_search_projection_rebuild_required,
    trust_existing_search_projection_if_not_rebuild_pending,
};
use crate::{Result, StoreError};

use self::v47_provider_session_repair::migrate_to_v47;

pub(crate) fn run_migrations(
    conn: &Connection,
    user_version: i64,
    fresh_empty_store: bool,
) -> Result<()> {
    prepare_provider_session_migrations(conn, user_version)?;
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
    if user_version < 43 {
        migrate_to_v43(conn)?;
    }
    if user_version < 44 {
        migrate_to_v44(conn)?;
    }
    if user_version < 45 {
        migrate_to_v45(conn)?;
    }
    if user_version < 46 {
        migrate_to_v46(conn)?;
    }
    if user_version < 47 {
        migrate_to_v47(conn)?;
    }
    if user_version < 48 {
        migrate_import_outcomes_to_v48(conn)?;
    }
    if user_version < 49 {
        migrate_inventory_generations_to_v49(conn)?;
    }
    if user_version < 50 {
        migrate_inventory_completion_to_v50(conn)?;
    }
    if user_version < 51 {
        migrate_provider_publication_to_v51(conn)?;
    }
    if user_version < 52 {
        migrate_fresh_scheduling_to_v52(conn)?;
    }
    if user_version < 53 {
        migrate_publication_completion_to_v53(conn)?;
    }
    if user_version < 54 {
        migrate_publication_retirement_to_v54(conn)?;
    }
    if user_version < 55 {
        migrate_stable_views_to_v55(conn)?;
    }
    if user_version < 56 {
        migrate_lightweight_event_index_to_v56(conn)?;
    }
    if user_version < 57 {
        migrate_pending_work_projection_to_v57_with_mode(conn, fresh_empty_store)?;
    }
    Ok(())
}

fn migrate_to_v1(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(conn, "history_records", HISTORY_RECORD_COLUMNS)?;
        backfill_legacy_tables(conn)?;
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
        mark_search_projection_rebuild_required(conn)?;
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
        mark_search_projection_rebuild_required(conn)?;
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

fn migrate_to_v43(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(conn, "capture_sources", CAPTURE_SOURCE_IDENTITY_COLUMNS)?;
        backfill_capture_source_identity_columns(conn)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 43;")?;
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

fn migrate_to_v44(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        mark_search_projection_rebuild_required(conn)?;
        rebuild_v44_current_schema_tables(conn)?;
        drop_fts_table_if_exists(conn, "event_search")?;
        drop_fts_table_if_exists(conn, "artifact_search")?;
        create_fts_tables_if_supported(conn)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 44;")?;
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

fn migrate_to_v46(conn: &Connection) -> Result<()> {
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
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 46;")?;
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

pub(super) fn migrate_import_outcomes_to_v48(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        ensure_columns(
            conn,
            "catalog_sessions",
            LEGACY_CATALOG_IMPORT_REVISION_COLUMNS,
        )?;
        ensure_columns(
            conn,
            "source_import_files",
            LEGACY_SOURCE_IMPORT_REVISION_COLUMNS,
        )?;
        widen_import_outcome_checks(conn)?;
        install_schema_writer_fence(conn, 48)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 48;")?;
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

fn widen_import_outcome_checks(conn: &Connection) -> Result<()> {
    const LEGACY_CHECK: &str = "CHECK (indexed_status IN ('pending', 'indexed', 'failed'))";
    const CURRENT_CHECK: &str = "CHECK (indexed_status IN ('pending', 'indexed', \
        'completed_with_rejections', 'rejected', 'failed'))";

    conn.execute_batch("PRAGMA writable_schema = ON;")?;
    let update = (|| -> Result<()> {
        for table in ["catalog_sessions", "source_import_files"] {
            let sql: String = conn.query_row(
                "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )?;
            if sql.contains(CURRENT_CHECK) {
                continue;
            }
            if sql.matches(LEGACY_CHECK).count() != 1 {
                return Err(StoreError::ImportInventorySchemaIncompatible(table));
            }
            let updated = sql.replacen(LEGACY_CHECK, CURRENT_CHECK, 1);
            let changed = conn.execute(
                "UPDATE sqlite_schema SET sql = ?1 WHERE type = 'table' AND name = ?2",
                (&updated, table),
            )?;
            if changed != 1 {
                return Err(StoreError::ImportInventorySchemaIncompatible(table));
            }
        }
        Ok(())
    })();
    let reset = conn.execute_batch("PRAGMA writable_schema = OFF;");
    match (update, reset) {
        (Err(error), _) => return Err(error),
        (Ok(()), Err(error)) => return Err(StoreError::Sql(error)),
        (Ok(()), Ok(())) => {}
    }
    let schema_version: i64 = conn.query_row("PRAGMA schema_version", [], |row| row.get(0))?;
    conn.pragma_update(None, "schema_version", schema_version.saturating_add(1))?;
    Ok(())
}

fn migrate_inventory_generations_to_v49(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        // Legacy rows remain visible when no generation row exists. The next
        // real inventory allocates generation 1 for its concrete source root,
        // avoiding a foreground DISTINCT scan over the whole catalog here.
        conn.execute_batch("PRAGMA user_version = 49;")?;
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

fn migrate_inventory_completion_to_v50(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        if !table_has_column(conn, "import_inventory_generations", "completed_generation")? {
            conn.execute_batch(
                "ALTER TABLE import_inventory_generations\n\
                 ADD COLUMN completed_generation INTEGER NOT NULL DEFAULT 0\n\
                 CHECK (completed_generation >= 0 AND completed_generation <= current_generation);",
            )?;
        }
        conn.execute_batch(
            "UPDATE import_inventory_generations\n\
             SET completed_generation = current_generation;\n\
             PRAGMA user_version = 50;",
        )?;
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

pub(super) fn migrate_provider_publication_to_v51(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        conn.execute_batch(CREATE_TABLES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch(
            "INSERT OR IGNORE INTO semantic_replacement_revision (singleton, current_revision) VALUES (1, 0);\
             PRAGMA user_version = 51;",
        )?;
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

pub(super) fn migrate_fresh_scheduling_to_v52(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        let legacy_provider_staging =
            !table_has_column(conn, "provider_file_publications", "staging_initialized")?;
        let legacy_prior_material_scope =
            !table_has_column(conn, "provider_file_publications", "tracks_prior_material")?;
        if legacy_provider_staging || legacy_prior_material_scope {
            let recreate_views = stable_sql_views_exist(conn)?;
            if recreate_views {
                drop_stable_sql_views(conn)?;
            }
            if legacy_prior_material_scope {
                conn.execute_batch(
                    "ALTER TABLE provider_file_publications ADD COLUMN tracks_prior_material INTEGER \
                     NOT NULL DEFAULT 0 CHECK (tracks_prior_material IN (0, 1));",
                )?;
            }
            if legacy_provider_staging {
                conn.execute_batch(
                    "ALTER TABLE provider_file_publications ADD COLUMN staging_initialized INTEGER \
                     NOT NULL DEFAULT 0 CHECK (staging_initialized IN (0, 1));",
                )?;
            }
            rebuild_table_from_current_schema(conn, "provider_file_publications")?;
            if recreate_views {
                create_stable_sql_views(conn)?;
            }
        }
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS provider_file_publication_seen (
                replacement_id TEXT NOT NULL,
                entity_kind TEXT NOT NULL,
                entity_id TEXT NOT NULL,
                PRIMARY KEY (replacement_id, entity_kind, entity_id),
                FOREIGN KEY (replacement_id) REFERENCES provider_file_publications(replacement_id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS provider_file_publication_prior_sources (
                replacement_id TEXT NOT NULL,
                source_id TEXT NOT NULL,
                PRIMARY KEY (replacement_id, source_id),
                FOREIGN KEY (replacement_id) REFERENCES provider_file_publications(replacement_id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS provider_file_publication_batch (
                replacement_id TEXT NOT NULL,
                source_id TEXT NOT NULL,
                entity_id TEXT NOT NULL,
                PRIMARY KEY (replacement_id, source_id, entity_id),
                FOREIGN KEY (replacement_id) REFERENCES provider_file_publications(replacement_id) ON DELETE CASCADE
            );
            "#,
        )?;
        if legacy_prior_material_scope {
            // v50 sidecar state is unavailable after restart. Once mutation
            // began, classify conservatively so recovery reconciles any rows
            // committed before the crash, even without an append checkpoint.
            conn.execute_batch(
                r#"
                UPDATE provider_file_publications AS publication
                SET tracks_prior_material = CASE
                    WHEN publication.mutation_started = 0 THEN 0
                    ELSE 1
                END;
                "#,
            )?;
        }
        if legacy_provider_staging {
            // v51 staging lived in an unlinked Unix sidecar and cannot be trusted
            // after restart. Keep mutation fencing, but rewind every cursor whose
            // corresponding staged rows must be reconstructed in the main database.
            conn.execute_batch(
                r#"
                UPDATE provider_file_publications
                SET staging_initialized = 0,
                    preparation_complete = CASE
                        WHEN publication_kind = 'incremental' THEN 1 ELSE 0
                    END,
                    preparation_cursor = NULL,
                    cleanup_phase = 0,
                    cleanup_source_cursor = NULL,
                    cleanup_entity_cursor = NULL,
                    removed_artifacts = 0,
                    removed_summaries = 0,
                    removed_history_record_links = 0,
                    removed_history_records = 0,
                    removed_history_record_tags = 0,
                    removed_record_edges = 0,
                    removed_audit_log_entries = 0,
                    removed_vcs_workspaces = 0,
                    removed_vcs_changes = 0,
                    removed_events = 0,
                    removed_runs = 0,
                    removed_files_touched = 0,
                    removed_session_edges = 0,
                    tombstoned_sessions = 0;
                "#,
            )?;
        }
        if !table_has_column(conn, "catalog_sessions", "pending_reason")? {
            conn.execute_batch("ALTER TABLE catalog_sessions ADD COLUMN pending_reason TEXT;")?;
        }
        if !table_has_column(conn, "source_import_files", "pending_reason")? {
            conn.execute_batch("ALTER TABLE source_import_files ADD COLUMN pending_reason TEXT;")?;
        }
        conn.execute_batch(
            r#"
            -- Existing inventory rows are classified by bounded maintenance. These
            -- cursors make that work durable without rewriting the corpus here.
            CREATE TABLE IF NOT EXISTS import_pending_reason_repairs (
              inventory_family TEXT PRIMARY KEY NOT NULL
                CHECK (inventory_family IN ('catalog_sessions', 'source_import_files')),
              cursor_provider TEXT,
              cursor_source_root TEXT,
              cursor_source_path TEXT,
              completed INTEGER NOT NULL DEFAULT 0 CHECK (completed IN (0, 1))
            );

            INSERT OR IGNORE INTO import_pending_reason_repairs (inventory_family)
            VALUES ('catalog_sessions'), ('source_import_files');
            "#,
        )?;
        conn.execute_batch("PRAGMA user_version = 52;")?;
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

pub(super) fn migrate_publication_completion_to_v53(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        if !table_has_column(
            conn,
            "provider_file_publications",
            "completion_payload_json",
        )? {
            conn.execute_batch(
                r#"
                ALTER TABLE main.provider_file_publications
                ADD COLUMN completion_payload_json TEXT CHECK (
                    completion_payload_json IS NULL OR
                    length(CAST(completion_payload_json AS BLOB)) BETWEEN 1 AND 262144
                );
                "#,
            )?;
        }
        conn.execute_batch("PRAGMA user_version = 53;")?;
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

pub(super) fn migrate_publication_retirement_to_v54(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        if !table_has_column(
            conn,
            "provider_file_publications",
            "inventory_observation_invalidated",
        )? {
            conn.execute_batch(
                r#"
                ALTER TABLE main.provider_file_publications
                ADD COLUMN inventory_observation_invalidated INTEGER NOT NULL DEFAULT 0
                    CHECK (inventory_observation_invalidated IN (0, 1));
                "#,
            )?;
        }
        if !table_has_column(conn, "provider_file_publications", "retirement_started")? {
            conn.execute_batch(
                r#"
                ALTER TABLE main.provider_file_publications
                ADD COLUMN retirement_started INTEGER NOT NULL DEFAULT 0
                    CHECK (retirement_started IN (0, 1));
                "#,
            )?;
        }
        // v52 originally classified some mutated incremental publications as
        // not tracking prior material. Recovery must reconcile conservatively
        // after any mutation, even when no append checkpoint survived.
        conn.execute_batch(
            r#"
            UPDATE provider_file_publications
            SET tracks_prior_material = 1
            WHERE mutation_started != 0
              AND tracks_prior_material = 0;
            "#,
        )?;
        conn.execute_batch("PRAGMA user_version = 54;")?;
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
fn migrate_stable_views_to_v55(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        drop_stable_sql_views(conn)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 55;")?;
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

pub(super) fn migrate_lightweight_event_index_to_v56(conn: &Connection) -> Result<()> {
    // Fresh stores no longer create idx_events_seq. Keep an existing copy until
    // bounded idle maintenance can remove it without turning Store::open into
    // a corpus-sized foreground write.
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        // A v55 projection is structurally compatible with v56. Absence of a
        // durable rebuild marker is sufficient to trust it; older destructive
        // migrations leave that marker behind and therefore cannot publish.
        trust_existing_search_projection_if_not_rebuild_pending(conn)?;
        conn.execute_batch("PRAGMA user_version = 56;")?;
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

pub(super) fn migrate_pending_work_projection_to_v57_with_mode(
    conn: &Connection,
    fresh_empty_store: bool,
) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        // CREATE_TABLES_SQL adds only the empty projection and aggregate tables
        // on an existing store. The sole projection index is therefore also
        // created empty, before triggers can mirror any new inventory writes.
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_import_inventory_checkpoint_schema_v57(conn)?;
        conn.execute_batch(IMPORT_PENDING_WORK_SELECTION_INDEX_SQL)?;
        validate_pending_work_selection_index_v57(conn)?;
        conn.execute_batch(
            r#"
            INSERT OR IGNORE INTO import_pending_work_state (
              singleton, selection_mode, projection_version, legacy_cleanup_complete,
              legacy_cleanup_phase, legacy_cleanup_inventory_family,
              legacy_cleanup_provider, legacy_cleanup_source_root, legacy_cleanup_tail,
              material_cursor_rowid, material_scan_complete
            ) VALUES (1, 'projection', 2, 1, 'work', '', '', '', '', 0, 0);

            INSERT OR IGNORE INTO import_pending_reason_repairs (inventory_family)
            VALUES ('catalog_sessions'), ('source_import_files');

            UPDATE import_pending_reason_repairs
            SET cursor_provider = NULL,
                cursor_source_root = NULL,
                cursor_source_path = NULL,
                cursor_rowid = 0,
                completed = 0
            WHERE inventory_family IN ('catalog_sessions', 'source_import_files');

            PRAGMA user_version = 57;
            "#,
        )?;
        if fresh_empty_store {
            conn.execute_batch(FRESH_STORE_OPTIMIZED_INDEXES_SQL)?;
            conn.execute_batch(FRESH_STORE_PROVIDER_SESSION_UNIQUE_INDEX_SQL)?;
            conn.execute(
                "UPDATE import_pending_work_state \
                 SET selection_mode = 'direct' WHERE singleton = 1",
                [],
            )?;
        }
        install_import_pending_work_invariants(conn)?;
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

pub(super) fn ensure_import_inventory_checkpoint_schema_v57(conn: &Connection) -> Result<()> {
    let table_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name IN (\
           'import_inventory_runs', 'import_inventory_checkpoints', \
           'import_inventory_path_effects'\
         )",
        [],
        |row| row.get(0),
    )?;
    match table_count {
        0 => {
            conn.execute_batch(IMPORT_INVENTORY_CHECKPOINT_TABLES_SQL)?;
        }
        3 => {}
        _ => {
            return Err(StoreError::ImportInventorySchemaIncompatible(
                "durable inventory checkpoint tables are incomplete",
            ));
        }
    }
    let mirrored_queue_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name IN (\
           'import_inventory_directory_work', \
           'idx_import_inventory_directory_queue_selection'\
         ))",
        [],
        |row| row.get(0),
    )?;
    if mirrored_queue_exists {
        return Err(StoreError::ImportInventorySchemaIncompatible(
            "durable inventory directory queue must be owned only by capture scratch",
        ));
    }
    let checkpoint_columns: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('import_inventory_checkpoints') \
         WHERE name IN ('owner_state', 'scratch_owner_epoch', 'scratch_owner_token', \
                        'directory_queue_empty', 'active_directory_identity', \
                        'active_directory_fingerprint', \
                        'active_directory_observed_entries', 'discovered_path_count', \
                        'attempt_count', 'scratch_database_identity', 'selection_keyset', \
                        'selection_eof', 'selection_complete')",
        [],
        |row| row.get(0),
    )?;
    let effect_columns: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('import_inventory_path_effects') \
         WHERE name = 'capture_journal_identity'",
        [],
        |row| row.get(0),
    )?;
    let source_identity_key = import_inventory_checkpoint_unique_key_exists(
        conn,
        &[
            "inventory_family",
            "provider",
            "source_identity",
            "inventory_generation",
        ],
    )?;
    let source_root_key = import_inventory_checkpoint_unique_key_exists(
        conn,
        &[
            "inventory_family",
            "provider",
            "source_root",
            "inventory_generation",
        ],
    )?;
    if checkpoint_columns != 13 || effect_columns != 1 || !source_identity_key || !source_root_key {
        return Err(StoreError::ImportInventorySchemaIncompatible(
            "durable inventory checkpoint schema shape is incompatible",
        ));
    }
    Ok(())
}

fn import_inventory_checkpoint_unique_key_exists(
    conn: &Connection,
    expected_columns: &[&str],
) -> Result<bool> {
    let index_names = conn
        .prepare("PRAGMA index_list(import_inventory_checkpoints)")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, bool>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (index_name, unique) in index_names {
        if !unique {
            continue;
        }
        let columns = conn
            .prepare(
                "SELECT name, desc, coll FROM pragma_index_xinfo(?1) \
                 WHERE key = 1 AND cid >= 0 ORDER BY seqno",
            )?
            .query_map([index_name], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if columns.len() == expected_columns.len()
            && columns.iter().zip(expected_columns).all(
                |((actual, descending, collation), expected)| {
                    actual == expected && !descending && collation.eq_ignore_ascii_case("binary")
                },
            )
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn validate_pending_work_selection_index_v57(conn: &Connection) -> Result<()> {
    let index_shape = conn
        .prepare("PRAGMA index_list(import_pending_work)")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, bool>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .find(|(name, _, _)| name == "idx_import_pending_work_selection");
    let Some((_, unique, partial)) = index_shape else {
        return Err(StoreError::ImportInventorySchemaIncompatible(
            "pending-work selection index is missing",
        ));
    };
    if unique || partial {
        return Err(StoreError::ImportInventorySchemaIncompatible(
            "pending-work selection index has incompatible flags",
        ));
    }
    let table_columns = conn
        .prepare("PRAGMA table_info(import_pending_work)")?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let key_columns = conn
        .prepare("PRAGMA index_xinfo(idx_import_pending_work_selection)")?
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, bool>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(_, _, _, _, _, key)| *key)
        .collect::<Vec<_>>();
    let expected_columns = [
        "inventory_family",
        "provider",
        "source_root",
        "work_class",
        "indexed_at_ms",
        "source_path",
    ];
    if key_columns.len() != expected_columns.len() {
        return Err(StoreError::ImportInventorySchemaIncompatible(
            "pending-work selection index has incompatible key count",
        ));
    }
    for (position, (seqno, cid, name, descending, collation, _)) in key_columns.iter().enumerate() {
        let expected_name = expected_columns[position];
        let expected_cid = table_columns
            .iter()
            .find_map(|(cid, name)| (name == expected_name).then_some(*cid))
            .ok_or(StoreError::ImportInventorySchemaIncompatible(
                "pending-work selection column is missing",
            ))?;
        if *seqno != position as i64
            || *cid != expected_cid
            || name.as_deref() != Some(expected_name)
            || *descending
            || collation.as_deref() != Some("BINARY")
        {
            return Err(StoreError::ImportInventorySchemaIncompatible(
                "pending-work selection index has incompatible key shape",
            ));
        }
    }
    Ok(())
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

    let recreate_invariants = suspend_invariants_for_capture_source_rebuild(conn)?;
    let recreate_views = stable_sql_views_exist(conn)?;
    if recreate_views {
        drop_stable_sql_views(conn)?;
    }
    ensure_columns(conn, "capture_sources", CAPTURE_SOURCE_IDENTITY_COLUMNS)?;
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS capture_sources_new;
        CREATE TABLE capture_sources_new (
            id TEXT PRIMARY KEY NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('provider_import', 'provider_hook', 'direct_cli', 'manual')),

            provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'tabnine', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'rovodev', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'lingma', 'qoder', 'warp', 'codebuddy', 'auggie', 'firebender', 'junie', 'trae', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown', 'mimocode')),

            machine_id TEXT NOT NULL,
            process_id INTEGER,
            cwd TEXT,
            raw_source_path TEXT,
            source_format TEXT,
            source_root TEXT,
            source_identity TEXT,
            external_session_id TEXT,
            started_at_ms INTEGER NOT NULL,
            ended_at_ms INTEGER,
            fidelity TEXT NOT NULL CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
            visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full')),
            sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed')),
            sync_version INTEGER NOT NULL DEFAULT 0,
            metadata_json TEXT NOT NULL DEFAULT '{}'
        );
        INSERT INTO capture_sources_new
        (id, kind, provider, machine_id, process_id, cwd, raw_source_path, source_format, source_root, source_identity, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json)
        SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, source_format, source_root, source_identity, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json
        FROM capture_sources;
        DROP TABLE capture_sources;
        ALTER TABLE capture_sources_new RENAME TO capture_sources;
        "#,
    )?;
    if recreate_views {
        create_stable_sql_views(conn)?;
    }
    restore_invariants_after_capture_source_rebuild(conn, recreate_invariants)?;
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

            provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'tabnine', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'rovodev', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'lingma', 'qoder', 'warp', 'codebuddy', 'auggie', 'firebender', 'junie', 'trae', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown', 'mimocode')),

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
            import_revision INTEGER NOT NULL DEFAULT 1 CHECK (import_revision > 0),
            cataloged_at_ms INTEGER NOT NULL,
            is_stale INTEGER NOT NULL DEFAULT 0,
            indexed_at_ms INTEGER,
            indexed_file_size_bytes INTEGER,
            indexed_file_modified_at_ms INTEGER,
            indexed_status TEXT NOT NULL DEFAULT 'pending' CHECK (indexed_status IN ('pending', 'indexed', 'completed_with_rejections', 'rejected', 'failed')),
            indexed_error TEXT,
            indexed_event_count INTEGER,
            indexed_import_revision INTEGER CHECK (indexed_import_revision > 0),
            last_imported_at_ms INTEGER,
            last_imported_file_size_bytes INTEGER,
            last_imported_file_modified_at_ms INTEGER,
            last_imported_file_sha256 TEXT,
            last_imported_event_count INTEGER,
            metadata_json TEXT NOT NULL DEFAULT '{}'
        );
        INSERT INTO catalog_sessions_new
        (source_path, provider, source_format, source_root, external_session_id, parent_external_session_id, agent_type, role_hint, external_agent_id, cwd, session_started_at_ms, file_size_bytes, file_modified_at_ms, import_revision, cataloged_at_ms, is_stale, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status, indexed_error, indexed_event_count, indexed_import_revision, last_imported_at_ms, last_imported_file_size_bytes, last_imported_file_modified_at_ms, last_imported_file_sha256, last_imported_event_count, metadata_json)
        SELECT source_path, provider, source_format, source_root, external_session_id, parent_external_session_id, agent_type, role_hint, external_agent_id, cwd, session_started_at_ms, file_size_bytes, file_modified_at_ms, import_revision, cataloged_at_ms, is_stale, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status, indexed_error, indexed_event_count, indexed_import_revision, last_imported_at_ms, last_imported_file_size_bytes, last_imported_file_modified_at_ms, last_imported_file_sha256, last_imported_event_count, metadata_json
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
    ensure_columns(
        conn,
        "source_import_files",
        SOURCE_IMPORT_FILE_STATE_COLUMNS,
    )?;
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS source_import_files_new;
        CREATE TABLE source_import_files_new (

            provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'tabnine', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'rovodev', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'lingma', 'qoder', 'warp', 'codebuddy', 'auggie', 'firebender', 'junie', 'trae', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown', 'mimocode')),

            source_format TEXT NOT NULL,
            source_root TEXT NOT NULL,
            source_path TEXT NOT NULL,
            file_size_bytes INTEGER NOT NULL,
            file_modified_at_ms INTEGER NOT NULL,
            import_revision INTEGER NOT NULL DEFAULT 1 CHECK (import_revision > 0),
            observed_at_ms INTEGER NOT NULL,
            is_stale INTEGER NOT NULL DEFAULT 0,
            indexed_at_ms INTEGER,
            indexed_file_size_bytes INTEGER,
            indexed_file_modified_at_ms INTEGER,
            indexed_status TEXT NOT NULL DEFAULT 'pending' CHECK (indexed_status IN ('pending', 'indexed', 'completed_with_rejections', 'rejected', 'failed')),
            indexed_error TEXT,
            indexed_import_revision INTEGER CHECK (indexed_import_revision > 0),
            metadata_json TEXT NOT NULL DEFAULT '{}',
            PRIMARY KEY (provider, source_root, source_path)
        );
        INSERT INTO source_import_files_new
        (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, import_revision, observed_at_ms, is_stale, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status, indexed_error, indexed_import_revision, metadata_json)
        SELECT provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, import_revision, observed_at_ms, is_stale, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status, indexed_error, indexed_import_revision, metadata_json
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
