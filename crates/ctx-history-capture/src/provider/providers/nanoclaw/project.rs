use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::common::io::{OpenedProviderSourceFile, ProviderSourceDirectory, ProviderSourceRoot};
use crate::provider::provider_safe_path_segment;
use crate::provider::sqlite::{open_provider_sqlite_readonly, ReadOnlySqliteConnection};
use crate::provider::sqlite::{sqlite_component_change_token, with_sqlite_read_snapshot};
use crate::provider_sources::{
    observe_ordinary_file, open_root_handle_sqlite_source_snapshot,
    retain_sqlite_source_directory_authority, SqliteSourceAccessError,
    SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot,
};
use crate::{CaptureError, ProviderSourceFailureKind, Result};

use super::position::{nanoclaw_ordered_i64, NanoClawMessageSource};
use super::rows::{
    nanoclaw_observed_bytes, nanoclaw_retained_length_expr, nanoclaw_session_columns,
    nanoclaw_session_projection, NANOCLAW_NATIVE_MAX_RECORD_BYTES,
};
use super::{NANOCLAW_CAPTURE_REVISION, NANOCLAW_POLICY_REVISION};

const NANOCLAW_INVENTORY_PAGE_ENTRIES: usize = 64;
const NANOCLAW_INVENTORY_MIN_INTERVAL: Duration = Duration::from_millis(5);
const NANOCLAW_INVENTORY_HASH_DOMAIN: &[u8] = b"ctx-nanoclaw-inventory-sha256-v1\0";

#[cfg(test)]
std::thread_local! {
    static BEFORE_CENTRAL_GUARD_OPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) struct NanoClawCentralGuardOpenHook;

#[cfg(test)]
impl Drop for NanoClawCentralGuardOpenHook {
    fn drop(&mut self) {
        BEFORE_CENTRAL_GUARD_OPEN.with(|installed| {
            installed.borrow_mut().take();
        });
    }
}

#[cfg(test)]
pub(super) fn set_before_central_guard_open_hook(
    hook: impl FnOnce() + 'static,
) -> NanoClawCentralGuardOpenHook {
    BEFORE_CENTRAL_GUARD_OPEN.with(|installed| {
        *installed.borrow_mut() = Some(Box::new(hook));
    });
    NanoClawCentralGuardOpenHook
}

#[cfg(test)]
fn run_before_central_guard_open_hook() {
    BEFORE_CENTRAL_GUARD_OPEN.with(|installed| {
        if let Some(hook) = installed.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_central_guard_open_hook() {}

#[derive(Clone, PartialEq, Eq)]
struct NanoClawFrozenFileMetadata {
    length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
    change_token: [u8; 32],
}

impl NanoClawFrozenFileMetadata {
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "NanoClaw SQLite component must be a regular non-symlink file",
            });
        }
        let observation = observe_ordinary_file(path)?;
        if metadata.len() != observation.len()
            || metadata.modified().ok() != Some(observation.modified_at())
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let change_token = sqlite_component_change_token(path, &observation)?;
        Self::from_metadata(&metadata, change_token)
    }

    fn read_optional(path: &Path) -> Result<Option<Self>> {
        match fs::symlink_metadata(path) {
            Ok(metadata)
                if metadata.file_type().is_file() && !metadata.file_type().is_symlink() =>
            {
                Self::read(path).map(Some)
            }
            Ok(_) => Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "NanoClaw SQLite sidecar must be a regular non-symlink file",
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(CaptureError::Io(error)),
        }
    }

    fn from_metadata(metadata: &fs::Metadata, change_token: [u8; 32]) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            #[cfg(unix)]
            device: Some(metadata.dev()),
            #[cfg(not(unix))]
            device: None,
            #[cfg(unix)]
            inode: Some(metadata.ino()),
            #[cfg(not(unix))]
            inode: None,
            change_token,
        })
    }

    fn from_opened(opened: &OpenedProviderSourceFile) -> Result<Self> {
        Self::from_metadata(
            opened.metadata(),
            nanoclaw_root_bound_component_token(opened.metadata()),
        )
    }

    fn update_hash(&self, hasher: &mut Sha256) {
        nanoclaw_hash_u64(hasher, self.length);
        let (sign, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => (1_u8, duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                (0_u8, duration.as_secs(), duration.subsec_nanos())
            }
        };
        nanoclaw_hash_bytes(hasher, &[sign]);
        nanoclaw_hash_u64(hasher, seconds);
        nanoclaw_hash_u64(hasher, u64::from(nanos));
        nanoclaw_hash_bytes(hasher, &[u8::from(self.readonly)]);
        nanoclaw_hash_optional_u64(hasher, self.device);
        nanoclaw_hash_optional_u64(hasher, self.inode);
        nanoclaw_hash_bytes(hasher, &self.change_token);
    }
}

