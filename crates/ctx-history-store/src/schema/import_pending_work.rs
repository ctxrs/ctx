use rusqlite::Connection;

use crate::Result;

const IMPORT_PENDING_WORK_COUNT_TRIGGER_INVARIANTS_SQL: &str = r#"
CREATE TRIGGER IF NOT EXISTS trg_catalog_sessions_pending_count_insert
AFTER INSERT ON catalog_sessions
WHEN NEW.is_stale = 0 AND NEW.pending_reason IN (
    'fresh_new', 'fresh_changed', 'fresh_append',
    'recovery_retry', 'recovery_replacement', 'parser_revision',
    'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'
)
BEGIN
    INSERT INTO import_pending_work_counts (
        inventory_family, provider, source_root, work_class, pending_count
    ) VALUES (
        'catalog_sessions', NEW.provider, NEW.source_root,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        1
    )
    ON CONFLICT (inventory_family, provider, source_root, work_class)
    DO UPDATE SET pending_count = pending_count + 1;
END;

CREATE TRIGGER IF NOT EXISTS trg_catalog_sessions_pending_count_update_old
BEFORE UPDATE OF provider, source_root, is_stale, pending_reason ON catalog_sessions
WHEN OLD.is_stale = 0 AND OLD.pending_reason IN (
    'fresh_new', 'fresh_changed', 'fresh_append',
    'recovery_retry', 'recovery_replacement', 'parser_revision',
    'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'
)
BEGIN
    DELETE FROM import_pending_work_counts
    WHERE inventory_family = 'catalog_sessions'
      AND provider = OLD.provider
      AND source_root = OLD.source_root
      AND work_class = CASE
          WHEN OLD.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
          THEN 'fresh'
          ELSE 'recovery'
      END
      AND pending_count = 1;

    UPDATE import_pending_work_counts
    SET pending_count = pending_count - 1
    WHERE inventory_family = 'catalog_sessions'
      AND provider = OLD.provider
      AND source_root = OLD.source_root
      AND work_class = CASE
          WHEN OLD.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
          THEN 'fresh'
          ELSE 'recovery'
      END;
END;

CREATE TRIGGER IF NOT EXISTS trg_catalog_sessions_pending_count_update_new
AFTER UPDATE OF provider, source_root, is_stale, pending_reason ON catalog_sessions
WHEN NEW.is_stale = 0 AND NEW.pending_reason IN (
    'fresh_new', 'fresh_changed', 'fresh_append',
    'recovery_retry', 'recovery_replacement', 'parser_revision',
    'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'
)
BEGIN
    INSERT INTO import_pending_work_counts (
        inventory_family, provider, source_root, work_class, pending_count
    ) VALUES (
        'catalog_sessions', NEW.provider, NEW.source_root,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        1
    )
    ON CONFLICT (inventory_family, provider, source_root, work_class)
    DO UPDATE SET pending_count = pending_count + 1;
END;

CREATE TRIGGER IF NOT EXISTS trg_catalog_sessions_pending_count_delete
AFTER DELETE ON catalog_sessions
WHEN OLD.is_stale = 0 AND OLD.pending_reason IN (
    'fresh_new', 'fresh_changed', 'fresh_append',
    'recovery_retry', 'recovery_replacement', 'parser_revision',
    'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'
)
BEGIN
    DELETE FROM import_pending_work_counts
    WHERE inventory_family = 'catalog_sessions'
      AND provider = OLD.provider
      AND source_root = OLD.source_root
      AND work_class = CASE
          WHEN OLD.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
          THEN 'fresh'
          ELSE 'recovery'
      END
      AND pending_count = 1;

    UPDATE import_pending_work_counts
    SET pending_count = pending_count - 1
    WHERE inventory_family = 'catalog_sessions'
      AND provider = OLD.provider
      AND source_root = OLD.source_root
      AND work_class = CASE
          WHEN OLD.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
          THEN 'fresh'
          ELSE 'recovery'
      END;
END;

CREATE TRIGGER IF NOT EXISTS trg_source_import_files_pending_count_insert
AFTER INSERT ON source_import_files
WHEN NEW.is_stale = 0 AND NEW.pending_reason IN (
    'fresh_new', 'fresh_changed', 'fresh_append',
    'recovery_retry', 'recovery_replacement', 'parser_revision',
    'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'
)
BEGIN
    INSERT INTO import_pending_work_counts (
        inventory_family, provider, source_root, work_class, pending_count
    ) VALUES (
        'source_import_files', NEW.provider, NEW.source_root,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        1
    )
    ON CONFLICT (inventory_family, provider, source_root, work_class)
    DO UPDATE SET pending_count = pending_count + 1;
