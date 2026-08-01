//! Source-backed Zed discovery, bounded snapshot parsing, and Core projection.

use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use rusqlite::Connection;
use thiserror::Error;

use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceDirectory, ProviderSourceRoot},
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceAccessError, SqliteSourceReadSnapshot,
    },
    CaptureError,
};

mod decode;
mod dto;
mod model;
mod query;
pub(crate) mod source_backed;

const ZED_SNAPSHOT_ACQUISITION_ATTEMPTS: usize = 3;
const ZED_SOURCE_INVALID_REASON: &str = "Zed SQLite source must be a regular non-symlink file";
const ZED_SIDECAR_INVALID_REASON: &str = "Zed SQLite sidecar must be a regular non-symlink file";

#[derive(Debug, Error)]
pub(crate) enum ZedNativePathError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("I/O error while preparing Zed NativePath source: {0}")]
    Io(#[from] io::Error),
    #[error("SQLite error while preparing Zed NativePath source: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    SqliteSourceAccess(#[from] SqliteSourceAccessError),
    #[error("Zed NativePath source has an unsupported schema: {0}")]
    UnsupportedSchema(String),
}

pub(super) type ZedNativeResult<T> = std::result::Result<T, ZedNativePathError>;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ZedSourceBackedWork {
    pub(crate) logical_observation_passes: u64,
    pub(crate) projection_passes: u64,
    pub(crate) projected_core_records: u64,
}

#[cfg(test)]
std::thread_local! {
    static ZED_SOURCE_BACKED_WORK: std::cell::RefCell<ZedSourceBackedWork> =
        std::cell::RefCell::new(ZedSourceBackedWork::default());
}

#[cfg(test)]
pub(crate) fn reset_source_backed_work() {
    ZED_SOURCE_BACKED_WORK.with(|work| *work.borrow_mut() = ZedSourceBackedWork::default());
}

#[cfg(test)]
pub(crate) fn source_backed_work() -> ZedSourceBackedWork {
    ZED_SOURCE_BACKED_WORK.with(|work| *work.borrow())
}

#[cfg(test)]
fn record_zed_work(update: impl FnOnce(&mut ZedSourceBackedWork)) {
    ZED_SOURCE_BACKED_WORK.with(|work| update(&mut work.borrow_mut()));
}

#[cfg(test)]
fn record_zed_logical_observation() {
    record_zed_work(|work| {
        work.logical_observation_passes = work.logical_observation_passes.saturating_add(1);
    });
}

#[cfg(test)]
fn record_zed_projection_pass() {
    record_zed_work(|work| {
        work.projection_passes = work.projection_passes.saturating_add(1);
    });
}

#[cfg(test)]
fn record_zed_projected_core_record() {
    record_zed_work(|work| {
        work.projected_core_records = work.projected_core_records.saturating_add(1);
    });
}

pub(crate) struct ZedImmutableSqliteSnapshot {
    observed: Arc<ZedAdmittedSqliteFamily>,
    connection: Option<SqliteSourceReadSnapshot>,
    pub(crate) snapshot_revision: String,
}

pub(crate) enum ZedSnapshotAcquisition {
    Acquired(Box<ZedImmutableSqliteSnapshot>),
    Incomplete,
}

impl ZedImmutableSqliteSnapshot {
    pub(crate) fn connection(&self) -> ZedNativeResult<&Connection> {
        self.connection
            .as_ref()
            .ok_or_else(|| {
                ZedNativePathError::Capture(CaptureError::SystemInvariant(
                    "Zed SQLite snapshot was queried after finish",
                ))
            })?
            .connection()
            .map_err(Into::into)
    }

