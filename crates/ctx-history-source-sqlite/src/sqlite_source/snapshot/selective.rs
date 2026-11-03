//! Table-selective private copies. Provider policy supplies the table allowlist;
//! this layer owns the consistent no-write read, disk bound, and cleanup.
use super::*;

/// Explicit opt-in to omit oversized TEXT/BLOB cells from selected tables.
/// Omitted values retain their storage class as empty cells. Original byte
/// lengths are stored in `BYTES_COLUMN`; native columns may not use that name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqliteSourceColumnBound {
    pub tables: &'static [&'static str],
    pub column: &'static str,
    pub maximum_bytes: usize,
}

impl SqliteSourceColumnBound {
    pub const BYTES_COLUMN: &'static str = "__ctx_original_bytes";
}

impl SqliteSourceDirectoryAuthority {
    /// Captures the named ordinary tables and their indexes into one private
    /// database, streaming native values from one read transaction. Missing
    /// tables remain missing so the adapter retains schema-selection authority.
    /// Platforms without the no-write live reader retain full-family capture.
    pub fn open_selected_tables_snapshot_with_progress<E>(
        &self,
        database_name: &OsStr,
        tables: &'static [&'static str],
        report_progress: impl FnMut(SqliteSourceProgress) -> Result<(), E>,
    ) -> Result<SqliteSourceReadSnapshot, SqliteSourceProgressError<E>> {
        self.open_bounded_tables_snapshot_with_progress(
            database_name,
            tables,
            None,
            report_progress,
        )
    }

    /// Applies an explicit column bound before SQLite materializes selected cells.
    /// Bounded tables retain column types/collations and ordinary keys, omitting
    /// payload-dependent constraints, expression indexes, and partial indexes.
    /// Platforms without the no-write reader retain full-family capture.
    pub fn open_bounded_tables_snapshot_with_progress<E>(
        &self,
        database_name: &OsStr,
        tables: &'static [&'static str],
        bound: Option<SqliteSourceColumnBound>,
        mut report_progress: impl FnMut(SqliteSourceProgress) -> Result<(), E>,
    ) -> Result<SqliteSourceReadSnapshot, SqliteSourceProgressError<E>> {
        let policy = if selective_reader_available() {
            SqliteSourceSnapshotPolicy::SelectivePrivateCopy(tables, bound)
        } else {
            SqliteSourceSnapshotPolicy::StablePrivateCopy
        };
        open_root_handle_sqlite_source_snapshot_with_progress(
            self,
            database_name,
            policy,
            SqliteSourceSnapshotLimits::default(),
            &mut report_progress,
        )
    }
}

fn selective_reader_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        (unsafe { libc::geteuid() }) != 0
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

pub(super) fn acquire<E>(
    context: &Arc<SqliteSourceSnapshotContext>,
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
    limits: SqliteSourceSnapshotLimits,
    tables: &[&str],
    bound: Option<SqliteSourceColumnBound>,
    report: &mut impl FnMut(SqliteSourceProgress) -> Result<(), E>,
) -> Result<AcquiredSqliteConnection, SqliteSourceProgressError<E>> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (context, family, evidence, limits, tables, bound, report);
        Err(SqliteSourceAccessError::SnapshotUnavailable {
            reason: "selective snapshots require the Linux no-write SQLite reader".into(),
        }
        .into())
    }
    #[cfg(target_os = "linux")]
    {
        let (source, _authority) = open_pinned_read_only_wal(family)?;
        let operation = (|| {
            verify_connection_read_only(&source)?;
            configure_and_pin_snapshot(&source)?;
            family.revalidate_database_identity(evidence)?;
            copy_tables(context, &source, limits, tables, bound, report)
        })();
        // Close the live transaction before any adapter query. A failed close
        // also discards the private candidate; no second allocation is opened.
        let close = close_snapshot_read_connection(source, SqliteArtifactKind::ProviderDatabase);
        match (operation, close) {
            (Ok(copy), Ok(())) => Ok(copy),
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(close)) => Err(error.with_finalization(close)),
            (Ok(copy), Err(close)) => match copy.cleanup() {
                Ok(()) => Err(close.into()),
                Err(cleanup) => Err(SqliteSourceAccessError::Finalization {
                    primary: Box::new(close),
                    cleanup: Box::new(cleanup),
                }
                .into()),
            },
        }
    }
}

