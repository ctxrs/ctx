use rusqlite::Connection;

#[cfg(test)]
use crate::schema::ddl::CAPTURE_SOURCE_IDENTITY_COLUMNS;
use crate::schema::ddl::{
    ensure_columns, table_exists, table_has_column, CATALOG_SESSION_IMPORT_STATE_COLUMNS,
    CREATE_TABLES_SQL, HISTORY_RECORD_COLUMNS,
};
use crate::schema::views::{
    create_stable_sql_views, drop_stable_sql_views, stable_sql_views_exist,
};
use crate::{Result, StoreError};

const LEGACY_UPDATE_BATCH_ROWS: i64 = 512;

pub(crate) fn run_next_legacy_migration(
    conn: &Connection,
    user_version: i64,
    mut revalidate: impl FnMut() -> Result<()>,
) -> Result<()> {
    if user_version < 1 {
        migrate_to_v1(conn, &mut revalidate)
    } else if user_version < 2 {
        migrate_to_v2(conn, &mut revalidate)
    } else if user_version < 3 {
        migrate_to_v3(conn, &mut revalidate)
    } else if user_version < 4 {
        migrate_to_v4(conn, &mut revalidate)
    } else if user_version < 5 {
        migrate_to_v5(conn, &mut revalidate)
    } else if user_version < 6 {
        migrate_to_v6(conn, &mut revalidate)
    } else if user_version < 7 {
        migrate_to_v7(conn, &mut revalidate)
    } else if user_version < 8 {
        migrate_to_v8(conn, &mut revalidate)
    } else if user_version < 9 {
        migrate_to_v9(conn, &mut revalidate)
    } else if user_version < 10 {
        migrate_to_v10(conn, &mut revalidate)
    } else if user_version < 11 {
        migrate_to_v11(conn, &mut revalidate)
    } else if user_version < 12 {
        migrate_to_v12(conn, &mut revalidate)
    } else if user_version < 13 {
        migrate_to_v13(conn, &mut revalidate)
    } else if user_version < 14 {
        migrate_to_v14(conn, &mut revalidate)
    } else if user_version < 15 {
        migrate_to_v15(conn, &mut revalidate)
    } else if user_version < 16 {
        migrate_to_v16(conn, &mut revalidate)
    } else {
        Err(StoreError::UnsupportedSchemaVersion(user_version))
    }
}

fn migrate_to_v1(conn: &Connection, revalidate: &mut impl FnMut() -> Result<()>) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(conn, "history_records", HISTORY_RECORD_COLUMNS)?;
        if backfill_legacy_tables_bounded(conn)? == 0 {
            conn.execute_batch("PRAGMA user_version = 1;")?;
        }
        revalidate()?;
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

fn migrate_to_v2(conn: &Connection, revalidate: &mut impl FnMut() -> Result<()>) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(conn, "history_records", HISTORY_RECORD_COLUMNS)?;
        conn.execute_batch("PRAGMA user_version = 2;")?;
        revalidate()?;
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

fn migrate_to_v3(conn: &Connection, revalidate: &mut impl FnMut() -> Result<()>) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(conn, "history_records", HISTORY_RECORD_COLUMNS)?;
        conn.execute_batch("PRAGMA user_version = 3;")?;
        revalidate()?;
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

fn migrate_to_v4(conn: &Connection, revalidate: &mut impl FnMut() -> Result<()>) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch("PRAGMA user_version = 4;")?;
        revalidate()?;
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

fn migrate_to_v5(conn: &Connection, revalidate: &mut impl FnMut() -> Result<()>) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(
            conn,
            "catalog_sessions",
            CATALOG_SESSION_IMPORT_STATE_COLUMNS,
        )?;
        conn.execute_batch("PRAGMA user_version = 5;")?;
        revalidate()?;
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

fn migrate_to_v6(conn: &Connection, revalidate: &mut impl FnMut() -> Result<()>) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(
            conn,
            "catalog_sessions",
            CATALOG_SESSION_IMPORT_STATE_COLUMNS,
        )?;
        conn.execute_batch("PRAGMA user_version = 6;")?;
        revalidate()?;
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

fn migrate_to_v7(conn: &Connection, revalidate: &mut impl FnMut() -> Result<()>) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch("PRAGMA user_version = 7;")?;
        revalidate()?;
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