END;

CREATE TRIGGER IF NOT EXISTS trg_source_import_files_pending_count_update_old
BEFORE UPDATE OF provider, source_root, is_stale, pending_reason ON source_import_files
WHEN OLD.is_stale = 0 AND OLD.pending_reason IN (
    'fresh_new', 'fresh_changed', 'fresh_append',
    'recovery_retry', 'recovery_replacement', 'parser_revision',
    'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'
)
BEGIN
    DELETE FROM import_pending_work_counts
    WHERE inventory_family = 'source_import_files'
      AND provider = OLD.provider
      AND source_root = OLD.source_root
      AND work_class = CASE
          WHEN OLD.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
          THEN 'fresh'
          ELSE 'recovery'
      END
      AND pending_count = 1;

    UPDATE import_pending_work_counts
    SET pending_count = pending_count - 1
    WHERE inventory_family = 'source_import_files'
      AND provider = OLD.provider
      AND source_root = OLD.source_root
      AND work_class = CASE
          WHEN OLD.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
          THEN 'fresh'
          ELSE 'recovery'
      END;
END;

CREATE TRIGGER IF NOT EXISTS trg_source_import_files_pending_count_update_new
AFTER UPDATE OF provider, source_root, is_stale, pending_reason ON source_import_files
WHEN NEW.is_stale = 0 AND NEW.pending_reason IN (
    'fresh_new', 'fresh_changed', 'fresh_append',
    'recovery_retry', 'recovery_replacement', 'parser_revision',
    'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'
)
BEGIN
    INSERT INTO import_pending_work_counts (
        inventory_family, provider, source_root, work_class, pending_count
    ) VALUES (
        'source_import_files', NEW.provider, NEW.source_root,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        1
    )
    ON CONFLICT (inventory_family, provider, source_root, work_class)
    DO UPDATE SET pending_count = pending_count + 1;
END;

CREATE TRIGGER IF NOT EXISTS trg_source_import_files_pending_count_delete
AFTER DELETE ON source_import_files
WHEN OLD.is_stale = 0 AND OLD.pending_reason IN (
    'fresh_new', 'fresh_changed', 'fresh_append',
    'recovery_retry', 'recovery_replacement', 'parser_revision',
    'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'
)
BEGIN
    DELETE FROM import_pending_work_counts
    WHERE inventory_family = 'source_import_files'
      AND provider = OLD.provider
      AND source_root = OLD.source_root
      AND work_class = CASE
          WHEN OLD.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
          THEN 'fresh'
          ELSE 'recovery'
      END
      AND pending_count = 1;

    UPDATE import_pending_work_counts
    SET pending_count = pending_count - 1
    WHERE inventory_family = 'source_import_files'
      AND provider = OLD.provider
      AND source_root = OLD.source_root
      AND work_class = CASE
          WHEN OLD.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
          THEN 'fresh'
          ELSE 'recovery'
      END;
END;
"#;

const IMPORT_PENDING_WORK_PROJECTION_TRIGGER_INVARIANTS_SQL: &str = r#"
CREATE TRIGGER IF NOT EXISTS trg_catalog_sessions_pending_work_insert
AFTER INSERT ON catalog_sessions
WHEN NEW.is_stale = 0 AND NEW.pending_reason IN (
    'fresh_new', 'fresh_changed', 'fresh_append',
    'recovery_retry', 'recovery_replacement', 'parser_revision',
    'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'
)
BEGIN
    INSERT INTO import_pending_work (
        inventory_family, provider, source_root, source_path,
        work_class, indexed_at_ms
    ) VALUES (
        'catalog_sessions', NEW.provider, NEW.source_root, NEW.source_path,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        NEW.indexed_at_ms
    );
END;

CREATE TRIGGER IF NOT EXISTS trg_catalog_sessions_pending_work_update_old
BEFORE UPDATE OF provider, source_root, source_path, is_stale, pending_reason, indexed_at_ms
ON catalog_sessions
BEGIN
    DELETE FROM import_pending_work
    WHERE inventory_family = 'catalog_sessions'
      AND provider = OLD.provider
      AND source_root = OLD.source_root
      AND source_path = OLD.source_path;
END;