#[derive(Debug)]
pub(super) struct NanoClawOpenedSqliteFamily {
    database: OpenedProviderSourceFile,
    wal: Option<OpenedProviderSourceFile>,
    shared_memory: Option<OpenedProviderSourceFile>,
    rollback_journal: Option<OpenedProviderSourceFile>,
}

impl NanoClawOpenedSqliteFamily {
    fn open(root: &ProviderSourceRoot, relative_path: &Path) -> Result<Self> {
        let database = root.open_file(relative_path)?;
        Ok(Self {
            database,
            wal: nanoclaw_open_optional_root_file(
                root,
                &nanoclaw_sidecar_path(relative_path, "-wal"),
            )?,
            shared_memory: nanoclaw_open_optional_root_file(
                root,
                &nanoclaw_sidecar_path(relative_path, "-shm"),
            )?,
            rollback_journal: nanoclaw_open_optional_root_file(
                root,
                &nanoclaw_sidecar_path(relative_path, "-journal"),
            )?,
        })
    }

    fn open_optional(root: &ProviderSourceRoot, relative_path: &Path) -> Result<Option<Self>> {
        match root.open_file(relative_path) {
            Ok(database) => Ok(Some(Self {
                database,
                wal: nanoclaw_open_optional_root_file(
                    root,
                    &nanoclaw_sidecar_path(relative_path, "-wal"),
                )?,
                shared_memory: nanoclaw_open_optional_root_file(
                    root,
                    &nanoclaw_sidecar_path(relative_path, "-shm"),
                )?,
                rollback_journal: nanoclaw_open_optional_root_file(
                    root,
                    &nanoclaw_sidecar_path(relative_path, "-journal"),
                )?,
            })),
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                for suffix in ["-wal", "-shm", "-journal"] {
                    let sidecar = nanoclaw_sidecar_path(relative_path, suffix);
                    if nanoclaw_open_optional_root_file(root, &sidecar)?.is_some() {
                        return Err(CaptureError::InvalidProviderTranscriptPath {
                            path: root.named_path().join(sidecar),
                            reason: "NanoClaw SQLite sidecar has no main database",
                        });
                    }
                }
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn snapshot(&self) -> Result<NanoClawSqliteSnapshot> {
        Ok(NanoClawSqliteSnapshot {
            database: NanoClawFrozenFileMetadata::from_opened(&self.database)?,
            wal: self
                .wal
                .as_ref()
                .map(NanoClawFrozenFileMetadata::from_opened)
                .transpose()?,
            shared_memory: self
                .shared_memory
                .as_ref()
                .map(NanoClawFrozenFileMetadata::from_opened)
                .transpose()?,
            rollback_journal: self
                .rollback_journal
                .as_ref()
                .map(NanoClawFrozenFileMetadata::from_opened)
                .transpose()?,
        })
    }

    fn revalidate(&self) -> Result<()> {
        self.database.revalidate()?;
        for sidecar in [
            self.wal.as_ref(),
            self.shared_memory.as_ref(),
            self.rollback_journal.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            sidecar.revalidate()?;
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct NanoClawSqliteSnapshot {
    database: NanoClawFrozenFileMetadata,
    wal: Option<NanoClawFrozenFileMetadata>,
    shared_memory: Option<NanoClawFrozenFileMetadata>,
    rollback_journal: Option<NanoClawFrozenFileMetadata>,
}

impl NanoClawSqliteSnapshot {
    pub(super) fn read(path: &Path) -> Result<Self> {
        Ok(Self {
            database: NanoClawFrozenFileMetadata::read(path)?,
            wal: NanoClawFrozenFileMetadata::read_optional(&nanoclaw_sidecar_path(path, "-wal"))?,
            shared_memory: NanoClawFrozenFileMetadata::read_optional(&nanoclaw_sidecar_path(
                path, "-shm",
            ))?,
            rollback_journal: NanoClawFrozenFileMetadata::read_optional(&nanoclaw_sidecar_path(
                path, "-journal",
            ))?,
        })
    }

    pub(super) fn read_optional(path: &Path) -> Result<Option<Self>> {
        match fs::symlink_metadata(path) {
            Ok(metadata)
                if metadata.file_type().is_file() && !metadata.file_type().is_symlink() =>
            {
                Self::read(path).map(Some)
            }
            Ok(_) => Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "NanoClaw message store must be a regular non-symlink file",
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(CaptureError::Io(error)),
        }
    }

    fn update_hash(&self, hasher: &mut Sha256) {
        self.database.update_hash(hasher);
        nanoclaw_hash_optional_file(hasher, self.wal.as_ref());
        nanoclaw_hash_optional_file(hasher, self.shared_memory.as_ref());
        nanoclaw_hash_optional_file(hasher, self.rollback_journal.as_ref());
    }

    pub(super) fn revalidate(&self, path: &Path) -> Result<bool> {
        match Self::read(path) {
            Ok(current) => Ok(current == *self),
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(false)
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"ctx-nanoclaw-sqlite-snapshot-sha256-v1\0");
        self.update_hash(&mut hasher);
        hasher.finalize().into()
    }
}

#[derive(Clone)]
pub(super) struct NanoClawRootBoundDatabase {
    root: ProviderSourceRoot,
    relative_path: PathBuf,
    parent: Arc<NanoClawRetainedSqliteParent>,
}

#[derive(Debug)]
struct NanoClawRetainedSqliteParent {
    directory: ProviderSourceDirectory,
    sqlite: SqliteSourceDirectoryAuthority,
}

impl NanoClawRootBoundDatabase {
    fn bind(root: &ProviderSourceRoot, relative_path: PathBuf) -> Result<Self> {
        let display_path = root.named_path().join(&relative_path);
        let parent_path =
            relative_path
                .parent()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: display_path.clone(),
                    reason: "NanoClaw SQLite database has no authority parent",
                })?;
        if relative_path.file_name().is_none() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: display_path,
                reason: "NanoClaw SQLite database has no leaf name",
            });
        }
        let directory = root.open_directory(parent_path)?;
        let authority_handle = directory.try_clone_authority_handle()?;
        let approved_parent = root.named_path().join(parent_path);
        let sqlite = retain_sqlite_source_directory_authority(&authority_handle, &approved_parent)
            .map_err(|error| nanoclaw_sqlite_access_error(&display_path, error))?;
        directory.revalidate()?;
        root.revalidate()?;
        Ok(Self {
            root: root.clone(),
            relative_path,
            parent: Arc::new(NanoClawRetainedSqliteParent { directory, sqlite }),
        })
    }