fn migrate_to_v8(conn: &Connection, revalidate: &mut impl FnMut() -> Result<()>) -> Result<()> {
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
        let rewritten = rewrite_history_table_names_bounded(conn, "sync_outbox", "local_table")?
            .saturating_add(rewrite_history_table_names_bounded(
                conn,
                "audit_log",
                "target_table",
            )?);
        drop_fts_table_if_column_exists(conn, "event_search", "work_record_id")?;
        drop_fts_table_if_column_exists(conn, "artifact_search", "work_record_id")?;
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if rewritten == 0 {
            conn.execute_batch("PRAGMA user_version = 8;")?;
        }
        revalidate()?;
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

fn migrate_to_v9(conn: &Connection, revalidate: &mut impl FnMut() -> Result<()>) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 9;")?;
        revalidate()?;
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

fn migrate_to_v10(conn: &Connection, revalidate: &mut impl FnMut() -> Result<()>) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch("PRAGMA user_version = 10;")?;
        revalidate()?;
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

fn migrate_to_v11(conn: &Connection, revalidate: &mut impl FnMut() -> Result<()>) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch("PRAGMA user_version = 11;")?;
        revalidate()?;
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

fn migrate_to_v12(conn: &Connection, revalidate: &mut impl FnMut() -> Result<()>) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        if invalidate_provider_import_indexes_bounded(conn)? == 0 {
            conn.execute_batch("PRAGMA user_version = 12;")?;
        }
        revalidate()?;
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

fn migrate_to_v13(conn: &Connection, revalidate: &mut impl FnMut() -> Result<()>) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 13;")?;
        revalidate()?;
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

fn migrate_to_v14(conn: &Connection, revalidate: &mut impl FnMut() -> Result<()>) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(
            conn,
            "catalog_sessions",
            CATALOG_SESSION_IMPORT_STATE_COLUMNS,
        )?;
        let backfilled = backfill_catalog_session_import_checkpoints_bounded(conn)?;
        create_stable_sql_views(conn)?;
        if backfilled == 0 {
            conn.execute_batch("PRAGMA user_version = 14;")?;
        }
        revalidate()?;
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

fn migrate_to_v15(conn: &Connection, revalidate: &mut impl FnMut() -> Result<()>) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 15;")?;
        revalidate()?;
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

fn migrate_to_v16(conn: &Connection, revalidate: &mut impl FnMut() -> Result<()>) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 16;")?;
        revalidate()?;
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

fn invalidate_provider_import_indexes_bounded(conn: &Connection) -> Result<usize> {
    if table_exists(conn, "catalog_sessions")? {
        let changed = conn.execute(
            r#"
            UPDATE catalog_sessions
            SET indexed_at_ms = NULL,
                indexed_file_size_bytes = NULL,
                indexed_file_modified_at_ms = NULL,
                indexed_status = 'pending',
                indexed_error = NULL,
                indexed_event_count = NULL
            WHERE rowid IN (
                SELECT rowid FROM catalog_sessions
                WHERE indexed_status = 'indexed'
                ORDER BY rowid
                LIMIT ?1
            )
            "#,
            [LEGACY_UPDATE_BATCH_ROWS],
        )?;
        if changed > 0 {
            return Ok(changed);
        }
    }
    if table_exists(conn, "source_import_files")? {
        return conn
            .execute(
                r#"
            UPDATE source_import_files
            SET indexed_at_ms = NULL,
                indexed_file_size_bytes = NULL,
                indexed_file_modified_at_ms = NULL,
                indexed_status = 'pending',
                indexed_error = NULL
            WHERE rowid IN (
                SELECT rowid FROM source_import_files
                WHERE indexed_status = 'indexed'
                ORDER BY rowid
                LIMIT ?1
            )
            "#,
                [LEGACY_UPDATE_BATCH_ROWS],
            )
            .map_err(Into::into);
    }
    Ok(0)
}

fn backfill_catalog_session_import_checkpoints_bounded(conn: &Connection) -> Result<usize> {
    if !table_exists(conn, "catalog_sessions")? {
        return Ok(0);
    }
    Ok(conn.execute(
        r#"
        UPDATE catalog_sessions
        SET last_imported_at_ms = indexed_at_ms,
            last_imported_file_size_bytes = indexed_file_size_bytes,
            last_imported_file_modified_at_ms = indexed_file_modified_at_ms,
            last_imported_event_count = indexed_event_count
        WHERE rowid IN (
            SELECT rowid FROM catalog_sessions
            WHERE last_imported_file_size_bytes IS NULL
              AND indexed_file_size_bytes IS NOT NULL
            ORDER BY rowid
            LIMIT ?1
        )
        "#,
        [LEGACY_UPDATE_BATCH_ROWS],
    )?)
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

fn rewrite_history_table_names_bounded(
    conn: &Connection,
    table: &str,
    column: &str,
) -> Result<usize> {
    if !table_exists(conn, table)? || !table_has_column(conn, table, column)? {
        return Ok(0);
    }
    Ok(conn.execute(
        &format!(
            "UPDATE {table}
             SET {column} = CASE {column}
                WHEN 'work_records' THEN 'history_records'
                WHEN 'work_record_links' THEN 'history_record_links'
                WHEN 'work_record_tags' THEN 'history_record_tags'
                ELSE {column}
             END
             WHERE rowid IN (
                 SELECT rowid FROM {table}
                 WHERE {column} IN ('work_records', 'work_record_links', 'work_record_tags')
                 ORDER BY rowid
                 LIMIT ?1
             )"
        ),
        [LEGACY_UPDATE_BATCH_ROWS],
    )?)
}

fn drop_fts_table_if_column_exists(conn: &Connection, table: &str, column: &str) -> Result<()> {
    if table_exists(conn, table)? && table_has_column(conn, table, column)? {
        conn.execute(&format!("DROP TABLE {table}"), [])?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn rebuild_capture_sources_provider_check(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "capture_sources")? {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        return Ok(());
    }

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
    Ok(())
}

#[cfg(test)]
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

fn backfill_legacy_tables_bounded(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        r#"
        UPDATE history_records
        SET summary = COALESCE(summary, body),
            created_at_ms = CASE
                WHEN created_at_ms = 0 AND strftime('%s', created_at) IS NOT NULL
                THEN COALESCE(CAST(strftime('%s', created_at) AS INTEGER) * 1000, created_at_ms)
                ELSE created_at_ms
            END,
            updated_at_ms = CASE
                WHEN updated_at_ms = 0 AND strftime('%s', updated_at) IS NOT NULL
                THEN COALESCE(CAST(strftime('%s', updated_at) AS INTEGER) * 1000, updated_at_ms)
                ELSE updated_at_ms
            END,
            started_at_ms = CASE
                WHEN started_at_ms IS NULL AND created_at_ms != 0 THEN created_at_ms
                WHEN started_at_ms IS NULL AND strftime('%s', created_at) IS NOT NULL
                THEN CAST(strftime('%s', created_at) AS INTEGER) * 1000
                ELSE started_at_ms
            END,
            last_activity_at_ms = CASE
                WHEN last_activity_at_ms = 0 AND updated_at_ms != 0 THEN updated_at_ms
                WHEN last_activity_at_ms = 0 AND strftime('%s', updated_at) IS NOT NULL
                THEN CAST(strftime('%s', updated_at) AS INTEGER) * 1000
                WHEN last_activity_at_ms = 0 AND created_at_ms != 0 THEN created_at_ms
                WHEN last_activity_at_ms = 0 AND strftime('%s', created_at) IS NOT NULL
                THEN CAST(strftime('%s', created_at) AS INTEGER) * 1000
                ELSE last_activity_at_ms
            END
        WHERE rowid IN (
            SELECT rowid FROM history_records
            WHERE summary IS NULL
               OR (created_at_ms = 0 AND strftime('%s', created_at) IS NOT NULL)
               OR (updated_at_ms = 0 AND strftime('%s', updated_at) IS NOT NULL)
               OR (started_at_ms IS NULL AND (created_at_ms != 0 OR strftime('%s', created_at) IS NOT NULL))
               OR (last_activity_at_ms = 0 AND (
                    updated_at_ms != 0
                    OR created_at_ms != 0
                    OR strftime('%s', updated_at) IS NOT NULL
                    OR strftime('%s', created_at) IS NOT NULL
               ))
            ORDER BY rowid
            LIMIT ?1
        )
        "#,
        [LEGACY_UPDATE_BATCH_ROWS],
    )?)
}
