use rusqlite::Connection;

use crate::schema::ddl::{ensure_columns, ColumnSpec};
use crate::Result;

pub(crate) const IMPORT_PENDING_WORK_PROJECTION_VERSION: i64 = 2;

const PENDING_REASON_REPAIR_V2_COLUMNS: &[ColumnSpec] = &[ColumnSpec {
    name: "cursor_rowid",
    definition: "cursor_rowid INTEGER NOT NULL DEFAULT 0",
}];

const PENDING_WORK_V2_COLUMNS: &[ColumnSpec] = &[ColumnSpec {
    name: "projection_version",
    definition: "projection_version INTEGER NOT NULL DEFAULT 1",
}];

const PENDING_WORK_STATE_V2_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec {
        name: "projection_version",
        definition: "projection_version INTEGER NOT NULL DEFAULT 1",
    },
    ColumnSpec {
        name: "legacy_cleanup_complete",
        definition: "legacy_cleanup_complete INTEGER NOT NULL DEFAULT 0",
    },
    ColumnSpec {
        name: "legacy_cleanup_phase",
        definition: "legacy_cleanup_phase TEXT NOT NULL DEFAULT 'work'",
    },
    ColumnSpec {
        name: "legacy_cleanup_inventory_family",
        definition: "legacy_cleanup_inventory_family TEXT NOT NULL DEFAULT ''",
    },
    ColumnSpec {
        name: "legacy_cleanup_provider",
        definition: "legacy_cleanup_provider TEXT NOT NULL DEFAULT ''",
    },
    ColumnSpec {
        name: "legacy_cleanup_source_root",
        definition: "legacy_cleanup_source_root TEXT NOT NULL DEFAULT ''",
    },
    ColumnSpec {
        name: "legacy_cleanup_tail",
        definition: "legacy_cleanup_tail TEXT NOT NULL DEFAULT ''",
    },
    ColumnSpec {
        name: "material_cursor_rowid",
        definition: "material_cursor_rowid INTEGER NOT NULL DEFAULT 0",
    },
    ColumnSpec {
        name: "material_scan_complete",
        definition: "material_scan_complete INTEGER NOT NULL DEFAULT 0",
    },
];

const LEGACY_MATERIAL_OWNER_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS import_pending_legacy_material_owners (
    projection_version INTEGER NOT NULL CHECK (projection_version > 0),
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('root', 'path')),
    provider TEXT NOT NULL,
    source_format TEXT NOT NULL,
    owner_source_root TEXT NOT NULL,
    source_path TEXT NOT NULL,
    capture_source_id TEXT NOT NULL,
    PRIMARY KEY (
      projection_version, owner_kind, provider, source_format,
      owner_source_root, source_path, capture_source_id
    )
) WITHOUT ROWID;
"#;

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
        inventory_family, provider, source_root, work_class, pending_count, projection_version
    ) VALUES (
        'catalog_sessions', NEW.provider, NEW.source_root,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        1, 2
    )
    ON CONFLICT (inventory_family, provider, source_root, work_class)
    DO UPDATE SET pending_count = pending_count + 1, projection_version = 2;
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
      AND projection_version = 2
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
      END
      AND projection_version = 2;
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
        inventory_family, provider, source_root, work_class, pending_count, projection_version
    ) VALUES (
        'catalog_sessions', NEW.provider, NEW.source_root,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        1, 2
    )
    ON CONFLICT (inventory_family, provider, source_root, work_class)
    DO UPDATE SET pending_count = pending_count + 1, projection_version = 2;
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
      AND projection_version = 2
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
      END
      AND projection_version = 2;
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
        inventory_family, provider, source_root, work_class, pending_count, projection_version
    ) VALUES (
        'source_import_files', NEW.provider, NEW.source_root,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        1, 2
    )
    ON CONFLICT (inventory_family, provider, source_root, work_class)
    DO UPDATE SET pending_count = pending_count + 1, projection_version = 2;
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
      AND projection_version = 2
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
      END
      AND projection_version = 2;
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
        inventory_family, provider, source_root, work_class, pending_count, projection_version
    ) VALUES (
        'source_import_files', NEW.provider, NEW.source_root,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        1, 2
    )
    ON CONFLICT (inventory_family, provider, source_root, work_class)
    DO UPDATE SET pending_count = pending_count + 1, projection_version = 2;
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
      AND projection_version = 2
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
      END
      AND projection_version = 2;
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
        work_class, indexed_at_ms, projection_version
    ) VALUES (
        'catalog_sessions', NEW.provider, NEW.source_root, NEW.source_path,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        NEW.indexed_at_ms, 2
    )
    ON CONFLICT (inventory_family, provider, source_root, source_path)
    DO UPDATE SET work_class = excluded.work_class,
                  indexed_at_ms = excluded.indexed_at_ms,
                  projection_version = 2;

    INSERT INTO import_pending_work_counts (
        inventory_family, provider, source_root, work_class,
        pending_count, projection_version
    ) VALUES (
        'catalog_sessions', NEW.provider, NEW.source_root,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        1, 2
    )
    ON CONFLICT (inventory_family, provider, source_root, work_class)
    DO UPDATE SET pending_count = CASE
                      WHEN projection_version = 2 THEN pending_count + 1
                      ELSE 1
                  END,
                  projection_version = 2;
