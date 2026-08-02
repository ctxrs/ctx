//! Stock SQLite snapshots for root-authorized provider databases.
//!
//! The ordinary provider-source layer approves and retains the database parent
//! directory. This module keeps that [`ProviderSourceDirectory`] capability,
//! opens every DB/WAL/SHM/journal leaf relative to it, rejects symlink,
//! reparse-point, cross-filesystem, and non-regular members, and never asks
//! SQLite to create or update files in the provider directory.
//!
//! Strict snapshots open a certified sidecar-free database through SQLite's
//! immutable URI mode or copy an exact DB/WAL family, with bounded I/O, to a
//! private directory below the ctx data root. An explicit logical-online-backup
//! policy instead retains a private logical DB while allowing later commits on
//! the same authorized database and WAL objects. Family-member replacement or
//! appearance remains fail-closed. Rollback journals remain typed unavailable
//! because recovery could require database writes. SHM is bounded volatile lock
//! coordination; provider DB/WAL bytes and directory entries are never mutated.

use std::{
    ffi::{c_char, c_void, OsStr, OsString},
    fs::{File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    ptr,
    sync::{Arc, Mutex, MutexGuard},
};

use ctx_history_core::platform_security::create_private_directory_all;
use rusqlite::{config::DbConfig, ffi, Connection, OpenFlags};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use thiserror::Error;
use url::Url;

#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;

use crate::{
    common::io::{
        OpenedProviderSourceFile, OpenedProviderSourcePath, ProviderSourceDirectory,
        ProviderSourceRoot,
    },
    CaptureError,
};

const EVIDENCE_DOMAIN: &[u8] = b"ctx-stock-sqlite-snapshot-v2\0";
// Admit an approximately 1 GiB provider database together with an active WAL
// of comparable size while retaining one finite cumulative copy bound.
const SQLITE_SNAPSHOT_MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const SQLITE_COPY_BUFFER_BYTES: usize = 64 * 1024;
const SQLITE_WAL_TOKEN_BYTES: usize = 64;
const SQLITE_SHM_MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqliteSourceComponent {
    RollbackJournal,
}

impl std::fmt::Display for SqliteSourceComponent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::RollbackJournal => "rollback journal",
        })
    }
}

#[derive(Debug, Error)]
pub(crate) enum SqliteSourceAccessError {
    #[error("unsafe SQLite source file {path:?}: {reason}")]
    UnsafeFile { path: PathBuf, reason: &'static str },
    #[error("SQLite source I/O failed during {operation} for {path:?}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("SQLite source open failed during {operation}: {source}")]
    Sqlite {
        operation: &'static str,
        #[source]
        source: rusqlite::Error,
    },
    #[error("SQLite source control {operation} failed with code {code}")]
    SqliteControl { operation: &'static str, code: i32 },
    #[error("SQLite source connection is not read-only")]
    ConnectionNotReadOnly,
    #[error("SQLite source connection is not query-only")]
    ConnectionNotQueryOnly,
    #[error("SQLite source connection does not match the approved path")]
    ConnectionIdentityMismatch,
    #[error("SQLite source file changed while its read snapshot was active")]
    SourceChanged,
    #[error("SQLite source snapshot exceeds the bounded limit for {path:?}: {length} > {maximum}")]
    SnapshotTooLarge {
        path: PathBuf,
        length: u64,
        maximum: u64,
    },
    #[error("SQLite source snapshot is unavailable: {reason}")]
    SnapshotUnavailable { reason: String },
    #[error("SQLite {component} is unavailable: {capability}")]
    UnsupportedSidecarIdentity {
        component: SqliteSourceComponent,
        capability: &'static str,
    },
    #[error("SQLite source snapshot transaction is no longer active")]
    SnapshotNotActive,
}

pub(crate) type SqliteSourceAccessResult<T> = Result<T, SqliteSourceAccessError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqliteSourceSnapshotStrategy {
    #[cfg(target_os = "linux")]
    ImmutableMain,
    CopiedFamily,
    LogicalOnlineBackup,
}

/// Selects how one authorized provider SQLite leaf is stabilized.
///
/// The strict physical-family policy remains the default for every existing
/// caller. Logical online backup is an explicit provider opt-in: it pins a
/// short source transaction, copies that view into private ctx storage through
/// SQLite's backup API, and thereafter fences only the approved parent and
/// admitted DB/WAL/SHM object identities. Ordinary commits and WAL growth on
/// those same objects cannot invalidate the admitted private snapshot, while
/// sidecar appearance, disappearance, and replacement remain fail-closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SqliteSourceSnapshotPolicy {
    StrictPhysicalFamily,
    LogicalOnlineBackup,
}

/// Content-free work and concurrency counters for one retained SQLite
/// directory authority and all snapshots opened through its clones.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SqliteSourceSnapshotCounters {
    immutable_snapshot_opens: u64,
    copied_snapshot_opens: u64,
    logical_online_backup_opens: u64,
    source_bytes_copied: u64,
    logical_online_backup_bytes: u64,
    #[cfg(test)]
    logical_projection_passes: u64,
    #[cfg(test)]
    logical_rows_projected: u64,
    #[cfg(test)]
    documents_staged: u64,
    #[cfg(test)]
    logical_noops: u64,
    #[cfg(test)]
    logical_replacements: u64,
    terminal_fences: u64,
    terminal_revalidations: u64,
    active_snapshots: u64,
    active_snapshot_bytes: u64,
    max_active_snapshots: u64,
    max_active_snapshot_bytes: u64,
}