    pub(crate) fn terminal_revalidator(
        &self,
    ) -> ZedNativeResult<Box<dyn Fn() -> ZedNativeResult<()> + Send + Sync + 'static>> {
        let revalidate_snapshot = self
            .connection
            .as_ref()
            .ok_or_else(|| {
                ZedNativePathError::Capture(CaptureError::SystemInvariant(
                    "Zed SQLite snapshot was queried after finish",
                ))
            })?
            .terminal_revalidator();
        let observed = Arc::clone(&self.observed);
        Ok(Box::new(move || {
            revalidate_snapshot().map_err(ZedNativePathError::from)?;
            observed.revalidate()
        }))
    }

    /// Ends SQLite's pinned transaction and then certifies every retained
    /// DB-family handle and the named parent-directory route.
    pub(crate) fn finish(&mut self) -> ZedNativeResult<()> {
        let snapshot = self.connection.take().ok_or_else(|| {
            ZedNativePathError::Capture(CaptureError::SystemInvariant(
                "Zed SQLite snapshot was finished more than once",
            ))
        })?;
        match snapshot.finish() {
            Ok(_) => {}
            Err(
                SqliteSourceAccessError::SourceChanged
                | SqliteSourceAccessError::ConnectionIdentityMismatch,
            ) => return Err(CaptureError::SourceChangedDuringCapture.into()),
            Err(_) if !self.observed.revalidate_bool()? => {
                return Err(CaptureError::SourceChangedDuringCapture.into());
            }
            Err(error) => return Err(error.into()),
        }
        self.observed.revalidate()
    }
}

#[derive(Debug)]
struct ZedAdmittedSqliteFamily {
    data_root: PathBuf,
    root: ProviderSourceRoot,
    directory: ProviderSourceDirectory,
    database_name: OsString,
    database: ZedAdmittedSqliteComponent,
    wal: Option<ZedAdmittedSqliteComponent>,
    shared_memory: Option<ZedAdmittedSqliteComponent>,
    rollback_journal: Option<ZedAdmittedSqliteComponent>,
}

#[derive(Debug)]
struct ZedAdmittedSqliteComponent {
    file: OpenedProviderSourceFile,
}

