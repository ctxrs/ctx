//! Consolidated current-schema migrations from v42 through v46.

use rusqlite::Connection;

use crate::schema::ddl::{ensure_columns, CAPTURE_SOURCE_IDENTITY_COLUMNS, CREATE_TABLES_SQL};
use crate::schema::fts::{create_fts_tables_if_supported, drop_fts_table_if_exists};
use crate::schema::indexes::INDEXES_SQL;
use crate::schema::provider_checks::{
    rebuild_capture_sources_provider_check, rebuild_catalog_sessions_provider_check,
    rebuild_source_import_files_provider_check,
};
use crate::schema::provider_session_identity::backfill_capture_source_identity_columns;
use crate::schema::rebuild::rebuild_v44_current_schema_tables;
use crate::schema::views::{
    create_stable_sql_views, drop_stable_sql_views, stable_sql_views_exist,
};
use crate::search::projections::rebuild_search_projection;
use crate::{Result, StoreError};

pub(super) fn migrate_to_v42(conn: &Connection) -> Result<()> {
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

pub(super) fn migrate_to_v43(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(conn, "capture_sources", CAPTURE_SOURCE_IDENTITY_COLUMNS)?;
        backfill_capture_source_identity_columns(conn)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        conn.execute_batch(INDEXES_SQL)?;
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

pub(super) fn migrate_to_v44(conn: &Connection, rebuild_search_projection_now: bool) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_v44_current_schema_tables(conn)?;
        drop_fts_table_if_exists(conn, "event_search")?;
        drop_fts_table_if_exists(conn, "artifact_search")?;
        create_fts_tables_if_supported(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        if rebuild_search_projection_now {
            rebuild_search_projection(conn)?;
        }
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

pub(super) fn migrate_to_v46(conn: &Connection) -> Result<()> {
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