impl SqliteSourceSnapshotCounters {
    pub(crate) const fn immutable_snapshot_opens(self) -> u64 {
        self.immutable_snapshot_opens
    }

    pub(crate) const fn copied_snapshot_opens(self) -> u64 {
        self.copied_snapshot_opens
    }

    pub(crate) const fn logical_online_backup_opens(self) -> u64 {
        self.logical_online_backup_opens
    }

    pub(crate) const fn source_bytes_copied(self) -> u64 {
        self.source_bytes_copied
    }

    #[cfg(test)]
    pub(crate) const fn logical_online_backup_bytes(self) -> u64 {
        self.logical_online_backup_bytes
    }

    #[cfg(test)]
    pub(crate) const fn logical_projection_passes(self) -> u64 {
        self.logical_projection_passes
    }

    #[cfg(test)]
    pub(crate) const fn logical_rows_projected(self) -> u64 {
        self.logical_rows_projected
    }

    #[cfg(test)]
    pub(crate) const fn documents_staged(self) -> u64 {
        self.documents_staged
    }

    #[cfg(test)]
    pub(crate) const fn logical_noops(self) -> u64 {
        self.logical_noops
    }

    #[cfg(test)]
    pub(crate) const fn logical_replacements(self) -> u64 {
        self.logical_replacements
    }

    pub(crate) const fn terminal_fences(self) -> u64 {
        self.terminal_fences
    }

    pub(crate) const fn terminal_revalidations(self) -> u64 {
        self.terminal_revalidations
    }

    pub(crate) const fn active_snapshots(self) -> u64 {
        self.active_snapshots
    }

    #[cfg(test)]
    pub(crate) const fn active_snapshot_bytes(self) -> u64 {
        self.active_snapshot_bytes
    }

    pub(crate) const fn max_active_snapshots(self) -> u64 {
        self.max_active_snapshots
    }

    #[cfg(test)]
    pub(crate) const fn max_active_snapshot_bytes(self) -> u64 {
        self.max_active_snapshot_bytes
    }
}

#[derive(Debug)]
struct SqliteSourceSnapshotContext {
    data_root: PathBuf,
    counters: Mutex<SqliteSourceSnapshotCounters>,
}

impl SqliteSourceSnapshotContext {
    fn snapshot(&self) -> SqliteSourceSnapshotCounters {
        *self.lock()
    }