CREATE TRIGGER IF NOT EXISTS trg_catalog_sessions_pending_work_update_new
AFTER UPDATE OF provider, source_root, source_path, is_stale, pending_reason, indexed_at_ms
ON catalog_sessions
WHEN NEW.is_stale = 0 AND NEW.pending_reason IN (
    'fresh_new', 'fresh_changed', 'fresh_append',
    'recovery_retry', 'recovery_replacement', 'parser_revision',
    'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'
)
BEGIN
    INSERT INTO import_pending_work (
        inventory_family, provider, source_root, source_path,
        work_class, indexed_at_ms
    ) VALUES (
        'catalog_sessions', NEW.provider, NEW.source_root, NEW.source_path,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        NEW.indexed_at_ms
    );
END;

CREATE TRIGGER IF NOT EXISTS trg_catalog_sessions_pending_work_delete
AFTER DELETE ON catalog_sessions
BEGIN
    DELETE FROM import_pending_work
    WHERE inventory_family = 'catalog_sessions'
      AND provider = OLD.provider
      AND source_root = OLD.source_root
      AND source_path = OLD.source_path;
END;

CREATE TRIGGER IF NOT EXISTS trg_source_import_files_pending_work_insert
AFTER INSERT ON source_import_files
WHEN NEW.is_stale = 0 AND NEW.pending_reason IN (
    'fresh_new', 'fresh_changed', 'fresh_append',
    'recovery_retry', 'recovery_replacement', 'parser_revision',
    'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'
)
BEGIN
    INSERT INTO import_pending_work (
        inventory_family, provider, source_root, source_path,
        work_class, indexed_at_ms
    ) VALUES (
        'source_import_files', NEW.provider, NEW.source_root, NEW.source_path,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        NEW.indexed_at_ms
    );
END;

CREATE TRIGGER IF NOT EXISTS trg_source_import_files_pending_work_update_old
BEFORE UPDATE OF provider, source_root, source_path, is_stale, pending_reason, indexed_at_ms
ON source_import_files
BEGIN
    DELETE FROM import_pending_work
    WHERE inventory_family = 'source_import_files'
      AND provider = OLD.provider
      AND source_root = OLD.source_root
      AND source_path = OLD.source_path;
END;

CREATE TRIGGER IF NOT EXISTS trg_source_import_files_pending_work_update_new
AFTER UPDATE OF provider, source_root, source_path, is_stale, pending_reason, indexed_at_ms
ON source_import_files
WHEN NEW.is_stale = 0 AND NEW.pending_reason IN (
    'fresh_new', 'fresh_changed', 'fresh_append',
    'recovery_retry', 'recovery_replacement', 'parser_revision',
    'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'
)
BEGIN
    INSERT INTO import_pending_work (
        inventory_family, provider, source_root, source_path,
        work_class, indexed_at_ms
    ) VALUES (
        'source_import_files', NEW.provider, NEW.source_root, NEW.source_path,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        NEW.indexed_at_ms
    );
END;

CREATE TRIGGER IF NOT EXISTS trg_source_import_files_pending_work_delete
AFTER DELETE ON source_import_files
BEGIN
    DELETE FROM import_pending_work
    WHERE inventory_family = 'source_import_files'
      AND provider = OLD.provider
      AND source_root = OLD.source_root
      AND source_path = OLD.source_path;
END;
"#;

const DROP_IMPORT_PENDING_WORK_PROJECTION_TRIGGERS_SQL: &str = r#"
DROP TRIGGER IF EXISTS trg_catalog_sessions_pending_work_insert;
DROP TRIGGER IF EXISTS trg_catalog_sessions_pending_work_update_old;
DROP TRIGGER IF EXISTS trg_catalog_sessions_pending_work_update_new;
DROP TRIGGER IF EXISTS trg_catalog_sessions_pending_work_delete;
DROP TRIGGER IF EXISTS trg_source_import_files_pending_work_insert;
DROP TRIGGER IF EXISTS trg_source_import_files_pending_work_update_old;
DROP TRIGGER IF EXISTS trg_source_import_files_pending_work_update_new;
DROP TRIGGER IF EXISTS trg_source_import_files_pending_work_delete;
"#;

pub(crate) fn install_import_pending_work_invariants(conn: &Connection) -> Result<()> {
    conn.execute_batch(IMPORT_PENDING_WORK_COUNT_TRIGGER_INVARIANTS_SQL)?;
    let projection_mode = conn.query_row(
        "SELECT selection_mode = 'projection' FROM import_pending_work_state WHERE singleton = 1",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if projection_mode {
        conn.execute_batch(IMPORT_PENDING_WORK_PROJECTION_TRIGGER_INVARIANTS_SQL)?;
    } else {
        conn.execute_batch(DROP_IMPORT_PENDING_WORK_PROJECTION_TRIGGERS_SQL)?;
    }
    Ok(())
}
