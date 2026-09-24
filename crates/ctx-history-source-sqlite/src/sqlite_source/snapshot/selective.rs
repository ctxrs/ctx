//! Table-selective private copies. Provider policy supplies the table allowlist;
//! this layer owns the consistent no-write read, disk bound, and cleanup.
use super::*;

impl SqliteSourceDirectoryAuthority {
    /// Captures the named ordinary tables and their indexes into one private
    /// database, streaming native values from one read transaction. Missing
    /// tables remain missing so the adapter retains schema-selection authority.
    /// Platforms without the no-write live reader retain full-family capture.
    pub fn open_selected_tables_snapshot_with_progress<E>(
        &self,
        database_name: &OsStr,
        tables: &'static [&'static str],
        mut report_progress: impl FnMut(SqliteSourceProgress) -> Result<(), E>,
    ) -> Result<SqliteSourceReadSnapshot, SqliteSourceProgressError<E>> {
        let policy = if selective_reader_available() {
            SqliteSourceSnapshotPolicy::SelectivePrivateCopy(tables)
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
    report: &mut impl FnMut(SqliteSourceProgress) -> Result<(), E>,
) -> Result<AcquiredSqliteConnection, SqliteSourceProgressError<E>> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (context, family, evidence, limits, tables, report);
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
            copy_tables(context, &source, limits, tables, report)
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
                copy_table(source, &target, table, &mut bytes, report)?;
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
    let mut select = source
        .prepare(&format!("SELECT rowid, * FROM {quoted} NOT INDEXED"))
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