    fn record_source_bytes_copied(&self, bytes: u64) -> SqliteSourceAccessResult<()> {
        let mut counters = self.lock();
        counters.source_bytes_copied =
            checked_counter_add(counters.source_bytes_copied, bytes, "source bytes copied")?;
        Ok(())
    }

    fn record_logical_online_backup_bytes(&self, bytes: u64) -> SqliteSourceAccessResult<()> {
        let mut counters = self.lock();
        counters.logical_online_backup_bytes = checked_counter_add(
            counters.logical_online_backup_bytes,
            bytes,
            "logical online-backup bytes",
        )?;
        Ok(())
    }

    #[cfg(test)]
    fn record_logical_projection(
        &self,
        rows: u64,
        documents: u64,
        unchanged: bool,
    ) -> SqliteSourceAccessResult<()> {
        let mut counters = self.lock();
        let mut next = *counters;
        next.logical_projection_passes = checked_counter_add(
            next.logical_projection_passes,
            1,
            "logical projection passes",
        )?;
        next.logical_rows_projected =
            checked_counter_add(next.logical_rows_projected, rows, "logical rows projected")?;
        next.documents_staged =
            checked_counter_add(next.documents_staged, documents, "SQLite documents staged")?;
        if unchanged {
            next.logical_noops = checked_counter_add(next.logical_noops, 1, "logical no-ops")?;
        } else {
            next.logical_replacements =
                checked_counter_add(next.logical_replacements, 1, "logical replacements")?;
        }
        *counters = next;
        Ok(())
    }

    fn record_open(
        self: &Arc<Self>,
        strategy: SqliteSourceSnapshotStrategy,
        active_bytes: u64,
    ) -> SqliteSourceAccessResult<SqliteSourceSnapshotActivity> {
        let mut counters = self.lock();
        let mut next = *counters;
        match strategy {
            #[cfg(target_os = "linux")]
            SqliteSourceSnapshotStrategy::ImmutableMain => {
                next.immutable_snapshot_opens = checked_counter_add(
                    next.immutable_snapshot_opens,
                    1,
                    "immutable snapshot opens",
                )?;
            }
            SqliteSourceSnapshotStrategy::CopiedFamily => {
                next.copied_snapshot_opens =
                    checked_counter_add(next.copied_snapshot_opens, 1, "copied snapshot opens")?;
            }
            SqliteSourceSnapshotStrategy::LogicalOnlineBackup => {
                next.logical_online_backup_opens = checked_counter_add(
                    next.logical_online_backup_opens,
                    1,
                    "logical online-backup opens",
                )?;
            }
        }
        next.active_snapshots = checked_counter_add(next.active_snapshots, 1, "active snapshots")?;
        next.active_snapshot_bytes = checked_counter_add(
            next.active_snapshot_bytes,
            active_bytes,
            "active snapshot bytes",
        )?;
        next.max_active_snapshots = next.max_active_snapshots.max(next.active_snapshots);
        next.max_active_snapshot_bytes = next
            .max_active_snapshot_bytes
            .max(next.active_snapshot_bytes);
        *counters = next;
        drop(counters);
        Ok(SqliteSourceSnapshotActivity {
            context: Arc::clone(self),
            active_bytes,
        })
    }

    fn record_terminal_fence(&self) -> SqliteSourceAccessResult<()> {
        let mut counters = self.lock();
        counters.terminal_fences =
            checked_counter_add(counters.terminal_fences, 1, "terminal fences")?;
        Ok(())
    }