END;

CREATE TRIGGER IF NOT EXISTS trg_catalog_sessions_pending_work_update_old
BEFORE UPDATE OF provider, source_root, source_path, is_stale, pending_reason, indexed_at_ms
ON catalog_sessions
BEGIN
    DELETE FROM import_pending_work_counts
    WHERE inventory_family = 'catalog_sessions'
      AND provider = OLD.provider AND source_root = OLD.source_root
      AND work_class = (
        SELECT work_class FROM import_pending_work
        WHERE inventory_family = 'catalog_sessions'
          AND provider = OLD.provider AND source_root = OLD.source_root
          AND source_path = OLD.source_path AND projection_version = 2
      )
      AND projection_version = 2 AND pending_count = 1;

    UPDATE import_pending_work_counts
    SET pending_count = pending_count - 1
    WHERE inventory_family = 'catalog_sessions'
      AND provider = OLD.provider AND source_root = OLD.source_root
      AND work_class = (
        SELECT work_class FROM import_pending_work
        WHERE inventory_family = 'catalog_sessions'
          AND provider = OLD.provider AND source_root = OLD.source_root
          AND source_path = OLD.source_path AND projection_version = 2
      )
      AND projection_version = 2;

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
        work_class, indexed_at_ms, projection_version
    ) VALUES (
        'catalog_sessions', NEW.provider, NEW.source_root, NEW.source_path,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        NEW.indexed_at_ms, 2
    )
    ON CONFLICT (inventory_family, provider, source_root, source_path)
    DO UPDATE SET work_class = excluded.work_class,
                  indexed_at_ms = excluded.indexed_at_ms,
                  projection_version = 2;

    INSERT INTO import_pending_work_counts (
        inventory_family, provider, source_root, work_class,
        pending_count, projection_version
    ) VALUES (
        'catalog_sessions', NEW.provider, NEW.source_root,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        1, 2
    )
    ON CONFLICT (inventory_family, provider, source_root, work_class)
    DO UPDATE SET pending_count = CASE
                      WHEN projection_version = 2 THEN pending_count + 1
                      ELSE 1
                  END,
                  projection_version = 2;
END;