#[cfg(target_os = "linux")]
fn copy_tables<E>(
    context: &Arc<SqliteSourceSnapshotContext>,
    source: &Connection,
    limits: SqliteSourceSnapshotLimits,
    tables: &[&str],
    bound: Option<SqliteSourceColumnBound>,
    report: &mut impl FnMut(SqliteSourceProgress) -> Result<(), E>,
) -> Result<AcquiredSqliteConnection, SqliteSourceProgressError<E>> {
    let scratch = SqliteRouteScratch::new(context, limits.maximum_scratch_limit());
    let capacity = scratch.admit_available_capacity()?;
    let directory = create_snapshot_directory(&context.data_root, "provider-sqlite-snapshot-")?;
    let path = directory.path().join("source.sqlite");
    let operation = (|| {
        let target = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(private_error)?;
        let copied: Result<u64, SqliteSourceProgressError<E>> = (|| {
            target
                .set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)
                .map_err(private_error)?;
            // Disposable, unpublished output needs no rollback journal. Indexes
            // are created while empty and maintained per row, never bulk-sorted.
            target
                .execute_batch(
                    "PRAGMA page_size=4096; PRAGMA journal_mode=OFF;
                PRAGMA synchronous=OFF; PRAGMA cache_size=-512; PRAGMA mmap_size=0;
                PRAGMA temp_store=MEMORY; PRAGMA foreign_keys=OFF;
                PRAGMA ignore_check_constraints=ON;",
                )
                .map_err(private_error)?;
            target
                .pragma_update(None, "max_page_count", (capacity / 4096).max(1))
                .map_err(private_error)?;
            target.execute_batch("BEGIN").map_err(private_error)?;
            let mut bytes = 0_u64;
            report_progress(report, bytes)?;
            for table in tables {
                copy_table(source, &target, table, bound, &mut bytes, report)?;
            }
            target.execute_batch("COMMIT").map_err(private_error)?;
            report_progress(report, bytes)?;
            Ok(bytes)
        })();
        let close = target.close().map_err(|(_, error)| private_error(error));
        let bytes = match (copied, close) {
            (Ok(bytes), Ok(())) => bytes,
            (Err(error), Ok(())) => return Err(error),
            (Err(error), Err(close)) => return Err(error.with_finalization(close)),
            (Ok(_), Err(close)) => return Err(close.into()),
        };
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(private_error)?;
        Ok((connection, bytes))
    })();
    match operation {
        Ok((connection, copied_bytes)) => Ok(AcquiredSqliteConnection {
            connection,
            strategy: SqliteSourceSnapshotStrategy::SelectiveTables,
            copied_bytes,
            snapshot_directory: Some(directory),
            live_authority_handle: None,
            snapshot_activity: None,
            scratch,
        }),
        Err(error) => match close_private_snapshot_directory(
            directory,
            SqliteArtifactKind::PrivateSourceCopy,
            0,
            0,
        ) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(error.with_finalization(cleanup)),
        },
    }
}

