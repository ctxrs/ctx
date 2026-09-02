//! Stock SQLite snapshots for root-authorized provider databases.
//!
//! The ordinary provider-source layer approves and retains the database parent
//! directory. This module keeps that [`ProviderSourceDirectory`] capability,
//! opens every DB/WAL/SHM/journal leaf relative to it, rejects symlink,
//! reparse-point, cross-filesystem, and non-regular members, and never asks
//! SQLite to create or update files in the provider directory.
//!
//! The exact-policy path opens a sidecar-free database through SQLite's
//! immutable URI mode when the platform supports it. Every other route copies
//! one exact DB/WAL family, with bounded I/O, to one private directory below the
//! ctx data root. Family-member replacement or appearance remains fail-closed.
//! Rollback journals remain typed unavailable because recovery could require
//! database writes. SHM is bounded volatile lock coordination; provider
//! DB/WAL/SHM bytes and directory entries are never mutated.

use std::{
    ffi::{c_char, c_void, OsStr, OsString},
    fs::{File, Metadata, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    ptr,
    sync::{Arc, Mutex, MutexGuard},
};

use ctx_history_platform::platform_security::create_private_directory_all;
use rusqlite::{config::DbConfig, ffi, Connection, OpenFlags};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
#[cfg(target_os = "linux")]
use url::Url;

#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;

use ctx_history_source_io::{
    OpenedProviderSourceFile, OpenedProviderSourcePath, ProviderSourceDirectory,
    ProviderSourceRoot, SourceIoError,
};

use crate::{SqliteSourceProgress, SqliteSourceProgressStage};

const EVIDENCE_DOMAIN: &[u8] = b"ctx-stock-sqlite-snapshot-v2\0";
const SQLITE_SNAPSHOT_FREE_HEADROOM_BYTES: u64 = 16 * 1024 * 1024;
const SQLITE_SNAPSHOT_FREE_HEADROOM_DIVISOR: u64 = 20;
const SQLITE_COPY_BUFFER_BYTES: usize = 64 * 1024;
const SQLITE_REVISION_TOKEN_BYTES: usize = 64;
const SQLITE_SHM_MAX_BYTES: u64 = 8 * 1024 * 1024;

fn sqlite_snapshot_free_headroom_bytes(capacity_bytes: u64) -> u64 {
    SQLITE_SNAPSHOT_FREE_HEADROOM_BYTES.max(capacity_bytes / SQLITE_SNAPSHOT_FREE_HEADROOM_DIVISOR)
}

mod diagnostics;
mod resources;
pub use diagnostics::{
    resource_exhaustion_io_error, rusqlite_busy_or_locked, rusqlite_resource_failure,
    sqlite_retry_decision, SqliteArtifactKind, SqliteCleanupStatus, SqliteFailurePhase,
    SqliteRetryDecision, SqliteSourceAccessError, SqliteSourceComponent,
    SqliteSourceErrorComposition, SqliteSourceProgressError,
};
pub use resources::SqliteSourceSnapshotCounters;
#[cfg(any(test, feature = "test-support"))]
pub use resources::{
    override_next_scratch_available_space_for_test, SqliteSourceSnapshotCounterObserver,
};
use resources::{SqliteRouteScratch, SqliteSourceSnapshotActivity, SqliteSourceSnapshotContext};

pub type SqliteSourceAccessResult<T> = Result<T, SqliteSourceAccessError>;

/// A remove-on-close staging file created under the caller's private data root.
///
/// SQLite source acquisition owns the temporary-file implementation so provider
/// packs retain only a lower-layer scratch capability rather than a direct
/// production dependency on `tempfile`.
#[derive(Debug)]
pub struct SqliteSourceStagingFile {
    file: File,
    data_root: PathBuf,
}

pub struct SqliteSourceStagingReader<'file> {
    reader: BufReader<&'file mut File>,
    data_root: PathBuf,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteSourceStagingOperationForTest {
    Open,
    Write,
    Flush,
    Rewind,
    Read,
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static FAIL_NEXT_PRIVATE_STAGING_OPERATION: std::cell::Cell<Option<(SqliteSourceStagingOperationForTest, std::io::ErrorKind)>> = const { std::cell::Cell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
pub fn fail_next_private_sqlite_staging_operation_for_test(
    operation: SqliteSourceStagingOperationForTest,
    kind: std::io::ErrorKind,
) {
    FAIL_NEXT_PRIVATE_STAGING_OPERATION.with(|failure| failure.set(Some((operation, kind))));
}

#[cfg(any(test, feature = "test-support"))]
fn take_private_sqlite_staging_failure_for_test(
    expected: SqliteSourceStagingOperationForTest,
) -> Option<std::io::ErrorKind> {
    FAIL_NEXT_PRIVATE_STAGING_OPERATION.with(|failure| match failure.get() {
        Some((operation, kind)) if operation == expected => {
            failure.set(None);
            Some(kind)
        }
        _ => None,
    })
}

fn private_sqlite_staging_io_error(
    operation: &'static str,
    data_root: &Path,
    source: std::io::Error,
) -> SqliteSourceAccessError {
    SqliteSourceAccessError::ScratchIoUnavailable {
        operation,
        path: data_root.to_path_buf(),
        source,
    }
}

#[cfg(any(test, feature = "test-support"))]
fn injected_private_sqlite_staging_error(
    expected: SqliteSourceStagingOperationForTest,
    operation: &'static str,
    data_root: &Path,
) -> Option<SqliteSourceAccessError> {
    take_private_sqlite_staging_failure_for_test(expected).map(|kind| {
        private_sqlite_staging_io_error(operation, data_root, std::io::Error::from(kind))
    })
}

impl SqliteSourceStagingFile {
    pub fn write_all(&mut self, buffer: &[u8]) -> SqliteSourceAccessResult<()> {
        const OPERATION: &str = "writing a private provider SQLite staging file";
        #[cfg(any(test, feature = "test-support"))]
        if let Some(error) = injected_private_sqlite_staging_error(
            SqliteSourceStagingOperationForTest::Write,
            OPERATION,
            &self.data_root,
        ) {
            return Err(error);
        }
        self.file
            .write_all(buffer)
            .map_err(|source| private_sqlite_staging_io_error(OPERATION, &self.data_root, source))
    }

    pub fn flush(&mut self) -> SqliteSourceAccessResult<()> {
        const OPERATION: &str = "flushing a private provider SQLite staging file";
        #[cfg(any(test, feature = "test-support"))]
        if let Some(error) = injected_private_sqlite_staging_error(
            SqliteSourceStagingOperationForTest::Flush,
            OPERATION,
            &self.data_root,
        ) {
            return Err(error);
        }
        self.file
            .flush()
            .map_err(|source| private_sqlite_staging_io_error(OPERATION, &self.data_root, source))
    }

    pub fn rewind(&mut self) -> SqliteSourceAccessResult<()> {
        const OPERATION: &str = "rewinding a private provider SQLite staging file";
        #[cfg(any(test, feature = "test-support"))]
        if let Some(error) = injected_private_sqlite_staging_error(
            SqliteSourceStagingOperationForTest::Rewind,
            OPERATION,
            &self.data_root,
        ) {
            return Err(error);
        }
        self.file
            .seek(SeekFrom::Start(0))
            .map(|_| ())
            .map_err(|source| private_sqlite_staging_io_error(OPERATION, &self.data_root, source))
    }

    pub fn reader(&mut self) -> SqliteSourceStagingReader<'_> {
        SqliteSourceStagingReader {
            reader: BufReader::new(&mut self.file),
            data_root: self.data_root.clone(),
        }
    }
}

impl SqliteSourceStagingReader<'_> {
    pub fn read_line(&mut self, line: &mut String) -> SqliteSourceAccessResult<usize> {
        const OPERATION: &str = "reading a private provider SQLite staging file";
        #[cfg(any(test, feature = "test-support"))]
        if let Some(error) = injected_private_sqlite_staging_error(
            SqliteSourceStagingOperationForTest::Read,
            OPERATION,
            &self.data_root,
        ) {
            return Err(error);
        }
        self.reader
            .read_line(line)
            .map_err(|source| private_sqlite_staging_io_error(OPERATION, &self.data_root, source))
    }
}

pub fn open_private_sqlite_staging_file(
    data_root: &Path,
) -> SqliteSourceAccessResult<SqliteSourceStagingFile> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(error) = injected_private_sqlite_staging_error(
        SqliteSourceStagingOperationForTest::Open,
        "creating a private provider SQLite staging file",
        data_root,
    ) {
        return Err(error);
    }
    tempfile::tempfile_in(data_root)
        .map(|file| SqliteSourceStagingFile {
            file,
            data_root: data_root.to_path_buf(),
        })
        .map_err(|source| {
            private_sqlite_staging_io_error(
                "creating a private provider SQLite staging file",
                data_root,
                source,
            )
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteSourceSnapshotStrategy {
    #[cfg(target_os = "linux")]
    ImmutableMain,
    #[cfg(target_os = "linux")]
    PinnedReadOnlyWal,
    CopiedFamily,
}

/// Selects how one authorized provider SQLite leaf is stabilized.
///
/// Both policies acquire the same physical files. The stable-copy policy keeps
/// its private copy readable while the source's retained database identity is
/// still present; interpretation and publication policy remain with capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SqliteSourceSnapshotPolicy {
    ExactRevision,
    PinnedReadOnlyWal,
    StablePrivateCopy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteSourceEvidence {
    identity: [u8; 32],
    length: u64,
    wal_length: Option<u64>,
    shared_memory_length: Option<u64>,
    physical_revision: [u8; 32],
    schema: SqliteSchemaEvidence,
    source: SqliteConnectionEvidence,
    revision: [u8; 32],
}

impl SqliteSourceEvidence {
    pub fn identity(&self) -> &[u8; 32] {
        &self.identity
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn length(&self) -> u64 {
        self.length
    }

    pub fn revision(&self) -> &[u8; 32] {
        &self.revision
    }

    /// Returns the move-stable bounded DB/WAL content revision that can be
    /// reobserved without copying or opening a logical SQLite snapshot.
    ///
    /// This token is suitable only for exact replay. Callers must reobserve it
    /// through [`SqliteSourceReplayFence::revalidate`] at terminal validation
    /// before publishing retained logical content.
    pub fn physical_revision(&self) -> &[u8; 32] {
        &self.physical_revision
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn wal_length(&self) -> Option<u64> {
        self.wal_length
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn shared_memory_length(&self) -> Option<u64> {
        self.shared_memory_length
    }
}

/// Retained authority for one approved SQLite parent directory.
///
/// `path` is retained only to certify the parent route and describe errors.
/// SQLite family members are always opened relative to `directory`.
#[derive(Debug, Clone)]
pub struct SqliteSourceDirectoryAuthority {
    directory: Arc<ProviderSourceDirectory>,
    path: PathBuf,
    identity: NativeFileIdentity,
    snapshot_context: Arc<SqliteSourceSnapshotContext>,
}

impl SqliteSourceDirectoryAuthority {
    fn retain(
        data_root: &Path,
        authorized_parent: &File,
        approved_path: &Path,
    ) -> SqliteSourceAccessResult<Self> {
        validate_approved_parent_path(approved_path)?;
        let retained = NativeFileState::read(
            authorized_parent,
            approved_path,
            ExpectedObjectKind::Directory,
        )?;
        let root = ProviderSourceRoot::open(approved_path).map_err(|error| {
            map_provider_source_error(
                error,
                "opening the approved SQLite parent capability",
                approved_path,
            )
        })?;
        let directory = root.directory().map_err(|error| {
            map_provider_source_error(
                error,
                "retaining the approved SQLite parent capability",
                approved_path,
            )
        })?;
        let named = directory.try_clone_authority_handle().map_err(|source| {
            SqliteSourceAccessError::Io {
                operation: "retaining the approved SQLite parent capability handle",
                path: approved_path.to_path_buf(),
                source,
            }
        })?;
        let named_state =
            NativeFileState::read(&named, approved_path, ExpectedObjectKind::Directory)?;
        if retained.identity != named_state.identity {
            return Err(SqliteSourceAccessError::ConnectionIdentityMismatch);
        }
        Ok(Self {
            directory: Arc::new(directory),
            path: approved_path.to_path_buf(),
            identity: retained.identity,
            snapshot_context: Arc::new(SqliteSourceSnapshotContext {
                data_root: data_root.to_path_buf(),
                counters: Mutex::new(SqliteSourceSnapshotCounters::default()),
            }),
        })
    }

    pub fn snapshot_counters(&self) -> SqliteSourceSnapshotCounters {
        self.snapshot_context.snapshot()
    }

    /// Retains one exact physical DB/WAL revision for no-copy replay.
    ///
    /// Provider policy must first establish that its prior logical receipt or
    /// frontier is eligible for replay. The returned fence owns physical
    /// authority only and must be revalidated before retained logical content
    /// is published.
    pub fn observe_replay_fence(
        &self,
        database_name: &OsStr,
    ) -> SqliteSourceAccessResult<SqliteSourceReplayFence> {
        let family = SqliteSourceFamily::open(self, database_name, || {})?;
        let evidence = family.capture_revision_evidence()?;
        family.revalidate_revision(&evidence)?;
        let revision = evidence.content_revision_token();
        Ok(SqliteSourceReplayFence {
            authority: self.clone(),
            database_name: database_name.to_os_string(),
            revision,
            evidence,
        })
    }

    /// Observes one bounded physical DB/WAL family revision without retaining
    /// a fence. This lower-level API exists for bounded inventory routes that
    /// cannot retain one directory authority per leaf; single-database replay
    /// routes should prefer [`Self::observe_replay_fence`].
    pub fn observe_physical_revision(
        &self,
        database_name: &OsStr,
    ) -> SqliteSourceAccessResult<[u8; 32]> {
        let family = SqliteSourceFamily::open(self, database_name, || {})?;
        let evidence = family.capture_revision_evidence()?;
        family.revalidate_revision(&evidence)?;
        Ok(evidence.revision_token())
    }

    /// Acquires one private, exact copy of the currently authorized DB/WAL
    /// family. Same-object source writes after acquisition do not alter the
    /// copy, but replacing the retained database fails terminal revalidation.
    pub fn open_stable_snapshot(
        &self,
        database_name: &OsStr,
    ) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
        snapshot::open_root_handle_sqlite_source_snapshot_with_policy(
            self,
            database_name,
            SqliteSourceSnapshotPolicy::StablePrivateCopy,
            SqliteSourceSnapshotLimits::default(),
        )
    }

    pub fn open_stable_snapshot_with_progress<E>(
        &self,
        database_name: &OsStr,
        mut report_progress: impl FnMut(SqliteSourceProgress) -> Result<(), E>,
    ) -> Result<SqliteSourceReadSnapshot, SqliteSourceProgressError<E>> {
        snapshot::open_root_handle_sqlite_source_snapshot_with_progress(
            self,
            database_name,
            SqliteSourceSnapshotPolicy::StablePrivateCopy,
            SqliteSourceSnapshotLimits::default(),
            &mut report_progress,
        )
    }

    /// Opens one named provider DB/WAL view through SQLite's read-only SHM URI
    /// mode. The pinned transaction is coherent while WAL growth remains
    /// available to a successor refresh and provider bytes stay untouched.
    pub fn open_incremental_snapshot_with_progress<E>(
        &self,
        database_name: &OsStr,
        mut report_progress: impl FnMut(SqliteSourceProgress) -> Result<(), E>,
    ) -> Result<SqliteSourceReadSnapshot, SqliteSourceProgressError<E>> {
        snapshot::open_root_handle_sqlite_source_snapshot_with_progress(
            self,
            database_name,
            SqliteSourceSnapshotPolicy::PinnedReadOnlyWal,
            SqliteSourceSnapshotLimits::default(),
            &mut report_progress,
        )
    }

    pub fn revalidate(&self) -> SqliteSourceAccessResult<()> {
        let retained = self
            .directory
            .try_clone_authority_handle()
            .map_err(|source| {
                map_revalidation_io_error(
                    source,
                    "retaining the approved SQLite parent capability during revalidation",
                    &self.path,
                )
            })
            .and_then(|directory| {
                NativeFileState::read(&directory, &self.path, ExpectedObjectKind::Directory)
                    .map_err(map_revalidation_error)
            })?;
        if retained.identity != self.identity {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        let named_root = ProviderSourceRoot::open(&self.path).map_err(|error| {
            map_provider_source_revalidation_error(
                error,
                "reopening the approved SQLite parent capability during revalidation",
                &self.path,
            )
        })?;
        let named_directory = named_root.directory().map_err(|error| {
            map_provider_source_revalidation_error(
                error,
                "retaining the reopened SQLite parent capability during revalidation",
                &self.path,
            )
        })?;
        let named = named_directory
            .try_clone_authority_handle()
            .map_err(|source| {
                map_revalidation_io_error(
                    source,
                    "retaining the reopened SQLite parent capability handle during revalidation",
                    &self.path,
                )
            })?;
        let named_state = NativeFileState::read(&named, &self.path, ExpectedObjectKind::Directory)
            .map_err(map_revalidation_error)?;
        if named_state.identity == self.identity {
            Ok(())
        } else {
            Err(SqliteSourceAccessError::SourceChanged)
        }
    }
}

/// Retained physical authority for exact replay of one unchanged SQLite
/// DB/WAL family.
///
/// This fence proves only that the provider-owned physical source is still the
/// revision observed at construction. Parser, logical-source, and publication
/// policy remain the responsibility of the provider adapter.
#[derive(Debug)]
#[must_use = "exact replay requires terminal revalidation before publication"]
pub struct SqliteSourceReplayFence {
    authority: SqliteSourceDirectoryAuthority,
    database_name: OsString,
    revision: [u8; 32],
    evidence: SqliteFamilyEvidence,
}

impl SqliteSourceReplayFence {
    pub fn revision(&self) -> &[u8; 32] {
        &self.revision
    }

    /// Reobserves the retained DB/WAL family and fails closed unless its exact
    /// native objects, metadata, and bounded content evidence still match the
    /// admitted replay family.
    pub fn revalidate(&self) -> SqliteSourceAccessResult<()> {
        let family = SqliteSourceFamily::open(&self.authority, &self.database_name, || {})
            .map_err(map_revalidation_error)?;
        family
            .revalidate_revision(&self.evidence)
            .map_err(map_revalidation_error)
    }
}

/// A sealed compact witness for the exact SQLite family that backed one
/// completed read snapshot.
///
/// The witness retains no provider handles. Commit-time validation reopens the
/// approved parent through the same no-follow capability path, certifies the
/// main database, any admitted WAL, and relevant SHM identity. This bounds live
/// descriptors by active workers rather than total discovered databases.
#[must_use = "revalidate the terminal fence before publishing snapshot observations"]
#[derive(Debug)]
struct SqliteSourceTerminalFenceInner {
    data_root: PathBuf,
    approved_parent_path: PathBuf,
    database_name: OsString,
    native_evidence: SqliteFamilyEvidence,
    evidence: SqliteSourceEvidence,
    policy: SqliteSourceSnapshotPolicy,
    snapshot_context: Arc<SqliteSourceSnapshotContext>,
}

#[derive(Clone, Debug)]
pub struct SqliteSourceTerminalFence {
    inner: Arc<SqliteSourceTerminalFenceInner>,
}

impl SqliteSourceTerminalFence {
    pub fn evidence(&self) -> &SqliteSourceEvidence {
        &self.inner.evidence
    }

    /// Revalidates the exact retained source family without opening SQLite or
    /// acquiring another source snapshot.
    pub fn revalidate(&self) -> SqliteSourceAccessResult<()> {
        let root = ProviderSourceRoot::open(&self.inner.approved_parent_path).map_err(|error| {
            map_provider_source_revalidation_error(
                error,
                "reopening the approved SQLite parent for terminal revalidation",
                &self.inner.approved_parent_path,
            )
        })?;
        let directory = root.directory().map_err(|error| {
            map_provider_source_revalidation_error(
                error,
                "retaining the reopened SQLite parent for terminal revalidation",
                &self.inner.approved_parent_path,
            )
        })?;
        let authority_handle = directory.try_clone_authority_handle().map_err(|source| {
            map_revalidation_io_error(
                source,
                "retaining the reopened SQLite parent handle for terminal revalidation",
                &self.inner.approved_parent_path,
            )
        })?;
        let authority = SqliteSourceDirectoryAuthority::retain(
            &self.inner.data_root,
            &authority_handle,
            &self.inner.approved_parent_path,
        )
        .map_err(map_revalidation_error)?;
        match self.inner.policy {
            SqliteSourceSnapshotPolicy::ExactRevision => {
                let family = SqliteSourceFamily::open(&authority, &self.inner.database_name, || {})
                    .map_err(map_revalidation_error)?;
                family.revalidate(&self.inner.native_evidence)?;
            }
            SqliteSourceSnapshotPolicy::StablePrivateCopy => {
                let family = SqliteSourceFamily::open(&authority, &self.inner.database_name, || {})
                    .map_err(map_revalidation_error)?;
                family.revalidate_database_identity(&self.inner.native_evidence)?;
            }
            SqliteSourceSnapshotPolicy::PinnedReadOnlyWal => {
                let family = SqliteSourceFamily::open(&authority, &self.inner.database_name, || {})
                    .map_err(map_revalidation_error)?;
                revalidate_live_database_schema(
                    &family,
                    &self.inner.native_evidence,
                    &self.inner.evidence.schema,
                )?;
            }
        }
        self.inner.snapshot_context.record_terminal_revalidation()
    }
}

#[derive(Debug, Default)]
struct SqliteSourceTerminalFenceSlot {
    fence: Mutex<Option<SqliteSourceTerminalFence>>,
}

impl SqliteSourceTerminalFenceSlot {
    fn install(&self, fence: SqliteSourceTerminalFence) -> SqliteSourceAccessResult<()> {
        let mut retained =
            self.fence
                .lock()
                .map_err(|_| SqliteSourceAccessError::SnapshotUnavailable {
                    reason: "the retained SQLite terminal fence lock was poisoned".to_owned(),
                })?;
        if retained.is_some() {
            return Err(SqliteSourceAccessError::SnapshotUnavailable {
                reason: "the SQLite snapshot published more than one terminal fence".to_owned(),
            });
        }
        *retained = Some(fence);
        Ok(())
    }

    fn revalidate(&self) -> SqliteSourceAccessResult<()> {
        let retained =
            self.fence
                .lock()
                .map_err(|_| SqliteSourceAccessError::SnapshotUnavailable {
                    reason: "the retained SQLite terminal fence lock was poisoned".to_owned(),
                })?;
        retained
            .as_ref()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?
            .revalidate()
    }
}

/// A stock read-only SQLite connection with a pinned read transaction.
#[must_use = "call seal() or finish() after provider queries and before publishing observations"]
#[derive(Debug)]
pub struct SqliteSourceReadSnapshot {
    connection: Option<Connection>,
    family: Option<SqliteSourceFamily>,
    native_evidence: SqliteFamilyEvidence,
    sqlite_evidence: SqliteSnapshotEvidence,
    evidence: SqliteSourceEvidence,
    policy: SqliteSourceSnapshotPolicy,
    admitted_revision_is_replay_safe: bool,
    strategy: SqliteSourceSnapshotStrategy,
    copied_bytes: u64,
    _snapshot_directory: Option<TempDir>,
    _live_authority_handle: Option<File>,
    _scratch: Arc<SqliteRouteScratch>,
    snapshot_activity: Option<SqliteSourceSnapshotActivity>,
    snapshot_context: Arc<SqliteSourceSnapshotContext>,
    terminal_fence_slot: Arc<SqliteSourceTerminalFenceSlot>,
    explicitly_completed: bool,
    #[cfg(any(test, feature = "test-support"))]
    fail_next_cleanup: bool,
}

impl SqliteSourceReadSnapshot {
    pub fn connection(&self) -> SqliteSourceAccessResult<&Connection> {
        let connection = self
            .connection
            .as_ref()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?;
        verify_snapshot_active(connection)?;
        Ok(connection)
    }

    pub fn evidence(&self) -> &SqliteSourceEvidence {
        &self.evidence
    }

    pub fn admitted_revision_is_replay_safe(&self) -> bool {
        self.admitted_revision_is_replay_safe
    }

    /// Retains a content-free terminal revalidator before ownership of this
    /// snapshot is passed to a scanner that closes it through [`Self::finish`].
    ///
    /// The callback fails closed until the snapshot has sealed successfully.
    pub fn terminal_revalidator(
        &self,
    ) -> Box<dyn Fn() -> SqliteSourceAccessResult<()> + Send + Sync + 'static> {
        let slot = Arc::clone(&self.terminal_fence_slot);
        Box::new(move || slot.revalidate())
    }

    pub fn strategy(&self) -> SqliteSourceSnapshotStrategy {
        self.strategy
    }

    pub fn copied_bytes(&self) -> u64 {
        self.copied_bytes
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn family_revalidation_count(&self) -> u32 {
        self.family
            .as_ref()
            .map(SqliteSourceFamily::revalidation_count)
            .unwrap_or_default()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn snapshot_directory(&self) -> Option<&Path> {
        self._snapshot_directory
            .as_ref()
            .map(tempfile::TempDir::path)
    }

    /// Revalidates the pinned SQLite view and retained DB family without
    /// ending the read transaction.
    pub fn revalidate(&self) -> SqliteSourceAccessResult<()> {
        let connection = self.connection()?;
        let current_sqlite_evidence = capture_sqlite_evidence(connection)?;
        if current_sqlite_evidence != self.sqlite_evidence {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        let family = self
            .family
            .as_ref()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?;
        match self.policy {
            SqliteSourceSnapshotPolicy::ExactRevision => family.revalidate(&self.native_evidence),
            SqliteSourceSnapshotPolicy::PinnedReadOnlyWal => {
                family.revalidate_database_identity(&self.native_evidence)
            }
            SqliteSourceSnapshotPolicy::StablePrivateCopy => {
                family.revalidate_database_identity(&self.native_evidence)
            }
        }
    }

    /// Ends this read snapshot and retains its exact physical source-family
    /// authority for cheap commit-time revalidation.
    pub fn seal(mut self) -> SqliteSourceAccessResult<SqliteSourceTerminalFence> {
        self.explicitly_completed = true;
        if let Err(error) = self.revalidate() {
            return match self.cleanup_snapshot_storage() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(SqliteSourceAccessError::Finalization {
                    primary: Box::new(error),
                    cleanup: Box::new(cleanup),
                }),
            };
        }
        let family = match self.family.take() {
            Some(family) => family,
            None => {
                let error = SqliteSourceAccessError::SnapshotNotActive;
                return match self.cleanup_snapshot_storage() {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(SqliteSourceAccessError::Finalization {
                        primary: Box::new(error),
                        cleanup: Box::new(cleanup),
                    }),
                };
            }
        };
        let approved_parent_path = family.approved_parent_path().to_path_buf();
        let database_name = family.database_name().to_os_string();
        let data_root = self.snapshot_context.data_root.clone();
        drop(family);
        self.cleanup_snapshot_storage()?;
        let fence = SqliteSourceTerminalFence {
            inner: Arc::new(SqliteSourceTerminalFenceInner {
                data_root,
                approved_parent_path,
                database_name,
                native_evidence: self.native_evidence.clone(),
                evidence: self.evidence.clone(),
                policy: self.policy,
                snapshot_context: Arc::clone(&self.snapshot_context),
            }),
        };
        fence.revalidate()?;
        self.terminal_fence_slot.install(fence.clone())?;
        self.snapshot_context.record_terminal_fence()?;
        Ok(fence)
    }

    fn cleanup_snapshot_storage(&mut self) -> SqliteSourceAccessResult<()> {
        let artifact = if self._snapshot_directory.is_some() {
            SqliteArtifactKind::PrivateSourceCopy
        } else {
            SqliteArtifactKind::ProviderDatabase
        };
        #[cfg(any(test, feature = "test-support"))]
        if std::mem::take(&mut self.fail_next_cleanup) {
            let path = self._snapshot_directory.as_ref().map_or_else(
                || PathBuf::from("<injected-snapshot-cleanup>"),
                |directory| directory.path().to_path_buf(),
            );
            return Err(SqliteSourceAccessError::ScratchIoUnavailable {
                operation: "removing a ctx-owned SQLite snapshot directory",
                path,
                source: std::io::Error::other("injected SQLite snapshot cleanup failure"),
            }
            .with_diagnostic(
                SqliteFailurePhase::Cleanup,
                artifact,
                0,
                0,
                SqliteCleanupStatus::Failed,
            ));
        }
        let close_connection = self.connection.take().map_or(Ok(()), |connection| {
            close_snapshot_read_connection(connection, artifact)
        });
        let close_directory = self._snapshot_directory.take().map_or(Ok(()), |directory| {
            snapshot::close_private_snapshot_directory(directory, artifact, 0, 0)
        });
        drop(self.snapshot_activity.take());
        combine_sqlite_source_cleanup(close_connection, close_directory)
    }

    /// Compatibility path for callers that need only closing evidence.
    ///
    /// New shared lifecycles should keep the fence returned by [`Self::seal`]
    /// through commit-time physical revalidation.
    pub fn finish(self) -> SqliteSourceAccessResult<SqliteSourceEvidence> {
        let fence = self.seal()?;
        Ok(fence.evidence().clone())
    }

    /// Completes a provider operation and always seals the physical snapshot,
    /// preserving both failures when the operation and finalization fail.
    pub fn finish_with<T, E>(
        self,
        primary: std::result::Result<T, E>,
    ) -> std::result::Result<T, crate::SqliteReadFinalizationError<E, SqliteSourceAccessError>>
    {
        crate::sqlite::combine_sqlite_read_finalization(primary, self.finish().map(|_| ()))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn snapshot_counters(&self) -> SqliteSourceSnapshotCounters {
        self.snapshot_context.snapshot()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn counter_observer(&self) -> SqliteSourceSnapshotCounterObserver {
        SqliteSourceSnapshotCounterObserver {
            context: Arc::clone(&self.snapshot_context),
        }
    }
}

impl Drop for SqliteSourceReadSnapshot {
    fn drop(&mut self) {
        if !self.explicitly_completed {
            self.snapshot_context.record_unfinished_drop();
        }
        if let Err(error) = self.cleanup_snapshot_storage() {
            eprintln!("ctx SQLite snapshot fallback cleanup failed: {error}");
        }
    }
}

fn close_snapshot_read_connection(
    connection: Connection,
    artifact: SqliteArtifactKind,
) -> SqliteSourceAccessResult<()> {
    let clear = clear_snapshot_authorizer(&connection).map_err(|source| {
        SqliteSourceAccessError::CleanupUnavailable {
            operation: "clearing the SQLite snapshot authorizer",
            source: Box::new(source),
        }
        .with_diagnostic(
            SqliteFailurePhase::Cleanup,
            artifact,
            0,
            0,
            SqliteCleanupStatus::Failed,
        )
    });
    let rollback = connection.execute_batch("ROLLBACK").map_err(|source| {
        SqliteSourceAccessError::ScratchSqliteUnavailable {
            operation: "ending the private SQLite read snapshot",
            source,
        }
        .with_diagnostic(
            SqliteFailurePhase::Cleanup,
            artifact,
            0,
            0,
            SqliteCleanupStatus::Failed,
        )
    });
    let close = snapshot::close_private_sqlite_connection(
        connection,
        "closing the private SQLite read snapshot",
        artifact,
        0,
        0,
    );
    let result = combine_sqlite_source_cleanup(clear, rollback);
    combine_sqlite_source_cleanup(result, close)
}

fn revalidate_live_database_schema(
    family: &SqliteSourceFamily,
    native_evidence: &SqliteFamilyEvidence,
    expected_schema: &SqliteSchemaEvidence,
) -> SqliteSourceAccessResult<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (family, native_evidence, expected_schema);
        Err(SqliteSourceAccessError::SnapshotUnavailable {
            reason: "pinned read-only WAL snapshots require the Linux unix VFS".to_owned(),
        })
    }
    #[cfg(target_os = "linux")]
    {
        family.revalidate_database_identity(native_evidence)?;
        let (connection, _authority_handle) =
            snapshot::acquisition::open_pinned_read_only_wal(family)
                .map_err(map_revalidation_error)?;
        let validation = (|| {
            verify_connection_read_only(&connection)?;
            configure_and_pin_snapshot(&connection)?;
            let current = capture_sqlite_evidence(&connection)?;
            if current.schema() != expected_schema {
                return Err(SqliteSourceAccessError::SourceChanged);
            }
            family.revalidate_database_identity(native_evidence)
        })();
        let cleanup =
            close_snapshot_read_connection(connection, SqliteArtifactKind::ProviderDatabase);
        combine_sqlite_source_cleanup(validation, cleanup).map_err(map_revalidation_error)
    }
}

fn combine_sqlite_source_cleanup(
    primary: SqliteSourceAccessResult<()>,
    cleanup: SqliteSourceAccessResult<()>,
) -> SqliteSourceAccessResult<()> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(primary), Err(cleanup)) => Err(SqliteSourceAccessError::Finalization {
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
        }),
    }
}

mod family;
mod snapshot;

use family::{
    capture_sqlite_evidence, clear_snapshot_authorizer, configure_and_pin_snapshot,
    map_provider_source_error, map_provider_source_revalidation_error, map_revalidation_error,
    map_revalidation_io_error, sqlite_error, validate_approved_parent_path,
    verify_connection_read_only, verify_snapshot_active, ExpectedObjectKind, NativeFileIdentity,
    NativeFileState, SqliteConnectionEvidence, SqliteFamilyEvidence, SqliteFamilyMember,
    SqliteSchemaEvidence, SqliteSnapshotEvidence, SqliteSourceFamily,
};
#[cfg(any(test, feature = "test-support"))]
pub use snapshot::{
    fail_next_opened_snapshot_cleanup_for_test, fail_next_private_directory_cleanup_for_test,
    force_next_pinned_wal_unavailable_for_test,
};
pub use snapshot::{
    open_root_handle_sqlite_source_snapshot, open_root_handle_sqlite_source_snapshot_with_limits,
    retain_sqlite_source_directory_authority, SqliteSourceSnapshotLimits,
};

#[cfg(any(test, feature = "test-support"))]
mod tests;