CREATE TRIGGER IF NOT EXISTS trg_catalog_sessions_pending_work_delete
AFTER DELETE ON catalog_sessions
BEGIN
    DELETE FROM import_pending_work_counts
    WHERE inventory_family = 'catalog_sessions'
      AND provider = OLD.provider AND source_root = OLD.source_root
      AND work_class = (
        SELECT work_class FROM import_pending_work
        WHERE inventory_family = 'catalog_sessions'
          AND provider = OLD.provider AND source_root = OLD.source_root
          AND source_path = OLD.source_path AND projection_version = 2
      )
      AND projection_version = 2 AND pending_count = 1;

    UPDATE import_pending_work_counts
    SET pending_count = pending_count - 1
    WHERE inventory_family = 'catalog_sessions'
      AND provider = OLD.provider AND source_root = OLD.source_root
      AND work_class = (
        SELECT work_class FROM import_pending_work
        WHERE inventory_family = 'catalog_sessions'
          AND provider = OLD.provider AND source_root = OLD.source_root
          AND source_path = OLD.source_path AND projection_version = 2
      )
      AND projection_version = 2;

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
        work_class, indexed_at_ms, projection_version
    ) VALUES (
        'source_import_files', NEW.provider, NEW.source_root, NEW.source_path,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        NEW.indexed_at_ms, 2
    )
    ON CONFLICT (inventory_family, provider, source_root, source_path)
    DO UPDATE SET work_class = excluded.work_class,
                  indexed_at_ms = excluded.indexed_at_ms,
                  projection_version = 2;

    INSERT INTO import_pending_work_counts (
        inventory_family, provider, source_root, work_class,
        pending_count, projection_version
    ) VALUES (
        'source_import_files', NEW.provider, NEW.source_root,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        1, 2
    )
    ON CONFLICT (inventory_family, provider, source_root, work_class)
    DO UPDATE SET pending_count = CASE
                      WHEN projection_version = 2 THEN pending_count + 1
                      ELSE 1
                  END,
                  projection_version = 2;
END;

CREATE TRIGGER IF NOT EXISTS trg_source_import_files_pending_work_update_old
BEFORE UPDATE OF provider, source_root, source_path, is_stale, pending_reason, indexed_at_ms
ON source_import_files
BEGIN
    DELETE FROM import_pending_work_counts
    WHERE inventory_family = 'source_import_files'
      AND provider = OLD.provider AND source_root = OLD.source_root
      AND work_class = (
        SELECT work_class FROM import_pending_work
        WHERE inventory_family = 'source_import_files'
          AND provider = OLD.provider AND source_root = OLD.source_root
          AND source_path = OLD.source_path AND projection_version = 2
      )
      AND projection_version = 2 AND pending_count = 1;

    UPDATE import_pending_work_counts
    SET pending_count = pending_count - 1
    WHERE inventory_family = 'source_import_files'
      AND provider = OLD.provider AND source_root = OLD.source_root
      AND work_class = (
        SELECT work_class FROM import_pending_work
        WHERE inventory_family = 'source_import_files'
          AND provider = OLD.provider AND source_root = OLD.source_root
          AND source_path = OLD.source_path AND projection_version = 2
      )
      AND projection_version = 2;

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
        work_class, indexed_at_ms, projection_version
    ) VALUES (
        'source_import_files', NEW.provider, NEW.source_root, NEW.source_path,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        NEW.indexed_at_ms, 2
    )
    ON CONFLICT (inventory_family, provider, source_root, source_path)
    DO UPDATE SET work_class = excluded.work_class,
                  indexed_at_ms = excluded.indexed_at_ms,
                  projection_version = 2;

    INSERT INTO import_pending_work_counts (
        inventory_family, provider, source_root, work_class,
        pending_count, projection_version
    ) VALUES (
        'source_import_files', NEW.provider, NEW.source_root,
        CASE
            WHEN NEW.pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append')
            THEN 'fresh'
            ELSE 'recovery'
        END,
        1, 2
    )
    ON CONFLICT (inventory_family, provider, source_root, work_class)
    DO UPDATE SET pending_count = CASE
                      WHEN projection_version = 2 THEN pending_count + 1
                      ELSE 1
                  END,
                  projection_version = 2;
END;

CREATE TRIGGER IF NOT EXISTS trg_source_import_files_pending_work_delete
AFTER DELETE ON source_import_files
BEGIN
    DELETE FROM import_pending_work_counts
    WHERE inventory_family = 'source_import_files'
      AND provider = OLD.provider AND source_root = OLD.source_root
      AND work_class = (
        SELECT work_class FROM import_pending_work
        WHERE inventory_family = 'source_import_files'
          AND provider = OLD.provider AND source_root = OLD.source_root
          AND source_path = OLD.source_path AND projection_version = 2
      )
      AND projection_version = 2 AND pending_count = 1;

    UPDATE import_pending_work_counts
    SET pending_count = pending_count - 1
    WHERE inventory_family = 'source_import_files'
      AND provider = OLD.provider AND source_root = OLD.source_root
      AND work_class = (
        SELECT work_class FROM import_pending_work
        WHERE inventory_family = 'source_import_files'
          AND provider = OLD.provider AND source_root = OLD.source_root
          AND source_path = OLD.source_path AND projection_version = 2
      )
      AND projection_version = 2;

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