    fn display_path(&self) -> PathBuf {
        self.root.named_path().join(&self.relative_path)
    }

    fn database_name(&self) -> Result<&std::ffi::OsStr> {
        self.relative_path
            .file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: self.display_path(),
                reason: "NanoClaw SQLite database has no leaf name",
            })
    }

    fn open_snapshot(&self) -> Result<SqliteSourceReadSnapshot> {
        open_root_handle_sqlite_source_snapshot(&self.parent.sqlite, self.database_name()?)
            .map_err(|error| nanoclaw_sqlite_access_error(&self.display_path(), error))
    }

    fn revalidate_authority(&self) -> Result<()> {
        self.parent.directory.revalidate()?;
        self.root.revalidate()
    }

    fn read(&self) -> Result<NanoClawSqliteSnapshot> {
        let opened = NanoClawOpenedSqliteFamily::open(&self.root, &self.relative_path)?;
        let snapshot = opened.snapshot()?;
        opened.revalidate()?;
        self.revalidate_authority()?;
        Ok(snapshot)
    }

    fn read_optional(&self) -> Result<Option<NanoClawSqliteSnapshot>> {
        let Some(opened) =
            NanoClawOpenedSqliteFamily::open_optional(&self.root, &self.relative_path)?
        else {
            self.revalidate_authority()?;
            return Ok(None);
        };
        let snapshot = opened.snapshot()?;
        opened.revalidate()?;
        self.revalidate_authority()?;
        Ok(Some(snapshot))
    }
}

