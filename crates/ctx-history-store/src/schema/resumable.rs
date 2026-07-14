use rusqlite::{params, Connection, OptionalExtension};

use crate::schema::ddl::{table_exists, CREATE_TABLES_SQL};
use crate::schema::fts::{drop_fts_table_if_exists, execute_fts_ddl_if_supported, FTS_TABLES_SQL};
use crate::schema::indexes::INDEXES_SQL;
use crate::schema::rebuild::{create_table_rebuild_sql, table_columns};
use crate::schema::views::{create_stable_sql_views, drop_stable_sql_views};
use crate::{Result, StoreError};

const PROGRESS_TABLE: &str = "ctx_schema_migration_progress";
const COPY_BATCH_ROWS: i64 = 512;
const COPY_BATCH_BYTES: u64 = 8 * 1024 * 1024;
const MIN_CURSOR: i64 = i64::MIN;

const V43_TABLES: &[&str] = &["capture_sources", "catalog_sessions", "source_import_files"];
const V44_TABLES: &[&str] = &[
    "capture_sources",
    "vcs_workspaces",
    "history_records",
    "artifacts",
    "sessions",
    "session_edges",
    "runs",
    "events",
    "vcs_changes",
    "history_record_links",
    "summaries",
    "files_touched",
    "record_edges",
    "sync_outbox",
];
const V45_TABLES: &[&str] = &["event_search_lookup"];
const V46_TABLES: &[&str] = V43_TABLES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MigrationDiskNeed {
    None,
    Fixed(u64),
    DatabaseAmplification(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MigrationStep {
    Progress,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Prepare,
    Tables,
    FtsDrop,
    FtsCreate,
    Indexes,
    Views,
    Finalize,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Tables => "tables",
            Self::FtsDrop => "fts_drop",
            Self::FtsCreate => "fts_create",
            Self::Indexes => "indexes",
            Self::Views => "views",
            Self::Finalize => "finalize",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "prepare" => Ok(Self::Prepare),
            "tables" => Ok(Self::Tables),
            "fts_drop" => Ok(Self::FtsDrop),
            "fts_create" => Ok(Self::FtsCreate),
            "indexes" => Ok(Self::Indexes),
            "views" => Ok(Self::Views),
            "finalize" => Ok(Self::Finalize),
            _ => Err(StoreError::Sql(rusqlite::Error::InvalidQuery)),
        }
    }
}

#[derive(Debug)]
struct Progress {
    target_version: i64,
    phase: Phase,
    item_index: usize,
    cursor: i64,
}

#[derive(Debug)]
struct ColumnMapping {
    destination: String,
    source_expression: String,
    new_expression: String,
}

pub(crate) fn target_for_version(user_version: i64) -> Option<i64> {
    match user_version {
        16..=42 => Some(43),
        43 => Some(44),
        44 => Some(45),
        45 => Some(46),
        _ => None,
    }
}