const DROP_IMPORT_PENDING_WORK_COUNT_TRIGGERS_SQL: &str = r#"
DROP TRIGGER IF EXISTS trg_catalog_sessions_pending_count_insert;
DROP TRIGGER IF EXISTS trg_catalog_sessions_pending_count_update_old;
DROP TRIGGER IF EXISTS trg_catalog_sessions_pending_count_update_new;
DROP TRIGGER IF EXISTS trg_catalog_sessions_pending_count_delete;
DROP TRIGGER IF EXISTS trg_source_import_files_pending_count_insert;
DROP TRIGGER IF EXISTS trg_source_import_files_pending_count_update_old;
DROP TRIGGER IF EXISTS trg_source_import_files_pending_count_update_new;
DROP TRIGGER IF EXISTS trg_source_import_files_pending_count_delete;
"#;

pub(crate) fn ensure_import_pending_work_projection_v2(conn: &Connection) -> Result<()> {
    ensure_columns(
        conn,
        "import_pending_reason_repairs",
        PENDING_REASON_REPAIR_V2_COLUMNS,
    )?;
    ensure_columns(conn, "import_pending_work", PENDING_WORK_V2_COLUMNS)?;
    ensure_columns(conn, "import_pending_work_counts", PENDING_WORK_V2_COLUMNS)?;
    ensure_columns(
        conn,
        "import_pending_work_state",
        PENDING_WORK_STATE_V2_COLUMNS,
    )?;
    conn.execute_batch(LEGACY_MATERIAL_OWNER_TABLE_SQL)?;
    let version = conn.query_row(
        "SELECT projection_version FROM import_pending_work_state WHERE singleton = 1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if version != IMPORT_PENDING_WORK_PROJECTION_VERSION {
        conn.execute_batch(
            r#"
            UPDATE import_pending_work_state
            SET selection_mode = 'projection', projection_version = 2,
                legacy_cleanup_complete = 0,
                legacy_cleanup_phase = 'work',
                legacy_cleanup_inventory_family = '',
                legacy_cleanup_provider = '',
                legacy_cleanup_source_root = '',
                legacy_cleanup_tail = '',
                material_cursor_rowid = 0, material_scan_complete = 0
            WHERE singleton = 1;

            UPDATE import_pending_reason_repairs
            SET cursor_provider = NULL, cursor_source_root = NULL,
                cursor_source_path = NULL, cursor_rowid = 0, completed = 0
            WHERE inventory_family IN ('catalog_sessions', 'source_import_files');
            "#,
        )?;
    }
    Ok(())
}

pub(crate) fn install_import_pending_work_invariants(conn: &Connection) -> Result<()> {
    let projection_mode = conn.query_row(
        "SELECT selection_mode = 'projection' FROM import_pending_work_state WHERE singleton = 1",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if projection_mode {
        conn.execute_batch(DROP_IMPORT_PENDING_WORK_COUNT_TRIGGERS_SQL)?;
        conn.execute_batch(DROP_IMPORT_PENDING_WORK_PROJECTION_TRIGGERS_SQL)?;
        conn.execute_batch(IMPORT_PENDING_WORK_PROJECTION_TRIGGER_INVARIANTS_SQL)?;
    } else {
        conn.execute_batch(DROP_IMPORT_PENDING_WORK_PROJECTION_TRIGGERS_SQL)?;
        conn.execute_batch(DROP_IMPORT_PENDING_WORK_COUNT_TRIGGERS_SQL)?;
        conn.execute_batch(IMPORT_PENDING_WORK_COUNT_TRIGGER_INVARIANTS_SQL)?;
    }
    Ok(())
}