#[cfg(target_os = "linux")]
fn copy_table<E>(
    source: &Connection,
    target: &Connection,
    table: &str,
    bound: Option<SqliteSourceColumnBound>,
    bytes: &mut u64,
    report: &mut impl FnMut(SqliteSourceProgress) -> Result<(), E>,
) -> Result<(), SqliteSourceProgressError<E>> {
    use rusqlite::{types::ValueRef, OptionalExtension};
    let schema: Option<String> = source
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name=?1",
            [table],
            |row| row.get(0),
        )
        .optional()
        .map_err(source_error)?;
    let Some(schema) = schema else {
        return Ok(());
    };
    let quoted = format!("\"{}\"", table.replace('"', "\"\""));
    // Only ordinary stored columns have a byte-preserving copy contract.
    let hidden: bool = source
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_xinfo(?1) WHERE hidden != 0)",
            [table],
            |row| row.get(0),
        )
        .map_err(source_error)?;
    if hidden || schema.to_ascii_uppercase().contains("CREATE VIRTUAL TABLE") {
        return Err(SqliteSourceAccessError::SnapshotUnavailable {
            reason: "selective SQLite capture requires ordinary stored tables".into(),
        }
        .into());
    }
    // OpenCode's accepted schemas are ordinary rowid tables. Preserve sparse,
    // negative and explicitly assigned rowids used by adapter diagnostics.
    let shadowed: bool = source
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE lower(name)='rowid')",
            [table],
            |row| row.get(0),
        )
        .map_err(source_error)?;
    let without_rowid: bool = source
        .query_row(
            "SELECT wr FROM pragma_table_list WHERE schema='main' AND name=?1",
            [table],
            |row| row.get(0),
        )
        .map_err(source_error)?;
    if shadowed || without_rowid {
        return Err(SqliteSourceAccessError::SnapshotUnavailable {
            reason: "selected provider tables must expose their native rowid".into(),
        }
        .into());
    }
    let bound = bound.filter(|bound| bound.tables.contains(&table));
    if let Some(bound) = bound {
        create_bounded_schema(source, target, table, bound.column)?;
    } else {
        execute_schema(target, table, &schema)?;
        let mut indexes = source
            .prepare(
                "SELECT sql FROM sqlite_schema WHERE type='index' AND tbl_name=?1 AND sql IS NOT NULL",
            )
            .map_err(source_error)?;
        let mut rows = indexes.query([table]).map_err(source_error)?;
        while let Some(row) = rows.next().map_err(source_error)? {
            let sql: String = row.get(0).map_err(source_error)?;
            execute_schema(target, table, &sql)?;
        }
    }
    let selection = bounded_selection(source, target, table, bound)
        .map_err(SqliteSourceProgressError::Source)?;
    let mut select = source
        .prepare(&format!("SELECT {selection} FROM {quoted} NOT INDEXED"))
        .map_err(source_error)?;
    let columns = select.column_count();
    let parameters = vec!["?"; columns].join(",");
    let names = select
        .column_names()
        .iter()
        .map(|name| format!("\"{}\"", name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(",");
    let mut insert = target
        .prepare(&format!(
            "INSERT INTO {quoted} ({names}) VALUES ({parameters})"
        ))
        .map_err(private_error)?;
    let mut rows = select.query([]).map_err(source_error)?;
    let mut count = 0_u64;
    while let Some(row) = rows.next().map_err(source_error)? {
        // Borrow SQLite's row cells instead of cloning the body into a second
        // Rust buffer. The statement and page caches stay bounded per row.
        for column in 0..columns {
            let value = row.get_ref(column).map_err(source_error)?;
            let size = match value {
                ValueRef::Text(value) | ValueRef::Blob(value) => value.len() as u64,
                ValueRef::Null => 0,
                _ => 8,
            };
            *bytes = bytes.checked_add(size).ok_or_else(|| {
                SqliteSourceAccessError::SnapshotUnavailable {
                    reason: "selected SQLite value byte count overflowed".into(),
                }
            })?;
            insert
                .raw_bind_parameter(column + 1, BorrowedCell(value))
                .map_err(private_error)?;
        }
        insert.raw_execute().map_err(private_error)?;
        count += 1;
        if count.is_multiple_of(4096) {
            report_progress(report, *bytes)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn report_progress<E>(
    report: &mut impl FnMut(SqliteSourceProgress) -> Result<(), E>,
    bytes: u64,
) -> Result<(), SqliteSourceProgressError<E>> {
    let mut progress = SqliteSourceProgress::new(SqliteSourceProgressStage::SourceFamilyCopy);
    progress.snapshot_bytes_completed = Some(bytes);
    // The selected logical size is not known until the stream ends.
    report(progress).map_err(SqliteSourceProgressError::Progress)
}

#[cfg(target_os = "linux")]
fn source_error(error: rusqlite::Error) -> SqliteSourceAccessError {
    sqlite_error("reading selected provider SQLite tables", error)
        .with_exact_provider_content_provenance()
}

#[cfg(target_os = "linux")]
fn private_error(error: rusqlite::Error) -> SqliteSourceAccessError {
    SqliteSourceAccessError::private_scratch_sqlite("writing selected SQLite snapshot", error)
}

#[cfg(target_os = "linux")]
struct BorrowedCell<'a>(rusqlite::types::ValueRef<'a>);
#[cfg(target_os = "linux")]
impl rusqlite::ToSql for BorrowedCell<'_> {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::Borrowed(self.0))
    }
}

#[cfg(target_os = "linux")]
pub(in crate::sqlite_source) fn execute_schema(
    target: &Connection,
    table: &str,
    sql: &str,
) -> SqliteSourceAccessResult<()> {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let table = table.to_owned();
    // Execute only the native table/index declaration, never a trigger, view,
    // attached database, PRAGMA or unrelated schema operation from input SQL.
    target
        .authorizer(Some(move |context: AuthContext<'_>| {
            let allowed = matches!(context.action, AuthAction::Function { .. })
                || context.database_name == Some("main")
                    && match context.action {
                        AuthAction::CreateTable { table_name } => table_name == table,
                        AuthAction::CreateIndex { table_name, .. } => table_name == table,
                        AuthAction::Insert { table_name }
                        | AuthAction::Update { table_name, .. } => table_name == "sqlite_master",
                        AuthAction::Read { table_name, .. } => {
                            table_name == table || table_name == "sqlite_master"
                        }
                        AuthAction::Reindex { .. } => true,
                        _ => false,
                    };
            if allowed {
                Authorization::Allow
            } else {
                Authorization::Deny
            }
        }))
        .map_err(private_error)?;
    let result = target.execute(sql, []);
    let cleared = target.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    result
        .and(cleared)
        .map_err(|error| SqliteSourceAccessError::SnapshotUnavailable {
            reason: format!("selected provider table/index schema cannot be reproduced: {error}"),
        })
}