pub(crate) fn run_step(
    conn: &Connection,
    target_version: i64,
    mut revalidate: impl FnMut(MigrationDiskNeed, &'static str) -> Result<()>,
) -> Result<MigrationStep> {
    ensure_progress(conn, target_version, &mut revalidate)?;
    let progress = load_progress(conn)?.ok_or(rusqlite::Error::InvalidQuery)?;
    if progress.target_version != target_version {
        return Err(StoreError::Sql(rusqlite::Error::InvalidQuery));
    }
    match progress.phase {
        Phase::Prepare => prepare(conn, target_version, &mut revalidate),
        Phase::Tables => rebuild_table_step(conn, &progress, &mut revalidate),
        Phase::FtsDrop => fts_drop_step(conn, &progress, &mut revalidate),
        Phase::FtsCreate => fts_create_step(conn, &progress, &mut revalidate),
        Phase::Indexes => index_step(conn, &progress, &mut revalidate),
        Phase::Views => views_step(conn, &progress, &mut revalidate),
        Phase::Finalize => finalize(conn, &progress, &mut revalidate),
    }
}

fn ensure_progress(
    conn: &Connection,
    target_version: i64,
    revalidate: &mut impl FnMut(MigrationDiskNeed, &'static str) -> Result<()>,
) -> Result<()> {
    if table_exists(conn, PROGRESS_TABLE)? {
        return Ok(());
    }
    transaction(conn, || {
        revalidate(
            MigrationDiskNeed::Fixed(1024 * 1024),
            "ctx migration progress initialization",
        )?;
        conn.execute_batch(&format!(
            r#"
            CREATE TABLE {PROGRESS_TABLE} (
                singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
                target_version INTEGER NOT NULL,
                phase TEXT NOT NULL,
                item_index INTEGER NOT NULL,
                copy_cursor INTEGER NOT NULL
            );
            "#
        ))?;
        conn.execute(
            &format!(
                "INSERT INTO {PROGRESS_TABLE}
                 (singleton, target_version, phase, item_index, copy_cursor)
                 VALUES (1, ?1, ?2, 0, ?3)"
            ),
            params![target_version, Phase::Prepare.as_str(), MIN_CURSOR],
        )?;
        Ok(())
    })
}

fn load_progress(conn: &Connection) -> Result<Option<Progress>> {
    conn.query_row(
        &format!(
            "SELECT target_version, phase, item_index, copy_cursor
             FROM {PROGRESS_TABLE} WHERE singleton = 1"
        ),
        [],
        |row| {
            let phase: String = row.get(1)?;
            let item_index: i64 = row.get(2)?;
            Ok((row.get(0)?, phase, item_index, row.get(3)?))
        },
    )
    .optional()?
    .map(|(target_version, phase, item_index, cursor)| {
        Ok(Progress {
            target_version,
            phase: Phase::parse(&phase)?,
            item_index: usize::try_from(item_index)
                .map_err(|_| StoreError::Sql(rusqlite::Error::InvalidQuery))?,
            cursor,
        })
    })
    .transpose()
}

fn prepare(
    conn: &Connection,
    target_version: i64,
    revalidate: &mut impl FnMut(MigrationDiskNeed, &'static str) -> Result<()>,
) -> Result<MigrationStep> {
    transaction_with_foreign_keys_disabled(conn, || {
        revalidate(
            MigrationDiskNeed::Fixed(8 * 1024 * 1024),
            "ctx migration schema preparation",
        )?;
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if !tables_for_target(target_version).is_empty() {
            drop_stable_sql_views(conn)?;
        }
        update_progress(conn, Phase::Tables, 0, MIN_CURSOR)
    })?;
    Ok(MigrationStep::Progress)
}

fn rebuild_table_step(
    conn: &Connection,
    progress: &Progress,
    revalidate: &mut impl FnMut(MigrationDiskNeed, &'static str) -> Result<()>,
) -> Result<MigrationStep> {
    let tables = tables_for_target(progress.target_version);
    let Some(&table) = tables.get(progress.item_index) else {
        let next = phase_after_tables(progress.target_version);
        transaction(conn, || {
            revalidate(
                MigrationDiskNeed::Fixed(1024 * 1024),
                "ctx migration phase handoff",
            )?;
            update_progress(conn, next, 0, MIN_CURSOR)
        })?;
        return Ok(MigrationStep::Progress);
    };
    let shadow = shadow_table(table);
    if !table_exists(conn, table)? {
        transaction_with_foreign_keys_disabled(conn, || {
            revalidate(
                MigrationDiskNeed::Fixed(8 * 1024 * 1024),
                "ctx migration missing table creation",
            )?;
            conn.execute_batch(&create_table_rebuild_sql(table, table)?)?;
            update_progress(conn, Phase::Tables, progress.item_index + 1, MIN_CURSOR)
        })?;
        return Ok(MigrationStep::Progress);
    }
    if !table_exists(conn, &shadow)? {
        transaction_with_foreign_keys_disabled(conn, || {
            revalidate(
                MigrationDiskNeed::Fixed(8 * 1024 * 1024),
                "ctx migration shadow table creation",
            )?;
            conn.execute_batch(&create_table_rebuild_sql(table, &shadow)?)?;
            create_mirror_triggers(conn, progress.target_version, table, &shadow)?;
            update_progress(conn, Phase::Tables, progress.item_index, MIN_CURSOR)
        })?;
        return Ok(MigrationStep::Progress);
    }

    transaction_with_foreign_keys_disabled(conn, || {
        let mappings = column_mappings(conn, progress.target_version, table, &shadow)?;
        let candidate = copy_candidate(conn, table, &mappings, progress.cursor)?;
        let Some((cutoff, logical_bytes)) = candidate else {
            // Recheck under the same IMMEDIATE transaction that performs the
            // swap. A late raw SQLite writer either mirrored its row or blocks
            // here; no source row can be lost between certification and rename.
            if conn
                .query_row(
                    &format!("SELECT rowid FROM {} WHERE rowid > ?1 LIMIT 1", qi(table)),
                    params![progress.cursor],
                    |_| Ok(()),
                )
                .optional()?
                .is_some()
            {
                return Ok(());
            }
            revalidate(
                MigrationDiskNeed::Fixed(8 * 1024 * 1024),
                "ctx migration table publication",
            )?;
            drop_mirror_triggers(conn, table)?;
            conn.execute(&format!("DROP TABLE {}", qi(table)), [])?;
            conn.execute(
                &format!("ALTER TABLE {} RENAME TO {}", qi(&shadow), qi(table)),
                [],
            )?;
            update_progress(conn, Phase::Tables, progress.item_index + 1, MIN_CURSOR)?;
            return Ok(());
        };

        let estimated = logical_bytes.saturating_mul(3).max(8 * 1024 * 1024);
        revalidate(
            MigrationDiskNeed::Fixed(estimated),
            "ctx migration table copy batch",
        )?;
        let columns = mappings
            .iter()
            .map(|mapping| qi(&mapping.destination))
            .collect::<Vec<_>>();
        let source_expressions = mappings
            .iter()
            .map(|mapping| mapping.source_expression.as_str())
            .collect::<Vec<_>>();
        let column_list = columns.join(", ");
        let source_list = source_expressions.join(", ");
        conn.execute(
            &format!(
                "INSERT OR REPLACE INTO {} (rowid, {column_list})
                 SELECT rowid, {source_list} FROM {}
                 WHERE rowid > ?1 AND rowid <= ?2 ORDER BY rowid",
                qi(&shadow),
                qi(table),
            ),
            params![progress.cursor, cutoff],
        )?;
        update_progress(conn, Phase::Tables, progress.item_index, cutoff)
    })?;
    Ok(MigrationStep::Progress)
}

fn fts_drop_step(
    conn: &Connection,
    progress: &Progress,
    revalidate: &mut impl FnMut(MigrationDiskNeed, &'static str) -> Result<()>,
) -> Result<MigrationStep> {
    let tables = fts_table_names();
    let Some(table) = tables.get(progress.item_index) else {
        transaction(conn, || {
            update_progress(conn, Phase::FtsCreate, 0, MIN_CURSOR)
        })?;
        return Ok(MigrationStep::Progress);
    };
    transaction(conn, || {
        // DROP is reclaiming work. Requiring free-space reserve here can make a
        // full database impossible to recover; physical writes are still
        // measured and paced after the admitted slice.
        revalidate(MigrationDiskNeed::None, "ctx migration FTS cleanup")?;
        drop_fts_table_if_exists(conn, table)?;
        update_progress(conn, Phase::FtsDrop, progress.item_index + 1, MIN_CURSOR)
    })?;
    Ok(MigrationStep::Progress)
}

fn fts_create_step(
    conn: &Connection,
    progress: &Progress,
    revalidate: &mut impl FnMut(MigrationDiskNeed, &'static str) -> Result<()>,
) -> Result<MigrationStep> {
    let statements = sql_statements(FTS_TABLES_SQL);
    let Some(statement) = statements.get(progress.item_index) else {
        transaction(conn, || {
            update_progress(conn, Phase::Finalize, 0, MIN_CURSOR)
        })?;
        return Ok(MigrationStep::Progress);
    };
    transaction(conn, || {
        revalidate(
            MigrationDiskNeed::Fixed(8 * 1024 * 1024),
            "ctx migration FTS creation",
        )?;
        execute_fts_ddl_if_supported(conn, statement)?;
        update_progress(conn, Phase::FtsCreate, progress.item_index + 1, MIN_CURSOR)
    })?;
    Ok(MigrationStep::Progress)
}

fn index_step(
    conn: &Connection,
    progress: &Progress,
    revalidate: &mut impl FnMut(MigrationDiskNeed, &'static str) -> Result<()>,
) -> Result<MigrationStep> {
    let statements = sql_statements(INDEXES_SQL);
    let Some(statement) = statements.get(progress.item_index) else {
        transaction(conn, || update_progress(conn, Phase::Views, 0, MIN_CURSOR))?;
        return Ok(MigrationStep::Progress);
    };
    transaction(conn, || {
        revalidate(
            MigrationDiskNeed::DatabaseAmplification(2),
            "ctx migration index creation",
        )?;
        conn.execute_batch(statement)?;
        update_progress(conn, Phase::Indexes, progress.item_index + 1, MIN_CURSOR)
    })?;
    Ok(MigrationStep::Progress)
}

fn views_step(
    conn: &Connection,
    progress: &Progress,
    revalidate: &mut impl FnMut(MigrationDiskNeed, &'static str) -> Result<()>,
) -> Result<MigrationStep> {
    transaction(conn, || {
        revalidate(
            MigrationDiskNeed::Fixed(1024 * 1024),
            "ctx migration view creation",
        )?;
        create_stable_sql_views(conn)?;
        update_progress(conn, Phase::Finalize, progress.item_index, MIN_CURSOR)
    })?;
    Ok(MigrationStep::Progress)
}

fn finalize(
    conn: &Connection,
    progress: &Progress,
    revalidate: &mut impl FnMut(MigrationDiskNeed, &'static str) -> Result<()>,
) -> Result<MigrationStep> {
    transaction(conn, || {
        revalidate(
            MigrationDiskNeed::Fixed(1024 * 1024),
            "ctx migration finalization",
        )?;
        conn.execute_batch(&format!(
            "PRAGMA user_version = {}",
            progress.target_version
        ))?;
        conn.execute(&format!("DROP TABLE {PROGRESS_TABLE}"), [])?;
        Ok(())
    })?;
    Ok(MigrationStep::Complete)
}

fn phase_after_tables(target_version: i64) -> Phase {
    match target_version {
        45 => Phase::FtsDrop,
        46 => Phase::Indexes,
        _ => Phase::Finalize,
    }
}

fn tables_for_target(target_version: i64) -> &'static [&'static str] {
    match target_version {
        43 => V43_TABLES,
        44 => V44_TABLES,
        45 => V45_TABLES,
        46 => V46_TABLES,
        _ => &[],
    }
}

fn update_progress(conn: &Connection, phase: Phase, item_index: usize, cursor: i64) -> Result<()> {
    conn.execute(
        &format!(
            "UPDATE {PROGRESS_TABLE}
             SET phase = ?1, item_index = ?2, copy_cursor = ?3
             WHERE singleton = 1"
        ),
        params![
            phase.as_str(),
            i64::try_from(item_index).map_err(|_| rusqlite::Error::InvalidQuery)?,
            cursor
        ],
    )?;
    Ok(())
}

fn column_mappings(
    conn: &Connection,
    target_version: i64,
    table: &str,
    shadow: &str,
) -> Result<Vec<ColumnMapping>> {
    let old = table_columns(conn, table)?;
    let new = table_columns(conn, shadow)?;
    let mut mappings = Vec::new();
    for column in new {
        let old_has_column = old.iter().any(|old| old == &column);
        let source_root_backfill = target_version == 43
            && table == "capture_sources"
            && column == "source_root"
            && old.iter().any(|old| old == "raw_source_path");
        if source_root_backfill {
            let source_expression = if old_has_column {
                format!("COALESCE({}, {})", qi("source_root"), qi("raw_source_path"))
            } else {
                qi("raw_source_path")
            };
            mappings.push(ColumnMapping {
                destination: column,
                source_expression,
                new_expression: if old_has_column {
                    format!(
                        "COALESCE(NEW.{}, NEW.{})",
                        qi("source_root"),
                        qi("raw_source_path")
                    )
                } else {
                    format!("NEW.{}", qi("raw_source_path"))
                },
            });
        } else if old_has_column {
            mappings.push(ColumnMapping {
                destination: column.clone(),
                source_expression: qi(&column),
                new_expression: format!("NEW.{}", qi(&column)),
            });
        }
    }
    Ok(mappings)
}

fn copy_candidate(
    conn: &Connection,
    table: &str,
    mappings: &[ColumnMapping],
    cursor: i64,
) -> Result<Option<(i64, u64)>> {
    let size_expression = if mappings.is_empty() {
        "0".to_owned()
    } else {
        mappings
            .iter()
            .map(|mapping| {
                format!(
                    "COALESCE(length(CAST({} AS BLOB)), 0)",
                    mapping.source_expression
                )
            })
            .collect::<Vec<_>>()
            .join(" + ")
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT rowid, {size_expression} FROM {}
         WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
        qi(table)
    ))?;
    let mut rows = stmt.query(params![cursor, COPY_BATCH_ROWS])?;
    let mut cutoff = None;
    let mut bytes = 0_u64;
    while let Some(row) = rows.next()? {
        let rowid: i64 = row.get(0)?;
        let row_bytes = u64::try_from(row.get::<_, i64>(1)?.max(0)).unwrap_or(u64::MAX);
        if cutoff.is_some() && bytes.saturating_add(row_bytes) > COPY_BATCH_BYTES {
            break;
        }
        cutoff = Some(rowid);
        bytes = bytes.saturating_add(row_bytes);
        if bytes >= COPY_BATCH_BYTES {
            break;
        }
    }
    Ok(cutoff.map(|cutoff| (cutoff, bytes)))
}

fn create_mirror_triggers(
    conn: &Connection,
    target_version: i64,
    table: &str,
    shadow: &str,
) -> Result<()> {
    drop_mirror_triggers(conn, table)?;
    let mappings = column_mappings(conn, target_version, table, shadow)?;
    if mappings.is_empty() {
        return Err(StoreError::Sql(rusqlite::Error::InvalidQuery));
    }
    let names = mappings
        .iter()
        .map(|mapping| qi(&mapping.destination))
        .collect::<Vec<_>>();
    let new_values = mappings
        .iter()
        .map(|mapping| mapping.new_expression.as_str())
        .collect::<Vec<_>>();
    let column_list = names.join(", ");
    let value_list = new_values.join(", ");
    let insert = mirror_trigger(table, "insert");
    let update = mirror_trigger(table, "update");
    let delete = mirror_trigger(table, "delete");
    conn.execute_batch(&format!(
        r#"
        CREATE TRIGGER {} AFTER INSERT ON {}
        BEGIN
            INSERT OR REPLACE INTO {} (rowid, {column_list})
            VALUES (NEW.rowid, {value_list});
        END;
        CREATE TRIGGER {} AFTER UPDATE ON {}
        BEGIN
            DELETE FROM {} WHERE rowid = OLD.rowid;
            INSERT OR REPLACE INTO {} (rowid, {column_list})
            VALUES (NEW.rowid, {value_list});
        END;
        CREATE TRIGGER {} AFTER DELETE ON {}
        BEGIN
            DELETE FROM {} WHERE rowid = OLD.rowid;
        END;
        "#,
        qi(&insert),
        qi(table),
        qi(shadow),
        qi(&update),
        qi(table),
        qi(shadow),
        qi(shadow),
        qi(&delete),
        qi(table),
        qi(shadow),
    ))?;
    Ok(())
}

fn drop_mirror_triggers(conn: &Connection, table: &str) -> Result<()> {
    for operation in ["insert", "update", "delete"] {
        conn.execute(
            &format!(
                "DROP TRIGGER IF EXISTS {}",
                qi(&mirror_trigger(table, operation))
            ),
            [],
        )?;
    }
    Ok(())
}

fn mirror_trigger(table: &str, operation: &str) -> String {
    format!("ctx_migrate_{table}_{operation}")
}

fn shadow_table(table: &str) -> String {
    format!("ctx_migrate_shadow_{table}")
}

fn fts_table_names() -> Vec<&'static str> {
    vec![
        "ctx_history_search",
        "event_search",
        "artifact_search",
        "ctx_history_search_scriptgram",
        "event_search_scriptgram",
    ]
}

fn sql_statements(sql: &str) -> Vec<&str> {
    sql.split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .collect()
}

fn qi(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn transaction(conn: &Connection, operation: impl FnOnce() -> Result<()>) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    finish_transaction(conn, operation())
}

fn transaction_with_foreign_keys_disabled(
    conn: &Connection,
    operation: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let foreign_keys: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    if foreign_keys != 0 {
        conn.execute_batch("PRAGMA foreign_keys = OFF")?;
    }
    let result = transaction(conn, operation);
    if conn.is_autocommit() && foreign_keys != 0 {
        conn.execute_batch("PRAGMA foreign_keys = ON")?;
    }
    result
}

fn finish_transaction(conn: &Connection, result: Result<()>) -> Result<()> {
    match result {
        Ok(()) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                if !conn.is_autocommit() {
                    let _ = conn.execute_batch("ROLLBACK");
                }
                return Err(error.into());
            }
            Ok(())
        }
        Err(error) => {
            if let Err(rollback_error) = conn.execute_batch("ROLLBACK") {
                return Err(StoreError::Sql(rollback_error));
            }
            Err(error)
        }
    }
}
