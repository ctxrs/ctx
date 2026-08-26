//! Kiro-owned stock read-only SQLite access and source-backed projection.

use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use rusqlite::{limits::Limit, Connection};

use crate::{
    common::io::ProviderSourceRoot,
    provider::source_backed::SourceBackedRouteError,
    provider::sqlite::{ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists},
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteFailurePhase, SqliteSourceAccessError, SqliteSourceDirectoryAuthority,
        SqliteSourceErrorComposition, SqliteSourceEvidence, SqliteSourceReadSnapshot,
    },
    CaptureError, Result,
};
use ctx_history_source_sqlite::MAX_PROVIDER_SQLITE_VALUE_BYTES;
use source_backed::{KiroSourceBackedErrorV0, KiroSourceBackedResultV0};

#[path = "native_path_scan.rs"]
mod scan;
#[path = "source_backed.rs"]
mod source_backed;

pub(crate) use source_backed::registration::source_backed_driver_scoped;

#[derive(Debug)]
struct KiroSqliteDatabase {
    root: ProviderSourceRoot,
    authority: SqliteSourceDirectoryAuthority,
    snapshot: SqliteSourceReadSnapshot,
}

impl KiroSqliteDatabase {
    fn open(data_root: &Path, path: &Path) -> KiroSourceBackedResultV0<Self> {
        let parent_path = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let database_name =
            path.file_name()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: "Kiro SQLite source must have a database leaf name",
                })?;
        let root = ProviderSourceRoot::open(parent_path)?;
        let parent = root.directory()?;
        let authority_handle = parent.try_clone_authority_handle()?;
        let authority =
            retain_sqlite_source_directory_authority(data_root, &authority_handle, parent_path)?;
        let snapshot = open_root_handle_sqlite_source_snapshot(&authority, database_name)?;
        let configure = (|| {
            snapshot.revalidate()?;
            parent.revalidate()?;
            root.revalidate()?;
            let connection = snapshot.connection()?;
            let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
                .map_err(|_| CaptureError::SystemInvariant("Kiro SQLite value limit is invalid"))?;
            connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(|source| {
                    snapshot.diagnose_provider_query_error(
                        "setting the private Kiro SQLite busy timeout",
                        source,
                        SqliteFailurePhase::SourceValidation,
                    )
                })?;
            Ok(())
        })();
        if let Err(error) = configure {
            return Err(abort_kiro_snapshot(snapshot, error));
        }
        Ok(Self {
            root,
            authority,
            snapshot,
        })
    }

    fn connection(&self, _path: &Path) -> KiroSourceBackedResultV0<&Connection> {
        Ok(self.snapshot.connection()?)
    }

    fn evidence(&self) -> &SqliteSourceEvidence {
        self.snapshot.evidence()
    }

    fn revalidate(&self, _path: &Path) -> KiroSourceBackedResultV0<()> {
        self.snapshot.revalidate()?;
        self.root.revalidate()?;
        Ok(())
    }

    fn terminal_revalidator(
        &self,
    ) -> Box<dyn Fn() -> std::result::Result<(), SqliteSourceAccessError> + Send + Sync + 'static>
    {
        self.snapshot.terminal_revalidator()
    }

    fn with_private_scratch_database<T, E>(
        &self,
        prefix: &str,
        maximum_bytes: u64,
        use_scratch: impl FnOnce(&Connection, &Path) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: SqliteSourceErrorComposition,
    {
        self.snapshot
            .with_private_scratch_database(prefix, maximum_bytes, use_scratch)
    }

    fn sqlite_authority(&self) -> SqliteSourceDirectoryAuthority {
        self.authority.clone()
    }

    fn diagnose_provider_query_error(
        &self,
        error: KiroSourceBackedErrorV0,
        phase: SqliteFailurePhase,
    ) -> KiroSourceBackedErrorV0 {
        let source = match error {
            KiroSourceBackedErrorV0::Sqlite(source)
            | KiroSourceBackedErrorV0::Capture(CaptureError::Sqlite(source)) => source,
            error => return error,
        };
        self.snapshot
            .diagnose_provider_query_error("querying the private Kiro provider copy", source, phase)
            .into()
    }

    fn abort(self, primary: SourceBackedRouteError) -> SourceBackedRouteError {
        match self.snapshot.abort() {
            Ok(()) => primary,
            Err(cleanup) => {
                crate::provider::source_backed::combine_primary_and_cleanup_route_errors(
                    primary,
                    source_backed::registration::kiro_scan_error(cleanup.into()),
                )
            }
        }
    }

    fn finish(self, _path: &Path) -> KiroSourceBackedResultV0<SqliteSourceEvidence> {
        let Self { root, snapshot, .. } = self;
        let evidence = snapshot.finish()?;
        root.revalidate()?;
        Ok(evidence)
    }
}