#[cfg(target_os = "linux")]
fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(target_os = "linux")]
fn create_bounded_schema(
    source: &Connection,
    target: &Connection,
    table: &str,
    payload: &str,
) -> SqliteSourceAccessResult<()> {
    // The private projection cannot enforce constraints on omitted values.
    // Reconstruct stored columns and payload-independent keys from SQLite's
    // metadata instead of parsing or editing native CREATE TABLE statements.
    let mut columns = source
        .prepare("SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1) ORDER BY cid")
        .map_err(source_error)?;
    let mut rows = columns.query([table]).map_err(source_error)?;
    let mut declarations = Vec::new();
    let mut integer_primary = None;
    let mut primary_columns = 0;
    while let Some(row) = rows.next().map_err(source_error)? {
        let name: String = row.get(0).map_err(source_error)?;
        let declared_type: String = row.get(1).map_err(source_error)?;
        let not_null: bool = row.get(2).map_err(source_error)?;
        let primary: i64 = row.get(3).map_err(source_error)?;
        if primary > 0 {
            primary_columns += 1;
            if declared_type.eq_ignore_ascii_case("INTEGER") {
                integer_primary = Some(declarations.len());
            }
        }
        let (_, collation, _, _, _) = source
            .column_metadata(Some("main"), table, name.as_str())
            .map_err(source_error)?;
        declarations.push(format!(
            "{} {} COLLATE {} {}",
            quote_identifier(&name),
            quote_identifier(&declared_type),
            quote_identifier(&collation.unwrap_or(c"BINARY").to_string_lossy()),
            if not_null { "NOT NULL" } else { "" },
        ));
    }
    let mut indexes = source
        .prepare(
            "SELECT i.name, i.origin, s.sql,
             (SELECT min(seqno) FROM pragma_index_xinfo(i.name)
              WHERE key=1 AND (cid<0 OR name=?2 COLLATE NOCASE))
         FROM pragma_index_list(?1) i
         LEFT JOIN sqlite_schema s ON s.type='index' AND s.name=i.name
         WHERE i.partial=0",
        )
        .map_err(source_error)?;
    let mut rows = indexes.query([table, payload]).map_err(source_error)?;
    let mut secondary = Vec::new();
    // A filtered payload primary index still means the native INTEGER key
    // was not a rowid alias (notably INTEGER PRIMARY KEY DESC).
    let primary_index: bool = source
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_index_list(?1) WHERE origin='pk')",
            [table],
            |row| row.get(0),
        )
        .map_err(source_error)?;
    while let Some(row) = rows.next().map_err(source_error)? {
        let name: String = row.get(0).map_err(source_error)?;
        let origin: String = row.get(1).map_err(source_error)?;
        let omitted_key: Option<i64> = row.get(3).map_err(source_error)?;
        if origin == "c" && omitted_key.is_none() {
            secondary.push(row.get::<_, String>(2).map_err(source_error)?);
            continue;
        }
        if origin == "pk" && primary_columns == 1 && omitted_key.is_none() {
            if let Some(column) = integer_primary {
                // INTEGER PRIMARY KEY DESC is SQLite's non-rowid-alias case.
                declarations[column].push_str(" PRIMARY KEY DESC");
                continue;
            }
        }
        let mut keys = source
            .prepare(
                "SELECT name, coll, desc FROM pragma_index_xinfo(?1)
                 WHERE key=1 AND (?2 IS NULL OR seqno<?2) ORDER BY seqno",
            )
            .map_err(source_error)?;
        let keys = keys
            .query_map(rusqlite::params![name, omitted_key], |row| {
                Ok(format!(
                    "{} COLLATE {} {}",
                    quote_identifier(&row.get::<_, String>(0)?),
                    quote_identifier(&row.get::<_, String>(1)?),
                    if row.get::<_, bool>(2)? {
                        "DESC"
                    } else {
                        "ASC"
                    },
                ))
            })
            .map_err(source_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(source_error)?;
        if omitted_key.is_some() {
            // Keep a covering index's safe lookup prefix for schema admission
            // and joins. Shortening a UNIQUE index must not impose uniqueness.
            if !keys.is_empty() {
                // SQLite reserves automatic index names. Pick a private name
                // that cannot collide with any of the native schema objects.
                let mut name = name;
                if origin != "c" {
                    name = format!("__ctx_{name}");
                    while source
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1 COLLATE NOCASE)",
                            [&name],
                            |row| row.get::<_, bool>(0),
                        )
                        .map_err(source_error)?
                    {
                        name.push('_');
                    }
                }
                secondary.push(format!(
                    "CREATE INDEX {} ON {} ({})",
                    quote_identifier(&name),
                    quote_identifier(table),
                    keys.join(",")
                ));
            }
            continue;
        }
        declarations.push(format!(
            "{} ({})",
            if origin == "pk" {
                "PRIMARY KEY"
            } else {
                "UNIQUE"
            },
            keys.join(",")
        ));
    }
    if primary_columns == 1 && !primary_index {
        if let Some(column) = integer_primary {
            declarations[column].push_str(" PRIMARY KEY");
        }
    }
    let strict: bool = source
        .query_row(
            "SELECT strict FROM pragma_table_list WHERE schema='main' AND name=?1",
            [table],
            |row| row.get(0),
        )
        .map_err(source_error)?;
    execute_schema(
        target,
        table,
        &format!(
            "CREATE TABLE {} ({}) {}",
            quote_identifier(table),
            declarations.join(","),
            if strict { "STRICT" } else { "" }
        ),
    )?;
    for sql in secondary {
        execute_schema(target, table, &sql)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn bounded_selection(
    source: &Connection,
    target: &Connection,
    table: &str,
    bound: Option<SqliteSourceColumnBound>,
) -> SqliteSourceAccessResult<String> {
    let Some(bound) = bound.filter(|bound| bound.tables.contains(&table)) else {
        return Ok("rowid, *".into());
    };
    let table = quote_identifier(table);
    let columns = source
        .prepare(&format!("SELECT * FROM {table} LIMIT 0"))
        .map_err(source_error)?;
    let names = columns.column_names();
    if names
        .iter()
        .any(|name| name.eq_ignore_ascii_case(SqliteSourceColumnBound::BYTES_COLUMN))
    {
        return Err(SqliteSourceAccessError::SnapshotUnavailable {
            reason: "selected SQLite column conflicts with private byte-length metadata".into(),
        });
    }
    // Missing payload columns remain an adapter schema error.
    if !names
        .iter()
        .any(|name| name.eq_ignore_ascii_case(bound.column))
    {
        return Ok("rowid, *".into());
    }
    let bytes = quote_identifier(SqliteSourceColumnBound::BYTES_COLUMN);
    target
        .execute(
            &format!("ALTER TABLE {table} ADD COLUMN {bytes} INTEGER"),
            [],
        )
        .map_err(private_error)?;
    let mut selection = vec!["rowid".to_owned()];
    for name in names {
        let column = quote_identifier(name);
        selection.push(if name.eq_ignore_ascii_case(bound.column) {
            // octet_length reads the record header, including for TEXT with NUL
            // or multibyte characters. CASE never loads an oversized payload.
            format!(
                "CASE WHEN octet_length({column}) > {} THEN
                CASE typeof({column}) WHEN 'text' THEN '' WHEN 'blob' THEN x'' ELSE {column} END
                ELSE {column} END AS {column}",
                bound.maximum_bytes
            )
        } else {
            column
        });
    }
    let column = quote_identifier(bound.column);
    selection.push(format!("octet_length({column}) AS {bytes}"));
    Ok(selection.join(","))
}