    fn record_terminal_revalidation(&self) -> SqliteSourceAccessResult<()> {
        let mut counters = self.lock();
        counters.terminal_revalidations =
            checked_counter_add(counters.terminal_revalidations, 1, "terminal revalidations")?;
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, SqliteSourceSnapshotCounters> {
        match self.counters.lock() {
            Ok(counters) => counters,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[derive(Debug)]
struct SqliteSourceSnapshotActivity {
    context: Arc<SqliteSourceSnapshotContext>,
    active_bytes: u64,
}

impl Drop for SqliteSourceSnapshotActivity {
    fn drop(&mut self) {
        let mut counters = self.context.lock();
        counters.active_snapshots = counters.active_snapshots.saturating_sub(1);
        counters.active_snapshot_bytes = counters
            .active_snapshot_bytes
            .saturating_sub(self.active_bytes);
    }
}

fn checked_counter_add(
    value: u64,
    increment: u64,
    counter: &'static str,
) -> SqliteSourceAccessResult<u64> {
    value
        .checked_add(increment)
        .ok_or_else(|| SqliteSourceAccessError::SnapshotUnavailable {
            reason: format!("SQLite snapshot accounting overflowed {counter}"),
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SqliteSourceEvidence {
    identity: [u8; 32],
    length: u64,
    wal_length: Option<u64>,
    shared_memory_length: Option<u64>,
    schema: SqliteSchemaEvidence,
    source: SqliteConnectionEvidence,
    revision: [u8; 32],
}

impl SqliteSourceEvidence {
    #[cfg(test)]
    pub(crate) fn identity(&self) -> &[u8; 32] {
        &self.identity
    }

    #[cfg(test)]
    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    pub(crate) fn revision(&self) -> &[u8; 32] {
        &self.revision
    }

    #[cfg(test)]
    pub(crate) fn wal_length(&self) -> Option<u64> {
        self.wal_length
    }

    #[cfg(test)]
    pub(crate) fn shared_memory_length(&self) -> Option<u64> {
        self.shared_memory_length
    }
}

/// Retained authority for one approved SQLite parent directory.
///
/// `path` is retained only to certify the parent route and describe errors.
/// SQLite family members are always opened relative to `directory`.
#[derive(Debug, Clone)]
pub(crate) struct SqliteSourceDirectoryAuthority {
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

    fn data_root(&self) -> &Path {
        &self.snapshot_context.data_root
    }

    pub(crate) fn snapshot_counters(&self) -> SqliteSourceSnapshotCounters {
        self.snapshot_context.snapshot()
    }

    pub(crate) fn open_logical_online_backup_snapshot(
        &self,
        database_name: &OsStr,
    ) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
        open_root_handle_sqlite_source_snapshot_with_policy(
            self,
            database_name,
            SqliteSourceSnapshotPolicy::LogicalOnlineBackup,
        )
    }

    #[cfg(test)]
    pub(crate) fn record_logical_projection(
        &self,
        rows: u64,
        documents: u64,
        unchanged: bool,
    ) -> SqliteSourceAccessResult<()> {
        self.snapshot_context
            .record_logical_projection(rows, documents, unchanged)
    }

    pub(crate) fn revalidate(&self) -> SqliteSourceAccessResult<()> {
        let retained = self
            .directory
            .try_clone_authority_handle()
            .map_err(|_| SqliteSourceAccessError::SourceChanged)
            .and_then(|directory| {
                NativeFileState::read(&directory, &self.path, ExpectedObjectKind::Directory)
                    .map_err(map_revalidation_error)
            })?;
        if retained.identity != self.identity {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        let named_root = ProviderSourceRoot::open(&self.path)
            .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
        let named_directory = named_root
            .directory()
            .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
        let named = named_directory
            .try_clone_authority_handle()
            .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
        let named_state = NativeFileState::read(&named, &self.path, ExpectedObjectKind::Directory)
            .map_err(map_revalidation_error)?;
        if named_state.identity == self.identity {
            Ok(())
        } else {
            Err(SqliteSourceAccessError::SourceChanged)
        }
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
    _retained_snapshot_directory: Option<TempDir>,
    snapshot_context: Arc<SqliteSourceSnapshotContext>,
}

#[derive(Clone, Debug)]
pub(crate) struct SqliteSourceTerminalFence {
    inner: Arc<SqliteSourceTerminalFenceInner>,
}

impl SqliteSourceTerminalFence {
    pub(crate) fn evidence(&self) -> &SqliteSourceEvidence {
        &self.inner.evidence
    }

    /// Revalidates the exact retained source family without opening SQLite or
    /// acquiring another source snapshot.
    pub(crate) fn revalidate(&self) -> SqliteSourceAccessResult<()> {
        let root = ProviderSourceRoot::open(&self.inner.approved_parent_path)
            .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
        let directory = root
            .directory()
            .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
        let authority_handle = directory
            .try_clone_authority_handle()
            .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
        let authority = SqliteSourceDirectoryAuthority::retain(
            &self.inner.data_root,
            &authority_handle,
            &self.inner.approved_parent_path,
        )
        .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
        match self.inner.policy {
            SqliteSourceSnapshotPolicy::StrictPhysicalFamily => {
                let family = SqliteSourceFamily::open(&authority, &self.inner.database_name, || {})
                    .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
                family.revalidate(&self.inner.native_evidence)?;
            }
            SqliteSourceSnapshotPolicy::LogicalOnlineBackup => {
                let family = SqliteSourceFamily::open(&authority, &self.inner.database_name, || {})
                    .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
                family.revalidate_logical_database_identity(&self.inner.native_evidence)?;
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
pub(crate) struct SqliteSourceReadSnapshot {
    connection: Option<Connection>,
    family: Option<SqliteSourceFamily>,
    native_evidence: SqliteFamilyEvidence,
    sqlite_evidence: SqliteSnapshotEvidence,
    evidence: SqliteSourceEvidence,
    policy: SqliteSourceSnapshotPolicy,
    admitted_revision_is_replay_safe: bool,
    #[cfg(test)]
    strategy: SqliteSourceSnapshotStrategy,
    #[cfg(test)]
    copied_bytes: u64,
    _snapshot_directory: Option<TempDir>,
    snapshot_activity: Option<SqliteSourceSnapshotActivity>,
    snapshot_context: Arc<SqliteSourceSnapshotContext>,
    terminal_fence_slot: Arc<SqliteSourceTerminalFenceSlot>,
}

impl SqliteSourceReadSnapshot {
    pub(crate) fn connection(&self) -> SqliteSourceAccessResult<&Connection> {
        let connection = self
            .connection
            .as_ref()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?;
        verify_snapshot_active(connection)?;
        Ok(connection)
    }

    pub(crate) fn evidence(&self) -> &SqliteSourceEvidence {
        &self.evidence
    }

    pub(crate) fn admitted_revision_is_replay_safe(&self) -> bool {
        self.admitted_revision_is_replay_safe
    }

    /// Retains a content-free terminal revalidator before ownership of this
    /// snapshot is passed to a scanner that closes it through [`Self::finish`].
    ///
    /// The callback fails closed until the snapshot has sealed successfully.
    pub(crate) fn terminal_revalidator(
        &self,
    ) -> Box<dyn Fn() -> SqliteSourceAccessResult<()> + Send + Sync + 'static> {
        let slot = Arc::clone(&self.terminal_fence_slot);
        Box::new(move || slot.revalidate())
    }

    #[cfg(test)]
    pub(crate) fn strategy(&self) -> SqliteSourceSnapshotStrategy {
        self.strategy
    }

    #[cfg(test)]
    pub(crate) fn copied_bytes(&self) -> u64 {
        self.copied_bytes
    }

    #[cfg(test)]
    pub(crate) fn family_revalidation_count(&self) -> u32 {
        self.family
            .as_ref()
            .map(SqliteSourceFamily::revalidation_count)
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn snapshot_directory(&self) -> Option<&Path> {
        self._snapshot_directory
            .as_ref()
            .map(tempfile::TempDir::path)
    }

    /// Revalidates the pinned SQLite view and retained DB family without
    /// ending the read transaction.
    pub(crate) fn revalidate(&self) -> SqliteSourceAccessResult<()> {
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
            SqliteSourceSnapshotPolicy::StrictPhysicalFamily => {
                family.revalidate(&self.native_evidence)
            }
            SqliteSourceSnapshotPolicy::LogicalOnlineBackup => {
                family.revalidate_logical_database_identity(&self.native_evidence)
            }
        }
    }

    /// Ends this read snapshot and retains its exact physical source-family
    /// authority for cheap commit-time revalidation.
    pub(crate) fn seal(mut self) -> SqliteSourceAccessResult<SqliteSourceTerminalFence> {
        self.revalidate()?;
        let connection = self
            .connection
            .as_ref()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?;
        clear_snapshot_authorizer(connection)?;
        connection
            .execute_batch("ROLLBACK")
            .map_err(|source| sqlite_error("ending the provider read snapshot", source))?;
        self.connection.take();
        let family = self
            .family
            .take()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?;
        let approved_parent_path = family.approved_parent_path().to_path_buf();
        let database_name = family.database_name().to_os_string();
        let data_root = self.snapshot_context.data_root.clone();
        drop(family);
        let retained_snapshot_directory = match self.policy {
            SqliteSourceSnapshotPolicy::StrictPhysicalFamily => None,
            SqliteSourceSnapshotPolicy::LogicalOnlineBackup => self._snapshot_directory.take(),
        };
        let fence = SqliteSourceTerminalFence {
            inner: Arc::new(SqliteSourceTerminalFenceInner {
                data_root,
                approved_parent_path,
                database_name,
                native_evidence: self.native_evidence.clone(),
                evidence: self.evidence.clone(),
                policy: self.policy,
                _retained_snapshot_directory: retained_snapshot_directory,
                snapshot_context: Arc::clone(&self.snapshot_context),
            }),
        };
        fence.revalidate()?;
        self.terminal_fence_slot.install(fence.clone())?;
        self.snapshot_context.record_terminal_fence()?;
        drop(self.snapshot_activity.take());
        drop(self._snapshot_directory.take());
        Ok(fence)
    }

    /// Compatibility path for callers that need only closing evidence.
    ///
    /// New shared lifecycles should keep the fence returned by [`Self::seal`]
    /// through commit-time physical revalidation.
    pub(crate) fn finish(self) -> SqliteSourceAccessResult<SqliteSourceEvidence> {
        let fence = self.seal()?;
        Ok(fence.evidence().clone())
    }
}

impl Drop for SqliteSourceReadSnapshot {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.as_ref() {
            let _ = clear_snapshot_authorizer(connection);
            let _ = connection.execute_batch("ROLLBACK");
        }
        self.connection.take();
    }
}

mod family;
mod logical;
mod snapshot;

use family::{
    capture_sqlite_evidence, clear_snapshot_authorizer, configure_and_pin_snapshot,
    map_provider_source_error, map_revalidation_error, sqlite_error, validate_approved_parent_path,
    verify_connection_read_only, verify_snapshot_active, ExpectedObjectKind, NativeFileIdentity,
    NativeFileState, SqliteConnectionEvidence, SqliteFamilyEvidence, SqliteFamilyMember,
    SqliteSchemaEvidence, SqliteSnapshotEvidence, SqliteSourceFamily,
};
pub(crate) use logical::SqliteLogicalSnapshot;
use snapshot::open_root_handle_sqlite_source_snapshot_with_policy;
#[cfg(test)]
use snapshot::{
    certify_root_handle_sqlite_source_snapshot_copy_budget_for_test,
    open_root_handle_sqlite_source_online_backup_after_database_copy_for_test,
    open_root_handle_sqlite_source_online_backup_before_identity_check_for_test,
    open_root_handle_sqlite_source_online_backup_with_scratch_limit_for_test,
    open_root_handle_sqlite_source_snapshot_after_database_copy_for_test,
    open_root_handle_sqlite_source_snapshot_after_parent_certification_for_test,
    open_root_handle_sqlite_source_snapshot_for_test, run_online_backup_with_deadline_for_test,
};
pub(crate) use snapshot::{
    open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
};

#[cfg(test)]
mod tests;