fn abort_kiro_snapshot(
    snapshot: SqliteSourceReadSnapshot,
    primary: KiroSourceBackedErrorV0,
) -> KiroSourceBackedErrorV0 {
    match snapshot.abort() {
        Ok(()) => primary,
        Err(cleanup) => KiroSourceBackedErrorV0::Route(
            crate::provider::source_backed::combine_primary_and_cleanup_route_errors(
                source_backed::registration::kiro_scan_error(primary),
                source_backed::registration::kiro_scan_error(cleanup.into()),
            ),
        ),
    }
}

fn absolute_kiro_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

#[derive(Debug, Clone, Copy)]
struct KiroTables {
    v2: bool,
    legacy: bool,
}

impl KiroTables {
    fn probe(connection: &Connection) -> Result<Self> {
        let v2 = sqlite_table_exists(connection, "conversations_v2")?;
        if v2 {
            ensure_kiro_table_columns(
                &sqlite_table_columns(connection, "conversations_v2")?,
                "Kiro conversations_v2",
                &[
                    "key",
                    "conversation_id",
                    "value",
                    "created_at",
                    "updated_at",
                ],
            )?;
        }
        let legacy = sqlite_table_exists(connection, "conversations")?;
        if legacy {
            ensure_kiro_table_columns(
                &sqlite_table_columns(connection, "conversations")?,
                "Kiro conversations",
                &["key", "value"],
            )?;
        }
        if !v2 && !legacy {
            return Err(CaptureError::UnsupportedSchema(
                "Kiro SQLite source has neither conversations_v2 nor conversations".to_owned(),
            ));
        }
        Ok(Self { v2, legacy })
    }
}

fn ensure_kiro_table_columns(
    columns: &BTreeSet<String>,
    label: &str,
    required: &[&str],
) -> Result<()> {
    ensure_sqlite_table_columns(columns, label, required).map_err(|cause| match cause {
        ctx_history_source_sqlite::SqliteIoError::InvalidPayload(reason) => {
            CaptureError::UnsupportedSchema(reason)
        }
        cause => cause.into(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum KiroPhase {
    V2,
    Legacy,
}

impl KiroPhase {
    fn table(self) -> &'static str {
        match self {
            Self::V2 => "conversations_v2",
            Self::Legacy => "conversations",
        }
    }

    fn tag(self) -> u8 {
        match self {
            Self::V2 => 1,
            Self::Legacy => 2,
        }
    }
}

#[cfg(test)]
mod stock_sqlite_snapshot_tests {
    use std::{ffi::OsString, fs, path::Path};

    use rusqlite::{config::DbConfig, params, Connection};

    use super::KiroSqliteDatabase;

    #[test]
    fn stock_snapshot_queries_active_wal_without_persistent_writes() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source = temp.path().join("kiro.sqlite");
        create_database(&source, "main");
        persist_wal_row(&source, "from-wal");
        let before_read = persistent_directory_snapshot(temp.path());

        let database =
            KiroSqliteDatabase::open(crate::test_provider_sqlite_data_root(), &source).unwrap();
        assert_eq!(
            read_latest(database.connection(&source).unwrap()).unwrap(),
            "from-wal"
        );
        let evidence = database.finish(&source).unwrap();
        assert!(evidence.wal_length().is_some());
        assert!(evidence.shared_memory_length().is_some());
        assert_eq!(persistent_directory_snapshot(temp.path()), before_read);
    }

    #[test]
    fn stock_snapshot_rejects_leaf_swap_before_terminal_finish() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source = temp.path().join("kiro.sqlite");
        let attacker = temp.path().join("attacker.sqlite");
        let admitted = temp.path().join("admitted.sqlite");
        create_database(&source, "main");
        create_database(&attacker, "attacker");
        let database =
            KiroSqliteDatabase::open(crate::test_provider_sqlite_data_root(), &source).unwrap();
        fs::rename(&source, &admitted).unwrap();
        fs::rename(&attacker, &source).unwrap();
        let before_rejected_read = persistent_directory_snapshot(temp.path());
        assert!(database.finish(&source).is_err());
        assert_eq!(
            persistent_directory_snapshot(temp.path()),
            before_rejected_read
        );
    }

    fn create_database(path: &Path, value: &str) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        connection
            .execute("INSERT INTO messages (body) VALUES (?1)", params![value])
            .unwrap();
    }

    fn persist_wal_row(path: &Path, value: &str) {
        let writer = Connection::open(path).unwrap();
        let mode: String = writer
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        writer
            .execute("INSERT INTO messages (body) VALUES (?1)", params![value])
            .unwrap();
        writer
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .unwrap();
        drop(writer);
        assert!(path.with_file_name("kiro.sqlite-wal").exists());
        assert!(path.with_file_name("kiro.sqlite-shm").exists());
    }

    fn read_latest(connection: &Connection) -> crate::Result<String> {
        Ok(connection.query_row(
            "SELECT body FROM messages ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )?)
    }

    fn persistent_directory_snapshot(directory: &Path) -> Vec<(OsString, Vec<u8>)> {
        let mut paths = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                !path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .ends_with("-shm")
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                (
                    path.file_name().unwrap().to_os_string(),
                    fs::read(path).unwrap(),
                )
            })
            .collect()
    }
}
