//! Kiro-owned stock read-only SQLite access and source-backed projection.

use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Component, Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use rusqlite::Connection;

use crate::{
    common::io::{ProviderSourceDirectory, ProviderSourceRoot},
    provider::sqlite::{ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists},
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceAccessError, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
    },
    CaptureError, Result,
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
    parent: ProviderSourceDirectory,
    authority: SqliteSourceDirectoryAuthority,
    database_name: OsString,
    evidence: SqliteSourceEvidence,
}

impl KiroSqliteDatabase {
    fn open<T>(path: &Path, query: impl FnOnce(&Connection) -> Result<T>) -> Result<(Self, T)> {
        let parent_path =
            path.parent()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: "Kiro SQLite source must have a parent directory",
                })?;
        let database_name = path
            .file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Kiro SQLite source must have a database leaf name",
            })?
            .to_os_string();
        let parent = ProviderSourceRoot::open(parent_path)?.directory()?;
        let authority_handle = parent.try_clone_authority_handle()?;
        let authority = retain_sqlite_source_directory_authority(&authority_handle, parent_path)
            .map_err(|error| kiro_sqlite_source_error(path, error))?;
        let snapshot = open_root_handle_sqlite_source_snapshot(&authority, &database_name)
            .map_err(|error| kiro_sqlite_source_error(path, error))?;
        let evidence = snapshot.evidence().clone();
        let result = snapshot
            .connection()
            .map_err(|error| kiro_sqlite_source_error(path, error))
            .and_then(query);
        let finished = snapshot
            .finish()
            .map_err(|error| kiro_sqlite_source_error(path, error))?;
        if finished != evidence {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let database = Self {
            parent,
            authority,
            database_name,
            evidence,
        };
        database.revalidate()?;
        Ok((database, result?))
    }

    fn read<T, E>(
        &self,
        path: &Path,
        query: impl FnOnce(&Connection) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<CaptureError>,
    {
        self.revalidate().map_err(E::from)?;
        let snapshot =
            open_root_handle_sqlite_source_snapshot(&self.authority, &self.database_name)
                .map_err(|error| E::from(kiro_sqlite_source_error(path, error)))?;
        let result = if snapshot.evidence() == &self.evidence {
            snapshot
                .connection()
                .map_err(|error| E::from(kiro_sqlite_source_error(path, error)))
                .and_then(query)
        } else {
            Err(E::from(CaptureError::SourceChangedDuringCapture))
        };
        let finished = snapshot
            .finish()
            .map_err(|error| E::from(kiro_sqlite_source_error(path, error)))?;
        self.revalidate().map_err(E::from)?;
        if finished != self.evidence {
            return Err(E::from(CaptureError::SourceChangedDuringCapture));
        }
        result
    }

    fn revalidate(&self) -> Result<()> {
        self.parent.revalidate()?;
        self.parent.authority_root().revalidate()
    }

    fn evidence(&self) -> &SqliteSourceEvidence {
        &self.evidence
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

    fn initial_phase(self) -> KiroPhase {
        if self.v2 {
            KiroPhase::V2
        } else {
            KiroPhase::Legacy
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

fn hex(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod stock_sqlite_snapshot_tests {
    use std::{cell::Cell, ffi::OsString, fs, path::Path};

    use rusqlite::{config::DbConfig, params, Connection};

    use super::KiroSqliteDatabase;

    #[test]
    fn stock_snapshot_queries_active_wal_without_persistent_writes_and_rejects_swap() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source = temp.path().join("kiro.sqlite");
        let attacker = temp.path().join("attacker.sqlite");
        let admitted = temp.path().join("admitted.sqlite");
        create_database(&source, "main");
        create_database(&attacker, "attacker");
        persist_wal_row(&source, "from-wal");
        let before_read = persistent_directory_snapshot(temp.path());

        let (database, opened_value) = KiroSqliteDatabase::open(&source, read_latest).unwrap();
        assert_eq!(opened_value, "from-wal");
        assert!(database.evidence().wal_length().is_some());
        assert!(database.evidence().shared_memory_length().is_some());
        assert_eq!(
            database
                .read::<_, crate::CaptureError>(&source, read_latest)
                .unwrap(),
            "from-wal"
        );
        assert_eq!(persistent_directory_snapshot(temp.path()), before_read);

        fs::rename(&source, &admitted).unwrap();
        fs::rename(&attacker, &source).unwrap();
        let before_rejected_read = persistent_directory_snapshot(temp.path());
        let queried = Cell::new(false);
        let result = database.read::<_, crate::CaptureError>(&source, |_| -> crate::Result<()> {
            queried.set(true);
            Ok(())
        });
        assert!(result.is_err());
        assert!(!queried.get());
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