impl ZedAdmittedSqliteFamily {
    fn open(data_root: &Path, path: &Path) -> ZedNativeResult<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Zed SQLite path has no authority parent",
            })?;
        let filename =
            path.file_name()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: "Zed SQLite path has no file name",
                })?;
        let root = ProviderSourceRoot::open(parent)?;
        let directory = root.directory()?;
        let database = ZedAdmittedSqliteComponent::open(
            &root,
            Path::new(filename),
            ZED_SOURCE_INVALID_REASON,
        )?;
        let wal = ZedAdmittedSqliteComponent::open_optional(
            &root,
            &zed_sidecar_relative_path(filename, "-wal"),
            ZED_SIDECAR_INVALID_REASON,
        )?;
        let shared_memory = ZedAdmittedSqliteComponent::open_optional(
            &root,
            &zed_sidecar_relative_path(filename, "-shm"),
            ZED_SIDECAR_INVALID_REASON,
        )?;
        let rollback_journal = ZedAdmittedSqliteComponent::open_optional(
            &root,
            &zed_sidecar_relative_path(filename, "-journal"),
            ZED_SIDECAR_INVALID_REASON,
        )?;
        let family = Self {
            data_root: data_root.to_path_buf(),
            root,
            directory,
            database_name: filename.to_os_string(),
            database,
            wal,
            shared_memory,
            rollback_journal,
        };
        if !family.revalidate_bool()? {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        Ok(family)
    }

    fn connection(&self) -> ZedNativeResult<SqliteSourceReadSnapshot> {
        let directory = self.directory.try_clone_authority_handle()?;
        let authority = retain_sqlite_source_directory_authority(
            &self.data_root,
            &directory,
            self.root.named_path(),
        )?;
        match open_root_handle_sqlite_source_snapshot(&authority, &self.database_name) {
            Ok(snapshot) => Ok(snapshot),
            Err(
                SqliteSourceAccessError::SourceChanged
                | SqliteSourceAccessError::ConnectionIdentityMismatch,
            ) => Err(CaptureError::SourceChangedDuringCapture.into()),
            Err(_) if !self.revalidate_bool()? => {
                Err(CaptureError::SourceChangedDuringCapture.into())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn revalidate_bool(&self) -> ZedNativeResult<bool> {
        let result = (|| -> crate::Result<()> {
            self.database.file.revalidate()?;
            for component in self
                .wal
                .iter()
                .chain(self.shared_memory.iter())
                .chain(self.rollback_journal.iter())
            {
                component.file.revalidate()?;
            }
            self.directory.revalidate()?;
            self.root.revalidate()
        })();
        match result {
            Ok(()) => Ok(true),
            Err(CaptureError::InvalidProviderTranscriptPath { .. })
            | Err(CaptureError::SourceChangedDuringCapture) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn revalidate(&self) -> ZedNativeResult<()> {
        if self.revalidate_bool()? {
            Ok(())
        } else {
            Err(CaptureError::SourceChangedDuringCapture.into())
        }
    }
}

impl ZedAdmittedSqliteComponent {
    fn open(
        root: &ProviderSourceRoot,
        relative_path: &Path,
        invalid_reason: &'static str,
    ) -> ZedNativeResult<Self> {
        let file = root.open_file(relative_path).map_err(|error| match error {
            CaptureError::InvalidProviderTranscriptPath { path, .. } => {
                CaptureError::InvalidProviderTranscriptPath {
                    path,
                    reason: invalid_reason,
                }
            }
            error => error,
        })?;
        Ok(Self { file })
    }

    fn open_optional(
        root: &ProviderSourceRoot,
        relative_path: &Path,
        invalid_reason: &'static str,
    ) -> ZedNativeResult<Option<Self>> {
        match Self::open(root, relative_path, invalid_reason) {
            Ok(component) => Ok(Some(component)),
            Err(ZedNativePathError::Capture(CaptureError::Io(error)))
                if error.kind() == io::ErrorKind::NotFound =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
}

fn zed_sidecar_relative_path(filename: &std::ffi::OsStr, suffix: &str) -> PathBuf {
    let mut sidecar = filename.to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

fn zed_absolute_authority_path(path: &Path) -> ZedNativeResult<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub(super) fn acquire_immutable_snapshot(
    data_root: &Path,
    path: &Path,
) -> ZedNativeResult<ZedSnapshotAcquisition> {
    let authority_path = zed_absolute_authority_path(path)?;
    for _ in 0..ZED_SNAPSHOT_ACQUISITION_ATTEMPTS {
        let observed = match ZedAdmittedSqliteFamily::open(data_root, &authority_path) {
            Ok(observed) => observed,
            Err(ZedNativePathError::Capture(CaptureError::SourceChangedDuringCapture)) => continue,
            Err(error) => return Err(error),
        };
        let connection = match observed.connection() {
            Ok(connection) => connection,
            Err(ZedNativePathError::Capture(CaptureError::SourceChangedDuringCapture)) => continue,
            Err(error) => return Err(error),
        };
        if !observed.revalidate_bool()? {
            continue;
        }
        let snapshot_revision = query::observe_zed_logical_snapshot(
            connection
                .connection()
                .map_err(ZedNativePathError::SqliteSourceAccess)?,
        )?;
        match connection.revalidate() {
            Ok(()) => {}
            Err(
                SqliteSourceAccessError::SourceChanged
                | SqliteSourceAccessError::ConnectionIdentityMismatch,
            ) => continue,
            Err(_) if !observed.revalidate_bool()? => continue,
            Err(error) => return Err(error.into()),
        }
        if !observed.revalidate_bool()? {
            continue;
        }
        return Ok(ZedSnapshotAcquisition::Acquired(Box::new(
            ZedImmutableSqliteSnapshot {
                observed: Arc::new(observed),
                connection: Some(connection),
                snapshot_revision,
            },
        )));
    }
    Ok(ZedSnapshotAcquisition::Incomplete)
}