// This RAII owner keeps SQLite handles and root-bound guards in one value.
// Boxing the 2,024-byte root-bound state would add indirection to every read.
#[allow(clippy::large_enum_variant)]
pub(super) enum NanoClawDatabaseRead {
    Pathname(ReadOnlySqliteConnection),
    RootBound {
        path: PathBuf,
        route: NanoClawRootBoundDatabase,
        opened: NanoClawOpenedSqliteFamily,
        guard: Option<SqliteSourceReadSnapshot>,
    },
}

impl NanoClawDatabaseRead {
    pub(super) fn connection(&self) -> Result<&Connection> {
        match self {
            Self::Pathname(connection) => Ok(connection),
            Self::RootBound { path, guard, .. } => guard
                .as_ref()
                .ok_or(CaptureError::SystemInvariant(
                    "NanoClaw root-bound SQLite guard is no longer active",
                ))?
                .connection()
                .map_err(|error| nanoclaw_sqlite_access_error(path, error)),
        }
    }

    pub(super) fn revalidate(&self, expected: &NanoClawProjectDatabaseSnapshot) -> Result<bool> {
        if let Self::RootBound { opened, route, .. } = self {
            opened.revalidate()?;
            route.revalidate_authority()?;
        }
        expected.revalidate()
    }

    pub(super) fn finish(mut self, expected: &NanoClawProjectDatabaseSnapshot) -> Result<()> {
        if let Self::RootBound {
            path,
            route,
            opened,
            guard,
        } = &mut self
        {
            guard
                .take()
                .ok_or(CaptureError::SystemInvariant(
                    "NanoClaw root-bound SQLite guard is no longer active",
                ))?
                .finish()
                .map_err(|error| nanoclaw_sqlite_access_error(path, error))?;
            opened.revalidate()?;
            route.revalidate_authority()?;
        }
        if expected.revalidate()? {
            Ok(())
        } else {
            Err(CaptureError::SourceChangedDuringCapture)
        }
    }
}

#[derive(Clone)]
pub(super) struct NanoClawProjectDatabaseSnapshot {
    source: NanoClawMessageSource,
    path: PathBuf,
    sqlite: Option<NanoClawSqliteSnapshot>,
    root_bound: Option<NanoClawRootBoundDatabase>,
}

impl NanoClawProjectDatabaseSnapshot {
    pub(super) fn read(session_dir: &Path, source: NanoClawMessageSource) -> Result<Self> {
        let path = session_dir.join(source.file_name());
        let sqlite = NanoClawSqliteSnapshot::read_optional(&path)?;
        Ok(Self {
            source,
            path,
            sqlite,
            root_bound: None,
        })
    }

