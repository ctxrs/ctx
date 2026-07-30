//! Kiro-owned stock read-only SQLite access and source-backed projection.

use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use ctx_history_core::CaptureProvider;
use rusqlite::{limits::Limit, Connection};

use crate::{
    common::io::ProviderSourceRoot,
    provider::sqlite::{ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists},
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceAccessError, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
        SqliteSourceReadSnapshot,
    },
    CaptureError, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

#[path = "native_path_scan.rs"]
mod scan;
#[path = "source_backed.rs"]
mod source_backed;

pub(crate) use source_backed::registration::register as register_source_backed_route;
#[cfg(test)]
pub(crate) use source_backed::{
    scan_kiro_source_backed_v0, KiroLocatorResolverV0, KiroSourceBackedErrorV0,
};

#[derive(Debug)]
struct KiroSqliteDatabase {
    root: ProviderSourceRoot,
    authority: SqliteSourceDirectoryAuthority,
    snapshot: SqliteSourceReadSnapshot,
}

impl KiroSqliteDatabase {
    fn open(data_root: &Path, path: &Path) -> Result<Self> {
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
            retain_sqlite_source_directory_authority(data_root, &authority_handle, parent_path)
                .map_err(|error| kiro_sqlite_source_error(path, error))?;
        let snapshot = open_root_handle_sqlite_source_snapshot(&authority, database_name)
            .map_err(|error| kiro_sqlite_source_error(path, error))?;
        snapshot
            .revalidate()
            .map_err(|error| kiro_sqlite_source_error(path, error))?;
        parent.revalidate()?;
        root.revalidate()?;
        let connection = snapshot
            .connection()
            .map_err(|error| kiro_sqlite_source_error(path, error))?;
        let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
            .map_err(|_| CaptureError::SystemInvariant("Kiro SQLite value limit is invalid"))?;
        connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(Self {
            root,
            authority,
            snapshot,
        })
    }

    fn connection(&self, path: &Path) -> Result<&Connection> {
        self.snapshot
            .connection()
            .map_err(|error| kiro_sqlite_source_error(path, error))
    }

    fn evidence(&self) -> &SqliteSourceEvidence {
        self.snapshot.evidence()
    }

    fn revalidate(&self, path: &Path) -> Result<()> {
        self.snapshot
            .revalidate()
            .map_err(|error| kiro_sqlite_source_error(path, error))?;
        self.root.revalidate()
    }

    fn terminal_revalidator(
        &self,
    ) -> Box<dyn Fn() -> std::result::Result<(), SqliteSourceAccessError> + Send + Sync + 'static>
    {
        self.snapshot.terminal_revalidator()
    }

    fn sqlite_authority(&self) -> SqliteSourceDirectoryAuthority {
        self.authority.clone()
    }

    fn finish(self, path: &Path) -> Result<SqliteSourceEvidence> {
        let Self { root, snapshot, .. } = self;
        let evidence = snapshot
            .finish()
            .map_err(|error| kiro_sqlite_source_error(path, error))?;
        root.revalidate()?;
        Ok(evidence)
    }
}

fn kiro_sqlite_source_error(path: &Path, error: SqliteSourceAccessError) -> CaptureError {
    match error {
        SqliteSourceAccessError::SourceChanged
        | SqliteSourceAccessError::ConnectionIdentityMismatch => {
            CaptureError::SourceChangedDuringCapture
        }
        error => CaptureError::ProviderSource {
            provider: CaptureProvider::KiroCli.as_str(),
            path: path.to_path_buf(),
            kind: crate::ProviderSourceFailureKind::SourceDatabase,
            detail: error.to_string(),
        },
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
        CaptureError::InvalidPayload(reason) => CaptureError::UnsupportedSchema(reason),
        cause => cause,
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