    fn read_root_bound(
        root: &ProviderSourceRoot,
        session_relative_path: &Path,
        source: NanoClawMessageSource,
    ) -> Result<Self> {
        let relative_path = session_relative_path.join(source.file_name());
        let route = NanoClawRootBoundDatabase::bind(root, relative_path)?;
        let path = route.display_path();
        let sqlite = route.read_optional()?;
        Ok(Self {
            source,
            path,
            sqlite,
            root_bound: Some(route),
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn is_present(&self) -> bool {
        self.sqlite.is_some()
    }

    pub(super) fn revalidate(&self) -> Result<bool> {
        let current = match &self.root_bound {
            Some(route) => route.read_optional(),
            None => NanoClawSqliteSnapshot::read_optional(&self.path),
        };
        match current {
            Ok(current) => Ok(current == self.sqlite),
            Err(CaptureError::SourceChangedDuringCapture)
            | Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(super) fn open_read(&self) -> Result<Option<NanoClawDatabaseRead>> {
        let Some(expected) = self.sqlite.as_ref() else {
            return Ok(None);
        };
        let Some(route) = self.root_bound.as_ref() else {
            return Ok(Some(NanoClawDatabaseRead::Pathname(
                open_provider_sqlite_readonly(&self.path)?,
            )));
        };
        let opened = NanoClawOpenedSqliteFamily::open(&route.root, &route.relative_path)?;
        if opened.snapshot()? != *expected {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let guard = route.open_snapshot()?;
        opened.revalidate()?;
        route.revalidate_authority()?;
        Ok(Some(NanoClawDatabaseRead::RootBound {
            path: self.path.clone(),
            route: route.clone(),
            opened,
            guard: Some(guard),
        }))
    }

    fn update_hash(&self, hasher: &mut Sha256) {
        nanoclaw_hash_bytes(hasher, &[self.source.tag()]);
        nanoclaw_hash_optional_sqlite(hasher, self.sqlite.as_ref());
    }
}

#[derive(Clone)]
struct NanoClawSessionDatabaseSnapshot {
    rowid: i64,
    agent_group_id: String,
    session_id: String,
    inbound: NanoClawProjectDatabaseSnapshot,
    outbound: NanoClawProjectDatabaseSnapshot,
}

impl NanoClawSessionDatabaseSnapshot {
    fn database(&self, source: NanoClawMessageSource) -> &NanoClawProjectDatabaseSnapshot {
        match source {
            NanoClawMessageSource::Inbound => &self.inbound,
            NanoClawMessageSource::Outbound => &self.outbound,
        }
    }

    fn revalidate(&self) -> Result<bool> {
        if !self.inbound.revalidate()? {
            return Ok(false);
        }
        self.outbound.revalidate()
    }
}

#[derive(Clone)]
struct NanoClawProjectInventory {
    digest: [u8; 32],
    session_count: u64,
    // These remain distinct database observations. The project snapshot coordinates their
    // lifetime and revalidation; it does not merge their connections or read transactions.
    session_databases: Vec<NanoClawSessionDatabaseSnapshot>,
}

#[derive(Clone)]
pub(super) struct NanoClawProjectSnapshot {
    central_path: PathBuf,
    central: NanoClawSqliteSnapshot,
    central_root_bound: Option<NanoClawRootBoundDatabase>,
    inventory: NanoClawProjectInventory,
}

pub(super) struct NanoClawSourceBackedProject {
    root: ProviderSourceRoot,
    sessions: ProviderSourceDirectory,
    snapshot: NanoClawProjectSnapshot,
    central_opened: NanoClawOpenedSqliteFamily,
    central_guard: Option<SqliteSourceReadSnapshot>,
}

impl NanoClawSourceBackedProject {
    /// Opens exactly the caller-selected project. No parent/default discovery
    /// and no pathname or copy fallback is available on this route.
    pub(super) fn open(path: &Path) -> Result<Self> {
        let requested_root = nanoclaw_requested_project_root(path)?;
        let root = ProviderSourceRoot::open(&requested_root)?;
        let sessions = root.open_directory(Path::new("data/v2-sessions"))?;
        sessions.revalidate()?;

        let central_relative = PathBuf::from("data/v2.db");
        let central_route = NanoClawRootBoundDatabase::bind(&root, central_relative.clone())?;
        let central_path = central_route.display_path();
        let central_opened = NanoClawOpenedSqliteFamily::open(&root, &central_relative)?;
        let central_snapshot = central_opened.snapshot()?;
        run_before_central_guard_open_hook();
        let central_guard = central_route.open_snapshot()?;
        let inventory = nanoclaw_stream_inventory(
            root.named_path(),
            central_guard
                .connection()
                .map_err(|error| nanoclaw_sqlite_access_error(&central_path, error))?,
            Some(&root),
        )?;
        let snapshot = NanoClawProjectSnapshot {
            central_path,
            central: central_snapshot,
            central_root_bound: Some(central_route.clone()),
            inventory,
        };
        if !snapshot.revalidate_frozen_inventory()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        central_opened.revalidate()?;
        central_route.revalidate_authority()?;
        sessions.revalidate()?;
        root.revalidate()?;
        Ok(Self {
            root,
            sessions,
            snapshot,
            central_opened,
            central_guard: Some(central_guard),
        })
    }

    pub(super) fn root_path(&self) -> &Path {
        self.root.named_path()
    }

    pub(super) fn snapshot(&self) -> &NanoClawProjectSnapshot {
        &self.snapshot
    }

    pub(super) fn connection(&self) -> Result<&Connection> {
        self.central_guard
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "NanoClaw central SQLite guard is no longer active",
            ))?
            .connection()
            .map_err(|error| nanoclaw_sqlite_access_error(&self.snapshot.central_path, error))
    }

    /// Ends the central read transaction and revalidates the complete central
    /// DB family, frozen session-tree inventory, and selected root route.
    pub(super) fn finish(mut self) -> Result<NanoClawProjectSnapshot> {
        self.sessions.revalidate()?;
        if !self.snapshot.revalidate_frozen_inventory()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        self.central_guard
            .take()
            .ok_or(CaptureError::SystemInvariant(
                "NanoClaw central SQLite guard is no longer active",
            ))?
            .finish()
            .map_err(|error| nanoclaw_sqlite_access_error(&self.snapshot.central_path, error))?;
        self.central_opened.revalidate()?;
        self.sessions.revalidate()?;
        if !self.snapshot.revalidate_frozen_inventory()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        self.root.revalidate()?;
        Ok(self.snapshot)
    }
}

impl NanoClawProjectSnapshot {
    pub(super) fn read(project_root: &Path, central_path: &Path) -> Result<Self> {
        Self::read_inner(project_root, central_path)
    }

    fn read_inner(project_root: &Path, central_path: &Path) -> Result<Self> {
        let central = NanoClawSqliteSnapshot::read(central_path)?;
        let conn = open_provider_sqlite_readonly(central_path)?;
        let inventory = with_sqlite_read_snapshot(&conn, || {
            nanoclaw_stream_inventory(project_root, &conn, None)
        })?;
        let snapshot = Self {
            central_path: central_path.to_path_buf(),
            central,
            central_root_bound: None,
            inventory,
        };
        if !snapshot.revalidate_frozen_inventory()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(snapshot)
    }

    pub(super) fn source_backed_revision_evidence(
        &self,
        user_version: i64,
        schema_fingerprint: &str,
    ) -> Result<Vec<u8>> {
        let component_databases = self
            .inventory
            .session_databases
            .iter()
            .map(|session| {
                u64::from(session.inbound.is_present()) + u64::from(session.outbound.is_present())
            })
            .sum();
        Ok(serde_json::to_vec(&NanoClawCompoundRevisionEvidence {
            version: 1,
            capture_revision: NANOCLAW_CAPTURE_REVISION,
            policy_revision: NANOCLAW_POLICY_REVISION,
            user_version,
            schema_fingerprint,
            central_sha256: nanoclaw_hex(&self.central.digest()),
            session_inventory_sha256: nanoclaw_hex(&self.inventory.digest),
            sessions: self.inventory.session_count,
            component_databases,
        })?)
    }

    pub(super) fn source_revision(&self, user_version: i64, schema_fingerprint: &str) -> String {
        format!(
            "nanoclaw-project-snapshot-v1:capture={NANOCLAW_CAPTURE_REVISION};policy={NANOCLAW_POLICY_REVISION};user_version={user_version};schema={schema_fingerprint};sessions={};inventory={};central={}",
            self.inventory.session_count,
            nanoclaw_hex(&self.inventory.digest),
            nanoclaw_hex(&self.central.digest()),
        )
    }

    pub(super) fn database(
        &self,
        rowid: i64,
        agent_group_id: &str,
        session_id: &str,
        source: NanoClawMessageSource,
    ) -> Result<&NanoClawProjectDatabaseSnapshot> {
        let index = self
            .inventory
            .session_databases
            .binary_search_by_key(&rowid, |session| session.rowid)
            .map_err(|_| CaptureError::SourceChangedDuringCapture)?;
        let session = &self.inventory.session_databases[index];
        if session.agent_group_id != agent_group_id || session.session_id != session_id {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(session.database(source))
    }

    pub(super) fn revalidate(&self) -> Result<bool> {
        self.revalidate_frozen_inventory()
    }

    pub(super) fn revalidate_before_commit(&self) -> Result<bool> {
        self.revalidate_frozen_inventory()
    }

    fn revalidate_frozen_inventory(&self) -> Result<bool> {
        if !self.central_revalidates()? {
            return Ok(false);
        }
        for session in &self.inventory.session_databases {
            if !session.revalidate()? {
                return Ok(false);
            }
        }
        self.central_revalidates()
    }

    fn central_revalidates(&self) -> Result<bool> {
        match &self.central_root_bound {
            Some(route) => match route.read() {
                Ok(current) => Ok(current == self.central),
                Err(CaptureError::SourceChangedDuringCapture)
                | Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
                Err(error) => Err(error),
            },
            None => self.central.revalidate(&self.central_path),
        }
    }
}

struct NanoClawInventoryPacer {
    entries: usize,
    window_started: Instant,
}

impl NanoClawInventoryPacer {
    fn new() -> Self {
        Self {
            entries: 0,
            window_started: Instant::now(),
        }
    }

    fn observe(&mut self) {
        self.entries = self.entries.saturating_add(1);
        if self.entries < NANOCLAW_INVENTORY_PAGE_ENTRIES {
            return;
        }
        let elapsed = self.window_started.elapsed();
        if elapsed < NANOCLAW_INVENTORY_MIN_INTERVAL {
            thread::sleep(NANOCLAW_INVENTORY_MIN_INTERVAL - elapsed);
        }
        self.entries = 0;
        self.window_started = Instant::now();
    }
}

fn nanoclaw_stream_inventory(
    project_root: &Path,
    conn: &Connection,
    source_root: Option<&ProviderSourceRoot>,
) -> Result<NanoClawProjectInventory> {
    let columns = nanoclaw_session_columns(conn)?;
    let retained = nanoclaw_retained_length_expr(&nanoclaw_session_projection(conn, &columns)?);
    let mut candidates = conn.prepare(&format!(
        "select s.rowid, {retained} from sessions s order by s.rowid"
    ))?;
    let mut hydrate = conn.prepare(
        "select CAST(id AS TEXT), CAST(agent_group_id AS TEXT) from sessions where rowid = ?1",
    )?;
    let mut rows = candidates.query([])?;
    let mut hasher = Sha256::new();
    hasher.update(NANOCLAW_INVENTORY_HASH_DOMAIN);
    let mut count = 0_u64;
    let mut session_databases = Vec::new();
    let mut pacer = NanoClawInventoryPacer::new();
    while let Some(row) = rows.next()? {
        let rowid: i64 = row.get(0)?;
        let retained_bytes: i64 = row.get(1)?;
        let observed_bytes = nanoclaw_observed_bytes(retained_bytes)?;
        nanoclaw_hash_u64(&mut hasher, nanoclaw_ordered_i64(rowid));
        nanoclaw_hash_u64(&mut hasher, observed_bytes);
        if observed_bytes <= NANOCLAW_NATIVE_MAX_RECORD_BYTES {
            let (session_id, agent_group_id): (String, String) =
                hydrate.query_row([rowid], |row| Ok((row.get(0)?, row.get(1)?)))?;
            nanoclaw_hash_bytes(&mut hasher, session_id.as_bytes());
            nanoclaw_hash_bytes(&mut hasher, agent_group_id.as_bytes());
            if provider_safe_path_segment(&agent_group_id)
                && provider_safe_path_segment(&session_id)
            {
                let session_dir = project_root
                    .join("data")
                    .join("v2-sessions")
                    .join(&agent_group_id)
                    .join(&session_id);
                let session_relative_path = PathBuf::from("data")
                    .join("v2-sessions")
                    .join(&agent_group_id)
                    .join(&session_id);
                let read_component = |source: NanoClawMessageSource| match source_root {
                    Some(root) => NanoClawProjectDatabaseSnapshot::read_root_bound(
                        root,
                        &session_relative_path,
                        source,
                    ),
                    None => NanoClawProjectDatabaseSnapshot::read(&session_dir, source),
                };
                let inbound = read_component(NanoClawMessageSource::Inbound)?;
                let outbound = read_component(NanoClawMessageSource::Outbound)?;
                inbound.update_hash(&mut hasher);
                outbound.update_hash(&mut hasher);
                session_databases.push(NanoClawSessionDatabaseSnapshot {
                    rowid,
                    agent_group_id,
                    session_id,
                    inbound,
                    outbound,
                });
            } else {
                nanoclaw_hash_bytes(&mut hasher, b"unsafe-session-path");
            }
        } else {
            nanoclaw_hash_bytes(&mut hasher, b"oversize-session-row");
        }
        count = count.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "NanoClaw inventory session count overflowed",
        ))?;
        pacer.observe();
    }
    Ok(NanoClawProjectInventory {
        digest: hasher.finalize().into(),
        session_count: count,
        session_databases,
    })
}

#[derive(Serialize)]
struct NanoClawCompoundRevisionEvidence<'a> {
    version: u32,
    capture_revision: u32,
    policy_revision: u32,
    user_version: i64,
    schema_fingerprint: &'a str,
    central_sha256: String,
    session_inventory_sha256: String,
    sessions: u64,
    component_databases: u64,
}

fn nanoclaw_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

fn nanoclaw_open_optional_root_file(
    root: &ProviderSourceRoot,
    relative_path: &Path,
) -> Result<Option<OpenedProviderSourceFile>> {
    match root.open_file(relative_path) {
        Ok(opened) => Ok(Some(opened)),
        Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn nanoclaw_root_bound_component_token(metadata: &fs::Metadata) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-nanoclaw-root-bound-component-v1\0");
    hasher.update(metadata.len().to_le_bytes());
    hasher.update([u8::from(metadata.permissions().readonly())]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        hasher.update(metadata.dev().to_le_bytes());
        hasher.update(metadata.ino().to_le_bytes());
        hasher.update(metadata.mtime().to_le_bytes());
        hasher.update(metadata.mtime_nsec().to_le_bytes());
        hasher.update(metadata.ctime().to_le_bytes());
        hasher.update(metadata.ctime_nsec().to_le_bytes());
    }
    #[cfg(not(unix))]
    {
        let (seconds, nanos) = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map_or((0, 0), |duration| {
                (duration.as_secs(), duration.subsec_nanos())
            });
        hasher.update(seconds.to_le_bytes());
        hasher.update(nanos.to_le_bytes());
    }
    hasher.finalize().into()
}

fn nanoclaw_sqlite_access_error(path: &Path, error: SqliteSourceAccessError) -> CaptureError {
    if matches!(error, SqliteSourceAccessError::SourceChanged) {
        return CaptureError::SourceChangedDuringCapture;
    }
    CaptureError::ProviderSource {
        provider: "nanoclaw",
        path: path.to_path_buf(),
        kind: ProviderSourceFailureKind::SourceDatabase,
        detail: error.to_string(),
    }
}

fn nanoclaw_hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}

fn nanoclaw_hash_u64(hasher: &mut Sha256, value: u64) {
    nanoclaw_hash_bytes(hasher, &value.to_be_bytes());
}

fn nanoclaw_hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    match value {
        Some(value) => {
            nanoclaw_hash_bytes(hasher, &[1]);
            nanoclaw_hash_u64(hasher, value);
        }
        None => nanoclaw_hash_bytes(hasher, &[0]),
    }
}

fn nanoclaw_hash_optional_file(hasher: &mut Sha256, value: Option<&NanoClawFrozenFileMetadata>) {
    match value {
        Some(value) => {
            nanoclaw_hash_bytes(hasher, &[1]);
            value.update_hash(hasher);
        }
        None => nanoclaw_hash_bytes(hasher, &[0]),
    }
}

fn nanoclaw_hash_optional_sqlite(hasher: &mut Sha256, snapshot: Option<&NanoClawSqliteSnapshot>) {
    match snapshot {
        Some(snapshot) => {
            nanoclaw_hash_bytes(hasher, &[1]);
            snapshot.update_hash(hasher);
        }
        None => nanoclaw_hash_bytes(hasher, &[0]),
    }
}

fn nanoclaw_hex(value: &[u8]) -> String {
    use std::fmt::Write;

    let mut encoded = String::with_capacity(value.len().saturating_mul(2));
    for byte in value {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

pub(super) fn nanoclaw_project_root(path: &Path) -> Result<PathBuf> {
    if path.is_dir() && path.join("data").join("v2.db").is_file() {
        return Ok(path.to_path_buf());
    }
    if path.file_name().and_then(|name| name.to_str()) == Some("v2.db") {
        if let Some(data_dir) = path.parent() {
            if let Some(root) = data_dir.parent() {
                return Ok(root.to_path_buf());
            }
        }
    }
    Err(CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason: "NanoClaw import path must be a project root or data/v2.db",
    })
}

fn nanoclaw_requested_project_root(path: &Path) -> Result<PathBuf> {
    let root = if path.file_name().and_then(|name| name.to_str()) == Some("v2.db") {
        path.parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "NanoClaw data/v2.db has no project root",
            })?
    } else {
        path.to_path_buf()
    };
    if root.is_absolute() {
        Ok(root)
    } else {
        Ok(std::env::current_dir()?.join(root))
    }
}
