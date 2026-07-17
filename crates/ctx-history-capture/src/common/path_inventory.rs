use std::{
    ffi::OsStr,
    fs::{self, ReadDir},
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::common::io::{
    FilesystemTraversalBudget, FilesystemTraversalCursor, FilesystemTraversalErrorRecovery,
};
use crate::common::scratch::{
    CaptureScratchSpace, DurableCaptureScratch, DurableScratchCleanupOutcome,
};
use crate::Result;

const PATH_INSERT_BATCH: usize = 64;
const SCRATCH_PATH_READ_BYTES_PER_ROW: u64 = 8 * 1024;
const SCRATCH_PATH_READ_BASE_BYTES: u64 = 4 * 1024;
const DURABLE_INVENTORY_FORMAT_VERSION: u32 = 1;
const DURABLE_INVENTORY_APPLICATION_ID: i64 = 0x4354_5849;
const DURABLE_INVENTORY_DATABASE_NAME: &str = "inventory.sqlite";
const DURABLE_INVENTORY_MAX_ID_BYTES: usize = 1024;
const DURABLE_INVENTORY_PAGE_ENTRIES: usize = 64;
const DURABLE_INVENTORY_SLICE_MAX_ELAPSED: Duration = Duration::from_millis(25);
const DURABLE_INVENTORY_RETRY_BASE_MS: i64 = 250;
const DURABLE_INVENTORY_RETRY_MAX_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourcePathInventorySlice {
    pub complete: bool,
    pub operations: u64,
    pub path_bytes: u64,
    pub discovered_files: usize,
}

pub struct BoundedSourcePathInventory {
    cursor: FilesystemTraversalCursor,
    storage: Option<PathInventoryStorage>,
    pending_paths: Vec<(Vec<u8>, Vec<u8>)>,
    metrics: SortedPathInventoryMetrics,
    restart_required: bool,
}

#[derive(Debug, Default)]
pub struct SourcePathInventoryPage {
    pub paths: Vec<PathBuf>,
    pub next_cursor: Option<Vec<u8>>,
    pub complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableSourceInventoryMode {
    Jsonl,
    RegularFiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativePathPlatform {
    Unix,
    Windows,
}

impl NativePathPlatform {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unix => "unix",
            Self::Windows => "windows",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativePathEncoding {
    UnixBytes,
    WindowsUtf16Be,
}

impl NativePathEncoding {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnixBytes => "unix_bytes",
            Self::WindowsUtf16Be => "windows_utf16be",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePathIdentity {
    pub platform: NativePathPlatform,
    pub encoding: NativePathEncoding,
    pub sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSourceInventoryRequest {
    pub build_identity: Vec<u8>,
    pub run_id: Vec<u8>,
    pub source_id: Vec<u8>,
    pub generation: u64,
    pub root: PathBuf,
    pub mode: DurableSourceInventoryMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSourceInventoryCheckpoint {
    pub format_version: u32,
    pub build_identity: Vec<u8>,
    pub run_id: Vec<u8>,
    pub source_id: Vec<u8>,
    pub generation: u64,
    pub mode: DurableSourceInventoryMode,
    pub scratch_identity: Vec<u8>,
    pub scratch_integrity: [u8; 32],
    pub scratch_lock_identity: Vec<u8>,
    pub root_identity: NativePathIdentity,
    pub root_object_identity: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSourceInventoryOwner {
    pub epoch: u64,
    pub token: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSourceInventoryScratch {
    pub identity: Vec<u8>,
    pub integrity: [u8; 32],
    pub lock_identity: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSourceInventoryDirectoryAuthority {
    pub path: NativePathIdentity,
    pub directory_identity: Vec<u8>,
    pub directory_fingerprint: [u8; 32],
    pub scratch: DurableSourceInventoryScratch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSourceInventoryActiveDirectory {
    pub authority: DurableSourceInventoryDirectoryAuthority,
    pub attempt_count: u64,
    pub replay_count: u64,
    pub observed_entries: u64,
    pub next_retry_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSourceInventoryOpenState {
    pub checkpoint: DurableSourceInventoryCheckpoint,
    pub scratch: DurableSourceInventoryScratch,
    pub current_owner: Option<DurableSourceInventoryOwner>,
    pub active_directory: Option<DurableSourceInventoryActiveDirectory>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableSourceInventoryPhase {
    Traversal,
    Selection,
    Effects,
    Complete,
    Abandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableSourceInventoryFailureKind {
    OpenDirectory,
    ReadDirectory,
    ScratchWrite,
    SourceChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSourceInventoryStatus {
    pub phase: DurableSourceInventoryPhase,
    pub queued_directories: u64,
    pub completed_directories: u64,
    pub active_directory: Option<DurableSourceInventoryActiveDirectory>,
    pub active_observed_entries: u64,
    pub replay_high_water_entries: u64,
    pub discovered_files: u64,
    pub selected_files: u64,
    pub selection_complete: bool,
    pub pending_effects: u64,
    pub replay_count: u64,
    pub next_retry_at_ms: Option<i64>,
    pub scratch: DurableSourceInventoryScratch,
    pub scratch_bytes: u64,
    pub error: Option<DurableSourceInventoryFailureKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSourceInventoryJournalEntry {
    pub journal_identity: [u8; 32],
    pub path_identity: NativePathIdentity,
    pub directory: DurableSourceInventoryDirectoryAuthority,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DurableSourceInventoryJournalPage {
    pub entries: Vec<DurableSourceInventoryJournalEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DurableSourceInventoryPathPage {
    pub entries: Vec<DurableSourceInventoryJournalEntry>,
    pub next_keyset: Option<Vec<u8>>,
    pub complete: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DurableSourceInventorySlice {
    pub observed_entries: u64,
    pub discovered_files: u64,
    pub selected_files: u64,
    pub selection_complete: bool,
    pub queued_directories: u64,
    pub traversal_complete: bool,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSourceInventoryCompletionProof {
    pub checkpoint: DurableSourceInventoryCheckpoint,
    pub owner: DurableSourceInventoryOwner,
    pub scratch: DurableSourceInventoryScratch,
    pub discovered_files: u64,
    pub selected_files: u64,
    pub completed_directories: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableSourceInventoryCleanupOutcome {
    Complete,
    Pending,
    Busy,
}

#[derive(Debug, thiserror::Error)]
pub enum DurableSourceInventoryError {
    #[error("durable source inventory request is invalid: {0}")]
    InvalidRequest(&'static str),
    #[error("durable source inventory scratch is already owned")]
    Locked,
    #[error("durable source inventory scratch is missing")]
    MissingScratch,
    #[error("durable source inventory scratch is corrupt")]
    CorruptScratch,
    #[error("durable source inventory scratch identity does not match its checkpoint")]
    TamperedScratch,
    #[error("durable source inventory checkpoint does not match the requested run")]
    CheckpointMismatch,
    #[error("durable source inventory owner epoch or token is stale")]
    StaleOwner,
    #[error("durable source inventory source root changed identity")]
    SourceChanged,
    #[error("durable source inventory was abandoned")]
    Abandoned,
    #[error("durable source inventory filesystem operation failed")]
    Filesystem(#[source] io::Error),
    #[error("durable source inventory scratch operation failed")]
    Scratch(#[source] rusqlite::Error),
}

type DurableInventoryResult<T> = std::result::Result<T, DurableSourceInventoryError>;

impl std::fmt::Debug for BoundedSourcePathInventory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BoundedSourcePathInventory")
            .field("discovered_files", &self.metrics.paths)
            .finish_non_exhaustive()
    }
}

impl BoundedSourcePathInventory {
    pub fn new(root: &Path) -> Self {
        Self {
            cursor: FilesystemTraversalCursor::regular_files(root),
            storage: None,
            pending_paths: Vec::with_capacity(PATH_INSERT_BATCH),
            metrics: SortedPathInventoryMetrics::default(),
            restart_required: false,
        }
    }

    pub fn new_jsonl(root: &Path) -> Self {
        Self {
            cursor: FilesystemTraversalCursor::jsonl(root),
            storage: None,
            pending_paths: Vec::with_capacity(PATH_INSERT_BATCH),
            metrics: SortedPathInventoryMetrics::default(),
            restart_required: false,
        }
    }

    pub fn advance(&mut self) -> Result<SourcePathInventorySlice> {
        self.advance_with_budget(FilesystemTraversalBudget::default())
    }

    fn advance_with_budget(
        &mut self,
        budget: FilesystemTraversalBudget,
    ) -> Result<SourcePathInventorySlice> {
        if self.restart_required {
            self.restart_after_traversal_failure()?;
        }
        if self.storage.is_none() {
            self.storage = Some(PathInventoryStorage::create()?);
            return Ok(SourcePathInventorySlice::default());
        }
        if !self.pending_paths.is_empty() {
            return self.flush_pending_paths();
        }
        let slice = match self.cursor.advance(budget, &mut |path| {
            let encoded = encode_path(path);
            self.pending_paths.push((encoded.clone(), encoded));
            Ok(())
        }) {
            Ok(slice) => slice,
            Err(error) => {
                if self.cursor.error_recovery() == FilesystemTraversalErrorRecovery::RestartRequired
                {
                    self.restart_required = true;
                }
                return Err(error);
            }
        };
        self.metrics.max_in_memory_batch = self
            .metrics
            .max_in_memory_batch
            .max(self.pending_paths.len());
        let persisted = self.flush_pending_paths()?;
        Ok(SourcePathInventorySlice {
            complete: persisted.complete,
            operations: slice.operations.saturating_add(persisted.operations),
            path_bytes: slice.path_bytes.saturating_add(persisted.path_bytes),
            discovered_files: persisted.discovered_files,
        })
    }

    fn flush_pending_paths(&mut self) -> Result<SourcePathInventorySlice> {
        let write_bytes = path_batch_write_bytes(&self.pending_paths);
        if !self.pending_paths.is_empty() {
            crate::pace_current_filesystem_operation(write_bytes);
        }
        let storage = self
            .storage
            .as_mut()
            .ok_or(crate::CaptureError::SystemInvariant(
                "path inventory storage is missing",
            ))?;
        self.metrics.paths = self.metrics.paths.saturating_add(flush_paths(
            &mut storage.connection,
            &mut self.pending_paths,
        )?);
        Ok(SourcePathInventorySlice {
            complete: self.cursor.is_complete(),
            operations: u64::from(write_bytes > 0),
            path_bytes: write_bytes,
            discovered_files: self.metrics.paths,
        })
    }

    fn restart_after_traversal_failure(&mut self) -> Result<()> {
        let replacement = PathInventoryStorage::create()?;
        self.cursor.restart();
        self.storage = Some(replacement);
        self.pending_paths.clear();
        self.metrics = SortedPathInventoryMetrics::default();
        self.restart_required = false;
        Ok(())
    }

    pub fn paths_page(
        &self,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<SourcePathInventoryPage> {
        self.ready_storage()?.paths_page("paths", after, limit)
    }

    pub fn selected_paths_page(
        &self,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<SourcePathInventoryPage> {
        self.ready_storage()?
            .paths_page("selected_paths", after, limit)
    }

    pub fn contains_path(&self, path: &Path) -> Result<bool> {
        self.ready_storage()?.contains_path("paths", path)
    }

    pub fn contains_selected_path(&self, path: &Path) -> Result<bool> {
        self.ready_storage()?.contains_path("selected_paths", path)
    }

    pub fn select_path_candidates(&mut self, candidates: &[(Vec<u8>, u64, PathBuf)]) -> Result<()> {
        if candidates.len() > PATH_INSERT_BATCH {
            return Err(crate::CaptureError::SystemInvariant(
                "source path selection page exceeds its internal row limit",
            ));
        }
        if candidates.is_empty() {
            return Ok(());
        }
        let write_bytes = candidates.iter().fold(0_u64, |total, (group, _, path)| {
            total
                .saturating_add(group.len() as u64)
                .saturating_add(path.as_os_str().len() as u64)
                .saturating_add(96)
        });
        crate::pace_current_filesystem_operation(write_bytes);
        crate::pace_current_disk_io(write_bytes);
        let storage = self.ready_storage()?;
        storage.connection.execute_batch("BEGIN IMMEDIATE")?;
        let selected = (|| -> Result<()> {
            let mut statement = storage.connection.prepare(
                "INSERT INTO selected_paths (group_key, rank, sort_key, path)
                 VALUES (?1, ?2, ?3, ?3)
                 ON CONFLICT(group_key) DO UPDATE SET
                     rank = excluded.rank,
                     sort_key = excluded.sort_key,
                     path = excluded.path
                 WHERE excluded.rank < selected_paths.rank
                    OR (excluded.rank = selected_paths.rank
                        AND excluded.sort_key < selected_paths.sort_key)",
            )?;
            for (group_key, rank, path) in candidates {
                let encoded_path = encode_path(path);
                statement.execute(params![
                    group_key,
                    i64::try_from(*rank).unwrap_or(i64::MAX),
                    encoded_path
                ])?;
            }
            Ok(())
        })();
        match selected {
            Ok(()) => storage.connection.execute_batch("COMMIT")?,
            Err(error) => {
                let _ = storage.connection.execute_batch("ROLLBACK");
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn metrics(&self) -> SourcePathInventorySlice {
        SourcePathInventorySlice {
            complete: self.cursor.is_complete(),
            discovered_files: self.metrics.paths,
            ..SourcePathInventorySlice::default()
        }
    }

    fn ready_storage(&self) -> Result<&PathInventoryStorage> {
        if !self.cursor.is_complete() {
            return Err(crate::CaptureError::SystemInvariant(
                "source path inventory was read before traversal completed",
            ));
        }
        self.storage
            .as_ref()
            .ok_or(crate::CaptureError::SystemInvariant(
                "source path inventory storage is missing",
            ))
    }
}

const DURABLE_INVENTORY_SCHEMA: &str = "
    PRAGMA journal_mode = DELETE;
    PRAGMA synchronous = FULL;
    PRAGMA foreign_keys = ON;
    PRAGMA trusted_schema = OFF;
    PRAGMA application_id = 1129601097;
    PRAGMA user_version = 1;
    CREATE TABLE inventory_meta (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        format_version INTEGER NOT NULL,
        build_identity BLOB NOT NULL,
        run_id BLOB NOT NULL,
        source_id BLOB NOT NULL,
        generation INTEGER NOT NULL,
        mode INTEGER NOT NULL,
        scratch_nonce BLOB NOT NULL,
        scratch_identity BLOB NOT NULL,
        scratch_integrity BLOB NOT NULL,
        scratch_lock_identity BLOB NOT NULL,
        root_platform INTEGER NOT NULL,
        root_encoding INTEGER NOT NULL,
        root_path BLOB NOT NULL,
        root_path_sha256 BLOB NOT NULL,
        root_object_identity BLOB NOT NULL,
        owner_epoch INTEGER,
        owner_token BLOB,
        phase INTEGER NOT NULL,
        active_directory_identity BLOB,
        active_observed_entries INTEGER NOT NULL,
        replay_high_water_entries INTEGER NOT NULL,
        queued_directories INTEGER NOT NULL,
        completed_directories INTEGER NOT NULL,
        discovered_files INTEGER NOT NULL,
        selected_files INTEGER NOT NULL,
        selection_complete INTEGER NOT NULL,
        pending_effects INTEGER NOT NULL,
        replay_count INTEGER NOT NULL,
        next_retry_at_ms INTEGER,
        last_error INTEGER,
        traversal_complete INTEGER NOT NULL,
        complete INTEGER NOT NULL,
        CHECK ((owner_epoch IS NULL AND owner_token IS NULL)
            OR (owner_epoch > 0 AND length(owner_token) BETWEEN 16 AND 64))
    );
    CREATE TABLE directory_queue (
        sequence INTEGER PRIMARY KEY,
        path_identity BLOB UNIQUE NOT NULL,
        platform INTEGER NOT NULL,
        encoding INTEGER NOT NULL,
        path BLOB NOT NULL,
        object_identity BLOB NOT NULL,
        directory_fingerprint BLOB NOT NULL,
        state INTEGER NOT NULL,
        attempt_count INTEGER NOT NULL,
        replay_count INTEGER NOT NULL,
        next_retry_at_ms INTEGER
    );
    CREATE INDEX directory_queue_state_sequence
        ON directory_queue(state, sequence);
    CREATE TABLE path_journal (
        sequence INTEGER PRIMARY KEY,
        journal_identity BLOB UNIQUE NOT NULL,
        path_identity BLOB UNIQUE NOT NULL,
        platform INTEGER NOT NULL,
        encoding INTEGER NOT NULL,
        path BLOB NOT NULL,
        directory_identity BLOB NOT NULL REFERENCES directory_queue(path_identity),
        state INTEGER NOT NULL
    );
    CREATE INDEX path_journal_state_sequence
        ON path_journal(state, sequence);
    CREATE UNIQUE INDEX path_journal_path ON path_journal(path);
    CREATE TABLE selected_paths (
        group_key BLOB PRIMARY KEY NOT NULL,
        rank INTEGER NOT NULL,
        sort_key BLOB UNIQUE NOT NULL,
        path_identity BLOB UNIQUE NOT NULL REFERENCES path_journal(path_identity)
    ) WITHOUT ROWID;
    CREATE INDEX selected_paths_sort_key ON selected_paths(sort_key);
";

struct ActiveDirectoryReader {
    path_identity: [u8; 32],
    entries: ReadDir,
}

struct ActiveDirectoryRecord {
    path_identity: [u8; 32],
    path: NativePathDescriptor,
    object_identity: Vec<u8>,
    fingerprint: [u8; 32],
    attempt_count: u64,
    replay_count: u64,
    next_retry_at_ms: Option<i64>,
}

enum ActiveDirectoryPreparation {
    Ready(ActiveDirectoryRecord),
    Waiting,
    TraversalComplete,
}

struct DurableInventoryMeta {
    checkpoint: DurableSourceInventoryCheckpoint,
    scratch_nonce: [u8; 16],
    owner: Option<DurableSourceInventoryOwner>,
    root_path: Vec<u8>,
    phase: DurableSourceInventoryPhase,
    active_directory_identity: Option<[u8; 32]>,
    active_observed_entries: u64,
    replay_high_water_entries: u64,
    queued_directories: u64,
    completed_directories: u64,
    discovered_files: u64,
    selected_files: u64,
    selection_complete: bool,
    pending_effects: u64,
    replay_count: u64,
    next_retry_at_ms: Option<i64>,
    last_error: Option<DurableSourceInventoryFailureKind>,
    traversal_complete: bool,
    complete: bool,
}

struct NativePathDescriptor {
    identity: NativePathIdentity,
    encoded: Vec<u8>,
}

enum RootKind {
    Directory,
    File,
}

struct RootObservation {
    kind: RootKind,
    path: NativePathDescriptor,
    object_identity: Vec<u8>,
}

enum DurableDirectoryObservation {
    Directory {
        path: NativePathDescriptor,
        object_identity: Vec<u8>,
    },
    File {
        path: NativePathDescriptor,
        journal_identity: [u8; 32],
    },
}

pub struct DurableSourcePathInventory {
    connection: Option<Connection>,
    scratch: Option<DurableCaptureScratch>,
    data_root: PathBuf,
    scratch_name: String,
    request: DurableSourceInventoryRequest,
    checkpoint: DurableSourceInventoryCheckpoint,
    owner: Option<DurableSourceInventoryOwner>,
    active_reader: Option<ActiveDirectoryReader>,
}

impl std::fmt::Debug for DurableSourcePathInventory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DurableSourcePathInventory")
            .field("format_version", &self.checkpoint.format_version)
            .field("generation", &self.checkpoint.generation)
            .field("owner_epoch", &self.owner.as_ref().map(|owner| owner.epoch))
            .finish_non_exhaustive()
    }
}

impl DurableSourcePathInventory {
    pub fn create(
        data_root: &Path,
        request: DurableSourceInventoryRequest,
    ) -> DurableInventoryResult<Self> {
        validate_durable_request(data_root, &request)?;
        let root = observe_inventory_root(&request.root)?;
        let scratch_name = durable_inventory_scratch_name(&request, &root.path.identity);
        let scratch =
            DurableCaptureScratch::create(data_root, &scratch_name, "durable-path-inventory")
                .map_err(map_durable_scratch_create_error)?;
        let scratch_identity = scratch
            .directory_identity()
            .map_err(DurableSourceInventoryError::Filesystem)?;
        let scratch_lock_identity = scratch
            .lock_identity()
            .map_err(DurableSourceInventoryError::Filesystem)?;
        drop(
            scratch
                .create_file(DURABLE_INVENTORY_DATABASE_NAME)
                .map_err(DurableSourceInventoryError::Filesystem)?,
        );
        let mut connection = Connection::open(scratch.path().join(DURABLE_INVENTORY_DATABASE_NAME))
            .map_err(DurableSourceInventoryError::Scratch)?;
        connection
            .execute_batch(DURABLE_INVENTORY_SCHEMA)
            .map_err(DurableSourceInventoryError::Scratch)?;

        let scratch_nonce = random_inventory_token();
        let scratch_integrity = durable_scratch_integrity(
            &request,
            &root,
            &scratch_nonce,
            &scratch_identity,
            &scratch_lock_identity,
        );
        let checkpoint = DurableSourceInventoryCheckpoint {
            format_version: DURABLE_INVENTORY_FORMAT_VERSION,
            build_identity: request.build_identity.clone(),
            run_id: request.run_id.clone(),
            source_id: request.source_id.clone(),
            generation: request.generation,
            mode: request.mode,
            scratch_identity,
            scratch_integrity,
            scratch_lock_identity,
            root_identity: root.path.identity.clone(),
            root_object_identity: root.object_identity.clone(),
        };
        initialize_durable_inventory(&mut connection, &request, &checkpoint, &scratch_nonce, root)?;
        Ok(Self {
            connection: Some(connection),
            scratch: Some(scratch),
            data_root: data_root.to_path_buf(),
            scratch_name,
            request,
            checkpoint,
            owner: None,
            active_reader: None,
        })
    }

    pub fn open(
        data_root: &Path,
        request: DurableSourceInventoryRequest,
        checkpoint: &DurableSourceInventoryCheckpoint,
    ) -> DurableInventoryResult<Self> {
        validate_durable_request(data_root, &request)?;
        let requested_root = native_path_descriptor(&request.root)?;
        let scratch_name = durable_inventory_scratch_name(&request, &requested_root.identity);
        let scratch = DurableCaptureScratch::open(data_root, &scratch_name)
            .map_err(map_durable_scratch_open_error)?;
        let current_scratch_identity = scratch
            .directory_identity()
            .map_err(DurableSourceInventoryError::Filesystem)?;
        let current_scratch_lock_identity = scratch
            .lock_identity()
            .map_err(DurableSourceInventoryError::Filesystem)?;
        if current_scratch_identity != checkpoint.scratch_identity {
            return Err(DurableSourceInventoryError::TamperedScratch);
        }
        if current_scratch_lock_identity != checkpoint.scratch_lock_identity {
            return Err(DurableSourceInventoryError::TamperedScratch);
        }
        let database_path = scratch.path().join(DURABLE_INVENTORY_DATABASE_NAME);
        validate_durable_database_path(&database_path)?;
        let connection = Connection::open_with_flags(
            database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)?;
        validate_durable_inventory_schema(&connection)?;
        let meta = read_durable_meta(&connection)?;
        validate_resume_contract(&request, checkpoint, &requested_root, &meta)?;
        if meta.phase == DurableSourceInventoryPhase::Abandoned {
            return Err(DurableSourceInventoryError::Abandoned);
        }
        let current_root = observe_inventory_root(&request.root)?;
        if current_root.object_identity != checkpoint.root_object_identity {
            return Err(DurableSourceInventoryError::SourceChanged);
        }
        let recomputed_integrity = durable_scratch_integrity(
            &request,
            &current_root,
            &meta.scratch_nonce,
            &current_scratch_identity,
            &current_scratch_lock_identity,
        );
        if recomputed_integrity != checkpoint.scratch_integrity {
            return Err(DurableSourceInventoryError::TamperedScratch);
        }
        maybe_fail_durable_inventory(DurableInventoryFailurePoint::OpenAfterValidation)?;
        Ok(Self {
            connection: Some(connection),
            scratch: Some(scratch),
            data_root: data_root.to_path_buf(),
            scratch_name,
            request,
            checkpoint: checkpoint.clone(),
            owner: meta.owner,
            active_reader: None,
        })
    }

    pub fn checkpoint(&self) -> &DurableSourceInventoryCheckpoint {
        &self.checkpoint
    }

    pub fn open_state(&self) -> DurableInventoryResult<DurableSourceInventoryOpenState> {
        let meta = read_durable_meta(self.connection()?)?;
        let active_directory = read_active_directory(self.connection()?, &self.checkpoint, &meta)?;
        Ok(DurableSourceInventoryOpenState {
            checkpoint: self.checkpoint.clone(),
            scratch: self.scratch_state(),
            current_owner: meta.owner,
            active_directory,
        })
    }

    pub fn adopt_owner(
        &mut self,
        expected: Option<&DurableSourceInventoryOwner>,
        next: DurableSourceInventoryOwner,
    ) -> DurableInventoryResult<()> {
        validate_external_owner(&next)?;
        match expected {
            Some(previous) if next.epoch <= previous.epoch => {
                return Err(DurableSourceInventoryError::InvalidRequest(
                    "next owner epoch must advance the store-issued owner",
                ));
            }
            None if next.epoch == 0 => {
                return Err(DurableSourceInventoryError::InvalidRequest(
                    "owner epoch must be positive",
                ));
            }
            _ => {}
        }
        if self.owner.as_ref() != expected {
            return Err(DurableSourceInventoryError::StaleOwner);
        }
        let transaction = self
            .connection_mut()?
            .transaction()
            .map_err(DurableSourceInventoryError::Scratch)?;
        let changed = match expected {
            Some(previous) => transaction.execute(
                "UPDATE inventory_meta SET owner_epoch = ?1, owner_token = ?2
                 WHERE singleton = 1 AND owner_epoch = ?3 AND owner_token = ?4",
                params![
                    owner_epoch_i64(next.epoch)?,
                    next.token.as_slice(),
                    owner_epoch_i64(previous.epoch)?,
                    previous.token.as_slice(),
                ],
            ),
            None => transaction.execute(
                "UPDATE inventory_meta SET owner_epoch = ?1, owner_token = ?2
                 WHERE singleton = 1 AND owner_epoch IS NULL AND owner_token IS NULL",
                params![owner_epoch_i64(next.epoch)?, next.token.as_slice()],
            ),
        }
        .map_err(DurableSourceInventoryError::Scratch)?;
        if changed != 1 {
            return Err(DurableSourceInventoryError::StaleOwner);
        }
        transaction
            .commit()
            .map_err(DurableSourceInventoryError::Scratch)?;
        self.owner = Some(next);
        maybe_fail_durable_inventory(DurableInventoryFailurePoint::OwnerAdoptionAfterCommit)
    }

    pub fn advance(
        &mut self,
        owner: &DurableSourceInventoryOwner,
    ) -> DurableInventoryResult<DurableSourceInventorySlice> {
        assert_durable_owner(self.connection()?, owner)?;
        if read_durable_meta(self.connection()?)?.complete {
            return self.current_slice();
        }
        if self.active_reader.is_none() {
            match prepare_active_directory(self.connection_mut()?, owner)? {
                ActiveDirectoryPreparation::Ready(active) => {
                    if active.fingerprint
                        != durable_directory_fingerprint(
                            &self.checkpoint,
                            &active.path_identity,
                            &active.object_identity,
                        )
                    {
                        return Err(DurableSourceInventoryError::CorruptScratch);
                    }
                    let active_path = active.path.path()?;
                    if !active_path.starts_with(&self.request.root) {
                        return Err(DurableSourceInventoryError::TamperedScratch);
                    }
                    let current = observe_directory(&active_path)?;
                    if current.object_identity != active.object_identity {
                        persist_durable_failure(
                            self.connection_mut()?,
                            owner,
                            DurableSourceInventoryFailureKind::SourceChanged,
                        )?;
                        return Err(DurableSourceInventoryError::SourceChanged);
                    }
                    crate::pace_current_filesystem_operation(active_path.as_os_str().len() as u64);
                    let entries = match fs::read_dir(current.path.path()?) {
                        Ok(entries) => entries,
                        Err(error) => {
                            persist_durable_failure(
                                self.connection_mut()?,
                                owner,
                                DurableSourceInventoryFailureKind::OpenDirectory,
                            )?;
                            return Err(DurableSourceInventoryError::Filesystem(error));
                        }
                    };
                    clear_active_retry(self.connection_mut()?, owner, &active.path_identity)?;
                    self.active_reader = Some(ActiveDirectoryReader {
                        path_identity: active.path_identity,
                        entries,
                    });
                }
                ActiveDirectoryPreparation::Waiting => return self.current_slice(),
                ActiveDirectoryPreparation::TraversalComplete => return self.current_slice(),
            }
        }

        let started = Instant::now();
        let mut observations = Vec::with_capacity(DURABLE_INVENTORY_PAGE_ENTRIES);
        let mut observed_entries = 0_u64;
        let mut exhausted = false;
        while observations.len() < DURABLE_INVENTORY_PAGE_ENTRIES
            && observed_entries < DURABLE_INVENTORY_PAGE_ENTRIES as u64
            && started.elapsed() < DURABLE_INVENTORY_SLICE_MAX_ELAPSED
        {
            crate::pace_current_filesystem_operation(0);
            let next = self
                .active_reader
                .as_mut()
                .ok_or(DurableSourceInventoryError::CorruptScratch)?
                .entries
                .next();
            let Some(entry) = next else {
                exhausted = true;
                break;
            };
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.active_reader = None;
                    persist_durable_failure(
                        self.connection_mut()?,
                        owner,
                        DurableSourceInventoryFailureKind::ReadDirectory,
                    )?;
                    return Err(DurableSourceInventoryError::Filesystem(error));
                }
            };
            observed_entries = observed_entries.saturating_add(1);
            match observe_directory_entry(&entry, &self.request) {
                Ok(Some(observation)) => observations.push(observation),
                Ok(None) => {}
                Err(error) => {
                    self.active_reader = None;
                    persist_durable_failure(
                        self.connection_mut()?,
                        owner,
                        DurableSourceInventoryFailureKind::SourceChanged,
                    )?;
                    return Err(error);
                }
            }
        }

        let active_identity = self
            .active_reader
            .as_ref()
            .ok_or(DurableSourceInventoryError::CorruptScratch)?
            .path_identity;
        let checkpoint = self.checkpoint.clone();
        let flush = flush_active_directory_page(
            self.connection_mut()?,
            owner,
            &checkpoint,
            &active_identity,
            &observations,
            observed_entries,
            exhausted,
        );
        if let Err(error) = flush {
            self.active_reader = None;
            let _ = persist_durable_failure(
                self.connection_mut()?,
                owner,
                DurableSourceInventoryFailureKind::ScratchWrite,
            );
            return Err(error);
        }
        if exhausted {
            self.active_reader = None;
            maybe_fail_durable_inventory(
                DurableInventoryFailurePoint::DirectoryCompleteAfterCommit,
            )?;
        }
        self.current_slice()
    }

    pub fn status(&self) -> DurableInventoryResult<DurableSourceInventoryStatus> {
        let meta = read_durable_meta(self.connection()?)?;
        Ok(DurableSourceInventoryStatus {
            phase: meta.phase,
            queued_directories: meta.queued_directories,
            completed_directories: meta.completed_directories,
            active_directory: read_active_directory(self.connection()?, &self.checkpoint, &meta)?,
            active_observed_entries: meta.active_observed_entries,
            replay_high_water_entries: meta.replay_high_water_entries,
            discovered_files: meta.discovered_files,
            selected_files: meta.selected_files,
            selection_complete: meta.selection_complete,
            pending_effects: meta.pending_effects,
            replay_count: meta.replay_count,
            next_retry_at_ms: meta.next_retry_at_ms,
            scratch: self.scratch_state(),
            scratch_bytes: durable_inventory_scratch_bytes(
                self.scratch
                    .as_ref()
                    .ok_or(DurableSourceInventoryError::CorruptScratch)?
                    .path(),
            )?,
            error: meta.last_error,
        })
    }

    pub fn paths_page(
        &self,
        owner: &DurableSourceInventoryOwner,
        after: Option<&[u8]>,
        limit: usize,
    ) -> DurableInventoryResult<DurableSourceInventoryPathPage> {
        let connection = self.connection()?;
        assert_durable_owner(connection, owner)?;
        if !read_durable_meta(connection)?.traversal_complete {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        read_durable_path_page(
            connection,
            &self.request,
            &self.checkpoint,
            after,
            limit,
            false,
        )
    }

    pub fn select_path_candidates(
        &mut self,
        owner: &DurableSourceInventoryOwner,
        candidates: &[(Vec<u8>, u64, PathBuf)],
    ) -> DurableInventoryResult<()> {
        if candidates.len() > DURABLE_INVENTORY_PAGE_ENTRIES {
            return Err(DurableSourceInventoryError::InvalidRequest(
                "path selection page exceeds the internal row bound",
            ));
        }
        let write_bytes = candidates
            .iter()
            .try_fold(0_u64, |total, (group, _, path)| {
                if group.is_empty() || group.len() > DURABLE_INVENTORY_MAX_ID_BYTES {
                    return Err(DurableSourceInventoryError::InvalidRequest(
                        "path selection group identity is not bounded",
                    ));
                }
                Ok(total
                    .saturating_add(group.len() as u64)
                    .saturating_add(path.as_os_str().len() as u64)
                    .saturating_add(128))
            })?;
        if write_bytes > 0 {
            crate::pace_current_filesystem_operation(write_bytes);
            crate::pace_current_disk_io(write_bytes);
        }
        let connection = self.connection_mut()?;
        let transaction = connection
            .transaction()
            .map_err(DurableSourceInventoryError::Scratch)?;
        assert_durable_owner_transaction(&transaction, owner)?;
        let (mode, traversal_complete, selection_complete): (i64, i64, i64) = transaction
            .query_row(
                "SELECT mode, traversal_complete, selection_complete
                 FROM inventory_meta WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(DurableSourceInventoryError::Scratch)?;
        if decode_mode(mode)? != DurableSourceInventoryMode::RegularFiles
            || !decode_bool(traversal_complete)?
            || decode_bool(selection_complete)?
        {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        let mut inserted_groups = 0_u64;
        for (group_key, rank, path) in candidates {
            let encoded_path = encode_path(path);
            let (platform, encoding) = native_path_tags()?;
            let path_identity = native_path_identity_hash(platform, encoding, &encoded_path);
            let stored: Option<(i64, i64, Vec<u8>)> = transaction
                .query_row(
                    "SELECT platform, encoding, path FROM path_journal
                     WHERE path_identity = ?1",
                    params![path_identity.as_slice()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(DurableSourceInventoryError::Scratch)?;
            if stored
                != Some((
                    encode_platform(platform),
                    encode_encoding(encoding),
                    encoded_path.clone(),
                ))
            {
                return Err(DurableSourceInventoryError::CheckpointMismatch);
            }
            let rank = i64::try_from(*rank).map_err(|_| {
                DurableSourceInventoryError::InvalidRequest("path selection rank exceeds SQLite")
            })?;
            let existing = transaction
                .query_row(
                    "SELECT rank, sort_key FROM selected_paths WHERE group_key = ?1",
                    params![group_key],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .optional()
                .map_err(DurableSourceInventoryError::Scratch)?;
            match existing {
                None => {
                    transaction
                        .execute(
                            "INSERT INTO selected_paths (group_key, rank, sort_key, path_identity)
                             VALUES (?1, ?2, ?3, ?4)",
                            params![group_key, rank, encoded_path, path_identity.as_slice()],
                        )
                        .map_err(DurableSourceInventoryError::Scratch)?;
                    inserted_groups = inserted_groups.saturating_add(1);
                }
                Some((current_rank, current_path))
                    if rank < current_rank
                        || (rank == current_rank && encoded_path < current_path) =>
                {
                    transaction
                        .execute(
                            "UPDATE selected_paths
                             SET rank = ?1, sort_key = ?2, path_identity = ?3
                             WHERE group_key = ?4",
                            params![rank, encoded_path, path_identity.as_slice(), group_key],
                        )
                        .map_err(DurableSourceInventoryError::Scratch)?;
                }
                Some(_) => {}
            }
        }
        if inserted_groups > 0 {
            let changed = transaction
                .execute(
                    "UPDATE inventory_meta SET selected_files = selected_files + ?1
                     WHERE singleton = 1",
                    params![u64_i64(inserted_groups)?],
                )
                .map_err(DurableSourceInventoryError::Scratch)?;
            if changed != 1 {
                return Err(DurableSourceInventoryError::CorruptScratch);
            }
        }
        transaction
            .commit()
            .map_err(DurableSourceInventoryError::Scratch)
    }

    pub fn complete_path_selection(
        &mut self,
        owner: &DurableSourceInventoryOwner,
    ) -> DurableInventoryResult<()> {
        let transaction = self
            .connection_mut()?
            .transaction()
            .map_err(DurableSourceInventoryError::Scratch)?;
        assert_durable_owner_transaction(&transaction, owner)?;
        let (mode, traversal_complete, selection_complete): (i64, i64, i64) = transaction
            .query_row(
                "SELECT mode, traversal_complete, selection_complete
                 FROM inventory_meta WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(DurableSourceInventoryError::Scratch)?;
        if decode_mode(mode)? != DurableSourceInventoryMode::RegularFiles
            || !decode_bool(traversal_complete)?
        {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        if decode_bool(selection_complete)? {
            transaction
                .commit()
                .map_err(DurableSourceInventoryError::Scratch)?;
            return Ok(());
        }
        let changed = transaction
            .execute(
                "UPDATE inventory_meta
                 SET selection_complete = 1, pending_effects = selected_files,
                     phase = CASE WHEN selected_files = 0 THEN 3 ELSE 2 END,
                     complete = CASE WHEN selected_files = 0 THEN 1 ELSE 0 END
                 WHERE singleton = 1 AND mode = 2 AND traversal_complete = 1
                   AND selection_complete = 0 AND active_directory_identity IS NULL
                   AND queued_directories = 0",
                [],
            )
            .map_err(DurableSourceInventoryError::Scratch)?;
        if changed != 1 {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        transaction
            .commit()
            .map_err(DurableSourceInventoryError::Scratch)
    }

    pub fn selected_paths_page(
        &self,
        owner: &DurableSourceInventoryOwner,
        after: Option<&[u8]>,
        limit: usize,
    ) -> DurableInventoryResult<DurableSourceInventoryPathPage> {
        let connection = self.connection()?;
        assert_durable_owner(connection, owner)?;
        let meta = read_durable_meta(connection)?;
        if self.request.mode != DurableSourceInventoryMode::RegularFiles || !meta.selection_complete
        {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        read_durable_path_page(
            connection,
            &self.request,
            &self.checkpoint,
            after,
            limit,
            true,
        )
    }

    pub fn contains_selected_path(
        &self,
        owner: &DurableSourceInventoryOwner,
        path: &Path,
    ) -> DurableInventoryResult<bool> {
        let connection = self.connection()?;
        assert_durable_owner(connection, owner)?;
        let meta = read_durable_meta(connection)?;
        if self.request.mode != DurableSourceInventoryMode::RegularFiles || !meta.selection_complete
        {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        let encoded = encode_path(path);
        crate::pace_current_disk_io(
            SCRATCH_PATH_READ_BASE_BYTES.saturating_add(encoded.len() as u64),
        );
        connection
            .query_row(
                "SELECT 1 FROM selected_paths WHERE sort_key = ?1",
                params![encoded],
                |_| Ok(()),
            )
            .optional()
            .map(|value| value.is_some())
            .map_err(DurableSourceInventoryError::Scratch)
    }

    pub fn next_effects_page(
        &self,
        owner: &DurableSourceInventoryOwner,
    ) -> DurableInventoryResult<DurableSourceInventoryJournalPage> {
        let connection = self.connection()?;
        assert_durable_owner(connection, owner)?;
        if self.request.mode == DurableSourceInventoryMode::RegularFiles
            && !read_durable_meta(connection)?.selection_complete
        {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        crate::pace_current_disk_io(SCRATCH_PATH_READ_BASE_BYTES.saturating_add(
            SCRATCH_PATH_READ_BYTES_PER_ROW.saturating_mul(DURABLE_INVENTORY_PAGE_ENTRIES as u64),
        ));
        let sql = if self.request.mode == DurableSourceInventoryMode::RegularFiles {
            "SELECT j.journal_identity, j.path_identity, j.platform, j.encoding, j.path,
                    d.path_identity, d.platform, d.encoding, d.object_identity,
                    d.directory_fingerprint
             FROM selected_paths AS s
             JOIN path_journal AS j ON j.path_identity = s.path_identity
             JOIN directory_queue AS d ON d.path_identity = j.directory_identity
             WHERE j.state = 0 ORDER BY s.sort_key LIMIT 64"
        } else {
            "SELECT j.journal_identity, j.path_identity, j.platform, j.encoding, j.path,
                    d.path_identity, d.platform, d.encoding, d.object_identity,
                    d.directory_fingerprint
             FROM path_journal AS j
             JOIN directory_queue AS d ON d.path_identity = j.directory_identity
             WHERE j.state = 0 ORDER BY j.sequence LIMIT 64"
        };
        let mut statement = connection
            .prepare(sql)
            .map_err(DurableSourceInventoryError::Scratch)?;
        let mut rows = statement
            .query([])
            .map_err(DurableSourceInventoryError::Scratch)?;
        let mut entries = Vec::with_capacity(DURABLE_INVENTORY_PAGE_ENTRIES);
        while let Some(row) = rows.next().map_err(DurableSourceInventoryError::Scratch)? {
            entries.push(decode_durable_journal_entry(
                row,
                &self.request,
                &self.checkpoint,
            )?);
        }
        Ok(DurableSourceInventoryJournalPage { entries })
    }

    pub fn acknowledge_effects(
        &mut self,
        owner: &DurableSourceInventoryOwner,
        journal_identities: &[[u8; 32]],
    ) -> DurableInventoryResult<()> {
        if journal_identities.len() > DURABLE_INVENTORY_PAGE_ENTRIES {
            return Err(DurableSourceInventoryError::InvalidRequest(
                "effect acknowledgement exceeds the internal page bound",
            ));
        }
        let mode = self.request.mode;
        let connection = self.connection_mut()?;
        let transaction = connection
            .transaction()
            .map_err(DurableSourceInventoryError::Scratch)?;
        assert_durable_owner_transaction(&transaction, owner)?;
        let mut acknowledged = 0_u64;
        for identity in journal_identities {
            if mode == DurableSourceInventoryMode::RegularFiles {
                let selected = transaction
                    .query_row(
                        "SELECT EXISTS(
                           SELECT 1 FROM selected_paths AS s
                           JOIN path_journal AS j ON j.path_identity = s.path_identity
                           WHERE j.journal_identity = ?1
                         )",
                        params![identity.as_slice()],
                        |row| row.get::<_, bool>(0),
                    )
                    .map_err(DurableSourceInventoryError::Scratch)?;
                if !selected {
                    return Err(DurableSourceInventoryError::CheckpointMismatch);
                }
            }
            let changed = transaction
                .execute(
                    "UPDATE path_journal SET state = 1
                     WHERE journal_identity = ?1 AND state = 0",
                    params![identity.as_slice()],
                )
                .map_err(DurableSourceInventoryError::Scratch)?;
            if changed == 1 {
                acknowledged = acknowledged.saturating_add(1);
                continue;
            }
            let state = transaction
                .query_row(
                    "SELECT state FROM path_journal WHERE journal_identity = ?1",
                    params![identity.as_slice()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(DurableSourceInventoryError::Scratch)?;
            if state != Some(1) {
                return Err(DurableSourceInventoryError::CheckpointMismatch);
            }
        }
        if acknowledged > 0 {
            let changed = transaction
                .execute(
                    "UPDATE inventory_meta
                     SET pending_effects = pending_effects - ?1
                     WHERE singleton = 1 AND pending_effects >= ?1",
                    params![u64_i64(acknowledged)?],
                )
                .map_err(DurableSourceInventoryError::Scratch)?;
            if changed != 1 {
                return Err(DurableSourceInventoryError::CorruptScratch);
            }
        }
        publish_complete_if_ready(&transaction, owner)?;
        transaction
            .commit()
            .map_err(DurableSourceInventoryError::Scratch)
    }

    pub fn completion_proof(
        &self,
        owner: &DurableSourceInventoryOwner,
    ) -> DurableInventoryResult<Option<DurableSourceInventoryCompletionProof>> {
        assert_durable_owner(self.connection()?, owner)?;
        let meta = read_durable_meta(self.connection()?)?;
        if !meta.complete
            || !meta.traversal_complete
            || !meta.selection_complete
            || meta.active_directory_identity.is_some()
            || meta.queued_directories != 0
            || meta.pending_effects != 0
        {
            return Ok(None);
        }
        let root = observe_inventory_root(&self.request.root)?;
        if root.path.identity != self.checkpoint.root_identity
            || root.object_identity != self.checkpoint.root_object_identity
        {
            return Err(DurableSourceInventoryError::SourceChanged);
        }
        Ok(Some(DurableSourceInventoryCompletionProof {
            checkpoint: self.checkpoint.clone(),
            owner: owner.clone(),
            scratch: self.scratch_state(),
            discovered_files: meta.discovered_files,
            selected_files: meta.selected_files,
            completed_directories: meta.completed_directories,
        }))
    }

    pub fn abandon(
        mut self,
        owner: &DurableSourceInventoryOwner,
    ) -> DurableInventoryResult<DurableSourceInventoryCleanupOutcome> {
        {
            let connection = self.connection_mut()?;
            let transaction = connection
                .transaction()
                .map_err(DurableSourceInventoryError::Scratch)?;
            assert_durable_owner_transaction(&transaction, owner)?;
            transaction
                .execute(
                    "UPDATE inventory_meta SET phase = 4, complete = 0 WHERE singleton = 1",
                    [],
                )
                .map_err(DurableSourceInventoryError::Scratch)?;
            transaction
                .commit()
                .map_err(DurableSourceInventoryError::Scratch)?;
        }
        drop(self.connection.take());
        if let Some(scratch) = self.scratch.take() {
            scratch
                .release()
                .map_err(DurableSourceInventoryError::Filesystem)?;
        }
        cleanup_durable_inventory_scratch(&self.data_root, &self.scratch_name)
    }

    pub fn cleanup_abandoned(
        data_root: &Path,
        request: &DurableSourceInventoryRequest,
    ) -> DurableInventoryResult<DurableSourceInventoryCleanupOutcome> {
        validate_durable_request(data_root, request)?;
        let root = native_path_descriptor(&request.root)?;
        let scratch_name = durable_inventory_scratch_name(request, &root.identity);
        cleanup_durable_inventory_scratch(data_root, &scratch_name)
    }

    fn connection(&self) -> DurableInventoryResult<&Connection> {
        self.connection
            .as_ref()
            .ok_or(DurableSourceInventoryError::CorruptScratch)
    }

    fn connection_mut(&mut self) -> DurableInventoryResult<&mut Connection> {
        self.connection
            .as_mut()
            .ok_or(DurableSourceInventoryError::CorruptScratch)
    }

    fn current_slice(&self) -> DurableInventoryResult<DurableSourceInventorySlice> {
        let meta = read_durable_meta(self.connection()?)?;
        Ok(DurableSourceInventorySlice {
            observed_entries: meta.active_observed_entries,
            discovered_files: meta.discovered_files,
            selected_files: meta.selected_files,
            selection_complete: meta.selection_complete,
            queued_directories: meta.queued_directories,
            traversal_complete: meta.traversal_complete,
            complete: meta.complete,
        })
    }

    fn scratch_state(&self) -> DurableSourceInventoryScratch {
        DurableSourceInventoryScratch {
            identity: self.checkpoint.scratch_identity.clone(),
            integrity: self.checkpoint.scratch_integrity,
            lock_identity: self.checkpoint.scratch_lock_identity.clone(),
        }
    }
}

impl NativePathDescriptor {
    fn path(&self) -> DurableInventoryResult<PathBuf> {
        decode_native_path(
            self.identity.platform,
            self.identity.encoding,
            self.encoded.clone(),
        )
    }
}

fn validate_durable_request(
    data_root: &Path,
    request: &DurableSourceInventoryRequest,
) -> DurableInventoryResult<()> {
    if !data_root.is_absolute() || !request.root.is_absolute() {
        return Err(DurableSourceInventoryError::InvalidRequest(
            "data root and source root must be absolute",
        ));
    }
    for value in [
        request.build_identity.as_slice(),
        request.run_id.as_slice(),
        request.source_id.as_slice(),
    ] {
        if value.is_empty() || value.len() > DURABLE_INVENTORY_MAX_ID_BYTES {
            return Err(DurableSourceInventoryError::InvalidRequest(
                "build, run, and source identities must be bounded non-empty bytes",
            ));
        }
    }
    let _ = owner_epoch_i64(request.generation)?;
    Ok(())
}

fn initialize_durable_inventory(
    connection: &mut Connection,
    request: &DurableSourceInventoryRequest,
    checkpoint: &DurableSourceInventoryCheckpoint,
    scratch_nonce: &[u8; 16],
    root: RootObservation,
) -> DurableInventoryResult<()> {
    let transaction = connection
        .transaction()
        .map_err(DurableSourceInventoryError::Scratch)?;
    let (
        phase,
        queued_directories,
        completed_directories,
        discovered_files,
        pending_effects,
        traversal_complete,
    ) = match root.kind {
        RootKind::Directory => (1_i64, 1_i64, 0_i64, 0_i64, 0_i64, 0_i64),
        RootKind::File => (
            2_i64,
            0_i64,
            1_i64,
            1_i64,
            i64::from(request.mode == DurableSourceInventoryMode::Jsonl),
            1_i64,
        ),
    };
    let selection_complete = i64::from(request.mode == DurableSourceInventoryMode::Jsonl);
    let root_fingerprint = durable_directory_fingerprint(
        checkpoint,
        &root.path.identity.sha256,
        &root.object_identity,
    );
    transaction
        .execute(
            "INSERT INTO inventory_meta (
                singleton, format_version, build_identity, run_id, source_id, generation,
                mode, scratch_nonce, scratch_identity, scratch_integrity,
                scratch_lock_identity, root_platform, root_encoding, root_path,
                root_path_sha256, root_object_identity, owner_epoch, owner_token, phase,
                active_directory_identity, active_observed_entries, replay_high_water_entries,
                queued_directories,
                completed_directories, discovered_files, selected_files, selection_complete,
                pending_effects, replay_count,
                next_retry_at_ms, last_error, traversal_complete, complete
             ) VALUES (
                1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, NULL, NULL, ?16, NULL, 0, 0, ?17, ?18, ?19, 0, ?20, ?21, 0,
                NULL, NULL, ?22, 0
             )",
            params![
                i64::from(DURABLE_INVENTORY_FORMAT_VERSION),
                request.build_identity,
                request.run_id,
                request.source_id,
                u64_i64(request.generation)?,
                encode_mode(request.mode),
                scratch_nonce.as_slice(),
                checkpoint.scratch_identity,
                checkpoint.scratch_integrity.as_slice(),
                checkpoint.scratch_lock_identity,
                encode_platform(root.path.identity.platform),
                encode_encoding(root.path.identity.encoding),
                root.path.encoded,
                root.path.identity.sha256.as_slice(),
                root.object_identity,
                phase,
                queued_directories,
                completed_directories,
                discovered_files,
                selection_complete,
                pending_effects,
                traversal_complete,
            ],
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    let directory_state = match root.kind {
        RootKind::Directory => 0_i64,
        RootKind::File => 2_i64,
    };
    transaction
        .execute(
            "INSERT INTO directory_queue (
                path_identity, platform, encoding, path, object_identity,
                directory_fingerprint, state, attempt_count, replay_count,
                next_retry_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 0, NULL)",
            params![
                root.path.identity.sha256.as_slice(),
                encode_platform(root.path.identity.platform),
                encode_encoding(root.path.identity.encoding),
                root.path.encoded,
                checkpoint.root_object_identity,
                root_fingerprint.as_slice(),
                directory_state,
            ],
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    match root.kind {
        RootKind::Directory => {}
        RootKind::File => {
            let journal_identity = journal_identity(request, &root.path.identity.sha256);
            transaction
                .execute(
                    "INSERT INTO path_journal (
                        journal_identity, path_identity, platform, encoding, path,
                        directory_identity, state
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
                    params![
                        journal_identity.as_slice(),
                        root.path.identity.sha256.as_slice(),
                        encode_platform(root.path.identity.platform),
                        encode_encoding(root.path.identity.encoding),
                        root.path.encoded,
                        root.path.identity.sha256.as_slice(),
                    ],
                )
                .map_err(DurableSourceInventoryError::Scratch)?;
        }
    }
    transaction
        .commit()
        .map_err(DurableSourceInventoryError::Scratch)
}

fn validate_durable_database_path(path: &Path) -> DurableInventoryResult<()> {
    crate::pace_current_filesystem_operation(path.as_os_str().len() as u64);
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            DurableSourceInventoryError::MissingScratch
        } else {
            DurableSourceInventoryError::Filesystem(error)
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(DurableSourceInventoryError::TamperedScratch);
    }
    Ok(())
}

fn validate_durable_inventory_schema(connection: &Connection) -> DurableInventoryResult<()> {
    let application_id = connection
        .query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)?;
    let user_version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)?;
    let table_count = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type = 'table'
               AND name IN ('inventory_meta', 'directory_queue', 'path_journal', 'selected_paths')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)?;
    if application_id != DURABLE_INVENTORY_APPLICATION_ID
        || user_version != i64::from(DURABLE_INVENTORY_FORMAT_VERSION)
        || table_count != 4
    {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
    Ok(())
}

fn read_durable_meta(connection: &Connection) -> DurableInventoryResult<DurableInventoryMeta> {
    connection
        .query_row(
            "SELECT
                format_version, build_identity, run_id, source_id, generation, mode,
                scratch_nonce, scratch_identity, scratch_integrity, scratch_lock_identity,
                root_platform, root_encoding, root_path, root_path_sha256,
                root_object_identity, owner_epoch, owner_token, phase,
                active_directory_identity, active_observed_entries, replay_high_water_entries,
                queued_directories,
                completed_directories, discovered_files, selected_files, selection_complete,
                pending_effects, replay_count,
                next_retry_at_ms, last_error, traversal_complete, complete
             FROM inventory_meta WHERE singleton = 1",
            [],
            |row| {
                let format_version = row.get::<_, i64>(0)?;
                let generation = row.get::<_, i64>(4)?;
                Ok((
                    format_version,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    generation,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Vec<u8>>(6)?,
                    row.get::<_, Vec<u8>>(7)?,
                    row.get::<_, Vec<u8>>(8)?,
                    row.get::<_, Vec<u8>>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, Vec<u8>>(12)?,
                    row.get::<_, Vec<u8>>(13)?,
                    row.get::<_, Vec<u8>>(14)?,
                    row.get::<_, Option<i64>>(15)?,
                    row.get::<_, Option<Vec<u8>>>(16)?,
                    row.get::<_, i64>(17)?,
                    row.get::<_, Option<Vec<u8>>>(18)?,
                    row.get::<_, i64>(19)?,
                    row.get::<_, i64>(20)?,
                    row.get::<_, i64>(21)?,
                    row.get::<_, i64>(22)?,
                    row.get::<_, i64>(23)?,
                    row.get::<_, i64>(24)?,
                    row.get::<_, i64>(25)?,
                    row.get::<_, i64>(26)?,
                    row.get::<_, i64>(27)?,
                    row.get::<_, Option<i64>>(28)?,
                    row.get::<_, Option<i64>>(29)?,
                    row.get::<_, i64>(30)?,
                    row.get::<_, i64>(31)?,
                ))
            },
        )
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)
        .and_then(|row| {
            let (
                format_version,
                build_identity,
                run_id,
                source_id,
                generation,
                mode,
                scratch_nonce,
                scratch_identity,
                scratch_integrity,
                scratch_lock_identity,
                root_platform,
                root_encoding,
                root_path,
                root_path_sha256,
                root_object_identity,
                owner_epoch,
                owner_token,
                phase,
                active_directory_identity,
                active_observed_entries,
                replay_high_water_entries,
                queued_directories,
                completed_directories,
                discovered_files,
                selected_files,
                selection_complete,
                pending_effects,
                replay_count,
                next_retry_at_ms,
                last_error,
                traversal_complete,
                complete,
            ) = row;
            let owner = match (owner_epoch, owner_token) {
                (None, None) => None,
                (Some(epoch), Some(token)) => {
                    let owner = DurableSourceInventoryOwner {
                        epoch: nonnegative_u64(epoch)?,
                        token,
                    };
                    validate_external_owner(&owner)?;
                    Some(owner)
                }
                _ => return Err(DurableSourceInventoryError::CorruptScratch),
            };
            let platform = decode_platform(root_platform)?;
            let encoding = decode_encoding(root_encoding)?;
            Ok(DurableInventoryMeta {
                checkpoint: DurableSourceInventoryCheckpoint {
                    format_version: nonnegative_u32(format_version)?,
                    build_identity,
                    run_id,
                    source_id,
                    generation: nonnegative_u64(generation)?,
                    mode: decode_mode(mode)?,
                    scratch_identity,
                    scratch_integrity: fixed_identity(scratch_integrity)?,
                    scratch_lock_identity,
                    root_identity: NativePathIdentity {
                        platform,
                        encoding,
                        sha256: fixed_identity(root_path_sha256)?,
                    },
                    root_object_identity,
                },
                scratch_nonce: fixed_token(scratch_nonce)?,
                owner,
                root_path,
                phase: decode_phase(phase)?,
                active_directory_identity: active_directory_identity
                    .map(fixed_identity)
                    .transpose()?,
                active_observed_entries: nonnegative_u64(active_observed_entries)?,
                replay_high_water_entries: nonnegative_u64(replay_high_water_entries)?,
                queued_directories: nonnegative_u64(queued_directories)?,
                completed_directories: nonnegative_u64(completed_directories)?,
                discovered_files: nonnegative_u64(discovered_files)?,
                selected_files: nonnegative_u64(selected_files)?,
                selection_complete: decode_bool(selection_complete)?,
                pending_effects: nonnegative_u64(pending_effects)?,
                replay_count: nonnegative_u64(replay_count)?,
                next_retry_at_ms,
                last_error: last_error.map(decode_failure_kind).transpose()?,
                traversal_complete: decode_bool(traversal_complete)?,
                complete: decode_bool(complete)?,
            })
        })
}

fn validate_resume_contract(
    request: &DurableSourceInventoryRequest,
    checkpoint: &DurableSourceInventoryCheckpoint,
    requested_root: &NativePathDescriptor,
    meta: &DurableInventoryMeta,
) -> DurableInventoryResult<()> {
    if checkpoint.format_version != DURABLE_INVENTORY_FORMAT_VERSION
        || &meta.checkpoint != checkpoint
        || request.build_identity != checkpoint.build_identity
        || request.run_id != checkpoint.run_id
        || request.source_id != checkpoint.source_id
        || request.generation != checkpoint.generation
        || request.mode != checkpoint.mode
        || requested_root.identity != checkpoint.root_identity
        || requested_root.encoded != meta.root_path
    {
        return Err(DurableSourceInventoryError::CheckpointMismatch);
    }
    Ok(())
}

fn read_active_directory(
    connection: &Connection,
    checkpoint: &DurableSourceInventoryCheckpoint,
    meta: &DurableInventoryMeta,
) -> DurableInventoryResult<Option<DurableSourceInventoryActiveDirectory>> {
    let Some(identity) = meta.active_directory_identity else {
        return Ok(None);
    };
    let record = read_directory_record(connection, &identity, 1)?;
    let expected_fingerprint =
        durable_directory_fingerprint(checkpoint, &record.path_identity, &record.object_identity);
    if record.fingerprint != expected_fingerprint
        || record.next_retry_at_ms != meta.next_retry_at_ms
    {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
    Ok(Some(DurableSourceInventoryActiveDirectory {
        authority: DurableSourceInventoryDirectoryAuthority {
            path: record.path.identity,
            directory_identity: record.object_identity,
            directory_fingerprint: record.fingerprint,
            scratch: checkpoint_scratch_state(checkpoint),
        },
        attempt_count: record.attempt_count,
        replay_count: record.replay_count,
        observed_entries: meta.active_observed_entries,
        next_retry_at_ms: record.next_retry_at_ms,
    }))
}

fn checkpoint_scratch_state(
    checkpoint: &DurableSourceInventoryCheckpoint,
) -> DurableSourceInventoryScratch {
    DurableSourceInventoryScratch {
        identity: checkpoint.scratch_identity.clone(),
        integrity: checkpoint.scratch_integrity,
        lock_identity: checkpoint.scratch_lock_identity.clone(),
    }
}

fn decode_durable_journal_entry(
    row: &rusqlite::Row<'_>,
    request: &DurableSourceInventoryRequest,
    checkpoint: &DurableSourceInventoryCheckpoint,
) -> DurableInventoryResult<DurableSourceInventoryJournalEntry> {
    let journal_id = fixed_identity(row.get(0).map_err(DurableSourceInventoryError::Scratch)?)?;
    let path_sha256 = fixed_identity(row.get(1).map_err(DurableSourceInventoryError::Scratch)?)?;
    let platform = decode_platform(row.get(2).map_err(DurableSourceInventoryError::Scratch)?)?;
    let encoding = decode_encoding(row.get(3).map_err(DurableSourceInventoryError::Scratch)?)?;
    let path_bytes: Vec<u8> = row.get(4).map_err(DurableSourceInventoryError::Scratch)?;
    let directory_sha256 =
        fixed_identity(row.get(5).map_err(DurableSourceInventoryError::Scratch)?)?;
    let directory_platform =
        decode_platform(row.get(6).map_err(DurableSourceInventoryError::Scratch)?)?;
    let directory_encoding =
        decode_encoding(row.get(7).map_err(DurableSourceInventoryError::Scratch)?)?;
    let directory_identity: Vec<u8> = row.get(8).map_err(DurableSourceInventoryError::Scratch)?;
    let directory_fingerprint =
        fixed_identity(row.get(9).map_err(DurableSourceInventoryError::Scratch)?)?;
    if native_path_identity_hash(platform, encoding, &path_bytes) != path_sha256
        || journal_identity(request, &path_sha256) != journal_id
        || directory_fingerprint
            != durable_directory_fingerprint(checkpoint, &directory_sha256, &directory_identity)
    {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
    Ok(DurableSourceInventoryJournalEntry {
        journal_identity: journal_id,
        path_identity: NativePathIdentity {
            platform,
            encoding,
            sha256: path_sha256,
        },
        directory: DurableSourceInventoryDirectoryAuthority {
            path: NativePathIdentity {
                platform: directory_platform,
                encoding: directory_encoding,
                sha256: directory_sha256,
            },
            directory_identity,
            directory_fingerprint,
            scratch: checkpoint_scratch_state(checkpoint),
        },
        path: decode_native_path(platform, encoding, path_bytes)?,
    })
}

fn read_durable_path_page(
    connection: &Connection,
    request: &DurableSourceInventoryRequest,
    checkpoint: &DurableSourceInventoryCheckpoint,
    after: Option<&[u8]>,
    limit: usize,
    selected: bool,
) -> DurableInventoryResult<DurableSourceInventoryPathPage> {
    let row_limit = limit.clamp(1, DURABLE_INVENTORY_PAGE_ENTRIES);
    crate::pace_current_disk_io(
        SCRATCH_PATH_READ_BASE_BYTES
            .saturating_add(SCRATCH_PATH_READ_BYTES_PER_ROW.saturating_mul(row_limit as u64)),
    );
    let sql = match (selected, after.is_some()) {
        (true, true) => {
            "SELECT j.journal_identity, j.path_identity, j.platform, j.encoding, j.path,
                    d.path_identity, d.platform, d.encoding, d.object_identity,
                    d.directory_fingerprint, s.sort_key
             FROM selected_paths AS s
             JOIN path_journal AS j ON j.path_identity = s.path_identity
             JOIN directory_queue AS d ON d.path_identity = j.directory_identity
             WHERE s.sort_key > ?1 ORDER BY s.sort_key LIMIT ?2"
        }
        (true, false) => {
            "SELECT j.journal_identity, j.path_identity, j.platform, j.encoding, j.path,
                    d.path_identity, d.platform, d.encoding, d.object_identity,
                    d.directory_fingerprint, s.sort_key
             FROM selected_paths AS s
             JOIN path_journal AS j ON j.path_identity = s.path_identity
             JOIN directory_queue AS d ON d.path_identity = j.directory_identity
             ORDER BY s.sort_key LIMIT ?1"
        }
        (false, true) => {
            "SELECT j.journal_identity, j.path_identity, j.platform, j.encoding, j.path,
                    d.path_identity, d.platform, d.encoding, d.object_identity,
                    d.directory_fingerprint, j.path
             FROM path_journal AS j
             JOIN directory_queue AS d ON d.path_identity = j.directory_identity
             WHERE j.path > ?1 ORDER BY j.path LIMIT ?2"
        }
        (false, false) => {
            "SELECT j.journal_identity, j.path_identity, j.platform, j.encoding, j.path,
                    d.path_identity, d.platform, d.encoding, d.object_identity,
                    d.directory_fingerprint, j.path
             FROM path_journal AS j
             JOIN directory_queue AS d ON d.path_identity = j.directory_identity
             ORDER BY j.path LIMIT ?1"
        }
    };
    let mut statement = connection
        .prepare(sql)
        .map_err(DurableSourceInventoryError::Scratch)?;
    let sql_limit = i64::try_from(row_limit).unwrap_or(i64::MAX);
    let mut rows = match after {
        Some(keyset) => statement.query(params![keyset, sql_limit]),
        None => statement.query(params![sql_limit]),
    }
    .map_err(DurableSourceInventoryError::Scratch)?;
    let mut entries = Vec::with_capacity(row_limit);
    let mut next_keyset = None;
    while let Some(row) = rows.next().map_err(DurableSourceInventoryError::Scratch)? {
        entries.push(decode_durable_journal_entry(row, request, checkpoint)?);
        next_keyset = Some(row.get(10).map_err(DurableSourceInventoryError::Scratch)?);
    }
    Ok(DurableSourceInventoryPathPage {
        complete: entries.len() < row_limit,
        entries,
        next_keyset,
    })
}

fn validate_external_owner(owner: &DurableSourceInventoryOwner) -> DurableInventoryResult<()> {
    if owner.epoch == 0 || owner.token.len() < 16 || owner.token.len() > 64 {
        return Err(DurableSourceInventoryError::InvalidRequest(
            "store-issued owner must have a positive epoch and a bounded token",
        ));
    }
    let _ = owner_epoch_i64(owner.epoch)?;
    Ok(())
}

fn observe_inventory_root(root: &Path) -> DurableInventoryResult<RootObservation> {
    crate::common::io::ensure_provider_path_parents_are_not_symlinks(root)
        .map_err(|_| DurableSourceInventoryError::SourceChanged)?;
    crate::pace_current_filesystem_operation(root.as_os_str().len() as u64);
    let metadata = fs::symlink_metadata(root).map_err(DurableSourceInventoryError::Filesystem)?;
    if metadata.file_type().is_symlink() {
        return Err(DurableSourceInventoryError::SourceChanged);
    }
    let kind = if metadata.file_type().is_dir() {
        RootKind::Directory
    } else if metadata.file_type().is_file() {
        crate::common::io::ensure_regular_provider_transcript_file(root)
            .map_err(|_| DurableSourceInventoryError::SourceChanged)?;
        RootKind::File
    } else {
        return Err(DurableSourceInventoryError::SourceChanged);
    };
    Ok(RootObservation {
        kind,
        path: native_path_descriptor(root)?,
        object_identity: native_object_identity(&metadata)?,
    })
}

fn observe_directory(path: &Path) -> DurableInventoryResult<RootObservation> {
    let observation = observe_inventory_root(path)?;
    if !matches!(observation.kind, RootKind::Directory) {
        return Err(DurableSourceInventoryError::SourceChanged);
    }
    Ok(observation)
}

fn native_path_descriptor(path: &Path) -> DurableInventoryResult<NativePathDescriptor> {
    let (platform, encoding) = native_path_tags()?;
    let encoded = encode_path(path);
    if encoded.is_empty() {
        return Err(DurableSourceInventoryError::InvalidRequest(
            "native path encoding is empty or unsupported",
        ));
    }
    Ok(NativePathDescriptor {
        identity: NativePathIdentity {
            platform,
            encoding,
            sha256: native_path_identity_hash(platform, encoding, &encoded),
        },
        encoded,
    })
}

#[cfg(unix)]
fn native_path_tags() -> DurableInventoryResult<(NativePathPlatform, NativePathEncoding)> {
    Ok((NativePathPlatform::Unix, NativePathEncoding::UnixBytes))
}

#[cfg(windows)]
fn native_path_tags() -> DurableInventoryResult<(NativePathPlatform, NativePathEncoding)> {
    Ok((
        NativePathPlatform::Windows,
        NativePathEncoding::WindowsUtf16Be,
    ))
}

#[cfg(not(any(unix, windows)))]
fn native_path_tags() -> DurableInventoryResult<(NativePathPlatform, NativePathEncoding)> {
    Err(DurableSourceInventoryError::InvalidRequest(
        "durable native path identity is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn native_object_identity(metadata: &fs::Metadata) -> DurableInventoryResult<Vec<u8>> {
    use std::os::unix::fs::MetadataExt;

    let mut identity = b"unix-object-v1\0".to_vec();
    identity.extend_from_slice(&metadata.dev().to_be_bytes());
    identity.extend_from_slice(&metadata.ino().to_be_bytes());
    Ok(identity)
}

#[cfg(windows)]
fn native_object_identity(metadata: &fs::Metadata) -> DurableInventoryResult<Vec<u8>> {
    use std::os::windows::fs::MetadataExt;

    let volume = metadata
        .volume_serial_number()
        .ok_or(DurableSourceInventoryError::SourceChanged)?;
    let index = metadata
        .file_index()
        .ok_or(DurableSourceInventoryError::SourceChanged)?;
    let mut identity = b"windows-object-v1\0".to_vec();
    identity.extend_from_slice(&volume.to_be_bytes());
    identity.extend_from_slice(&index.to_be_bytes());
    Ok(identity)
}

#[cfg(not(any(unix, windows)))]
fn native_object_identity(_metadata: &fs::Metadata) -> DurableInventoryResult<Vec<u8>> {
    Err(DurableSourceInventoryError::InvalidRequest(
        "durable filesystem identity is unsupported on this platform",
    ))
}

fn durable_inventory_scratch_name(
    request: &DurableSourceInventoryRequest,
    root: &NativePathIdentity,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-durable-inventory-scratch-v1\0");
    hash_length_prefixed(&mut hasher, &request.build_identity);
    hash_length_prefixed(&mut hasher, &request.run_id);
    hash_length_prefixed(&mut hasher, &request.source_id);
    hasher.update(request.generation.to_be_bytes());
    hasher.update([encode_mode(request.mode) as u8]);
    hasher.update(root.sha256);
    format!("inventory-{}", hex_bytes(&hasher.finalize()))
}

fn journal_identity(request: &DurableSourceInventoryRequest, path_identity: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-durable-inventory-journal-v1\0");
    hash_length_prefixed(&mut hasher, &request.run_id);
    hash_length_prefixed(&mut hasher, &request.source_id);
    hasher.update(request.generation.to_be_bytes());
    hasher.update(path_identity);
    hasher.finalize().into()
}

fn hash_length_prefixed(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn hex_bytes(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(value.len().saturating_mul(2));
    for byte in value {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn random_inventory_token() -> [u8; 16] {
    *Uuid::new_v4().as_bytes()
}

fn durable_scratch_integrity(
    request: &DurableSourceInventoryRequest,
    root: &RootObservation,
    scratch_nonce: &[u8; 16],
    scratch_identity: &[u8],
    scratch_lock_identity: &[u8],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-durable-inventory-scratch-v1\0");
    hasher.update(DURABLE_INVENTORY_FORMAT_VERSION.to_be_bytes());
    hash_inventory_field(&mut hasher, &request.build_identity);
    hash_inventory_field(&mut hasher, &request.run_id);
    hash_inventory_field(&mut hasher, &request.source_id);
    hasher.update(request.generation.to_be_bytes());
    hasher.update([encode_mode(request.mode) as u8]);
    hasher.update(scratch_nonce);
    hash_inventory_field(&mut hasher, scratch_identity);
    hash_inventory_field(&mut hasher, scratch_lock_identity);
    hasher.update([encode_platform(root.path.identity.platform) as u8]);
    hasher.update([encode_encoding(root.path.identity.encoding) as u8]);
    hash_inventory_field(&mut hasher, &root.path.encoded);
    hash_inventory_field(&mut hasher, &root.object_identity);
    hasher.finalize().into()
}

fn durable_directory_fingerprint(
    checkpoint: &DurableSourceInventoryCheckpoint,
    path_identity: &[u8; 32],
    directory_identity: &[u8],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-durable-inventory-directory-v1\0");
    hash_inventory_field(&mut hasher, &checkpoint.build_identity);
    hash_inventory_field(&mut hasher, &checkpoint.run_id);
    hash_inventory_field(&mut hasher, &checkpoint.source_id);
    hasher.update(checkpoint.generation.to_be_bytes());
    hasher.update(path_identity);
    hash_inventory_field(&mut hasher, directory_identity);
    hasher.update(checkpoint.scratch_integrity);
    hasher.finalize().into()
}

fn hash_inventory_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn prepare_active_directory(
    connection: &mut Connection,
    owner: &DurableSourceInventoryOwner,
) -> DurableInventoryResult<ActiveDirectoryPreparation> {
    let meta = read_durable_meta(connection)?;
    if meta.complete || meta.traversal_complete {
        return Ok(ActiveDirectoryPreparation::TraversalComplete);
    }
    if let Some(active_identity) = meta.active_directory_identity {
        if meta.next_retry_at_ms.is_some_and(|retry| retry > now_ms()) {
            return Ok(ActiveDirectoryPreparation::Waiting);
        }
        let active = read_directory_record(connection, &active_identity, 1)?;
        let attempt = active
            .attempt_count
            .checked_add(1)
            .ok_or(DurableSourceInventoryError::CorruptScratch)?;
        let next_retry = now_ms().saturating_add(retry_delay_ms(attempt));
        let transaction = connection
            .transaction()
            .map_err(DurableSourceInventoryError::Scratch)?;
        assert_durable_owner_transaction(&transaction, owner)?;
        let changed = transaction
            .execute(
                "UPDATE directory_queue
                 SET attempt_count = ?1, replay_count = replay_count + 1,
                     next_retry_at_ms = ?2
                 WHERE path_identity = ?3 AND state = 1",
                params![u64_i64(attempt)?, next_retry, active_identity.as_slice()],
            )
            .map_err(DurableSourceInventoryError::Scratch)?;
        if changed != 1 {
            return Err(DurableSourceInventoryError::CorruptScratch);
        }
        transaction
            .execute(
                "UPDATE inventory_meta
                 SET replay_count = replay_count + 1, next_retry_at_ms = ?1,
                     last_error = NULL
                 WHERE singleton = 1",
                params![next_retry],
            )
            .map_err(DurableSourceInventoryError::Scratch)?;
        transaction
            .commit()
            .map_err(DurableSourceInventoryError::Scratch)?;
        return Ok(ActiveDirectoryPreparation::Ready(active));
    }

    let queued = read_first_queued_directory(connection)?;
    let Some(active) = queued else {
        if meta.queued_directories != 0 {
            return Err(DurableSourceInventoryError::CorruptScratch);
        }
        let transaction = connection
            .transaction()
            .map_err(DurableSourceInventoryError::Scratch)?;
        assert_durable_owner_transaction(&transaction, owner)?;
        publish_traversal_complete_if_ready(&transaction, owner)?;
        transaction
            .commit()
            .map_err(DurableSourceInventoryError::Scratch)?;
        return Ok(ActiveDirectoryPreparation::TraversalComplete);
    };
    let attempt = active
        .attempt_count
        .checked_add(1)
        .ok_or(DurableSourceInventoryError::CorruptScratch)?;
    let next_retry = now_ms().saturating_add(retry_delay_ms(attempt));
    let transaction = connection
        .transaction()
        .map_err(DurableSourceInventoryError::Scratch)?;
    assert_durable_owner_transaction(&transaction, owner)?;
    let changed = transaction
        .execute(
            "UPDATE directory_queue
             SET state = 1, attempt_count = ?1, next_retry_at_ms = ?2
             WHERE path_identity = ?3 AND state = 0",
            params![
                u64_i64(attempt)?,
                next_retry,
                active.path_identity.as_slice(),
            ],
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    if changed != 1 {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
    transaction
        .execute(
            "UPDATE inventory_meta
             SET active_directory_identity = ?1, active_observed_entries = 0,
                 queued_directories = queued_directories - 1, next_retry_at_ms = ?2,
                 last_error = NULL
             WHERE singleton = 1 AND queued_directories > 0",
            params![active.path_identity.as_slice(), next_retry],
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    transaction
        .commit()
        .map_err(DurableSourceInventoryError::Scratch)?;
    maybe_fail_durable_inventory(DurableInventoryFailurePoint::ActiveTransitionAfterCommit)?;
    Ok(ActiveDirectoryPreparation::Ready(active))
}

fn read_first_queued_directory(
    connection: &Connection,
) -> DurableInventoryResult<Option<ActiveDirectoryRecord>> {
    connection
        .query_row(
            "SELECT path_identity, platform, encoding, path, object_identity,
                    directory_fingerprint, attempt_count, replay_count, next_retry_at_ms
             FROM directory_queue WHERE state = 0 ORDER BY sequence LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                ))
            },
        )
        .optional()
        .map_err(DurableSourceInventoryError::Scratch)?
        .map(decode_directory_record)
        .transpose()
}

fn read_directory_record(
    connection: &Connection,
    identity: &[u8; 32],
    expected_state: i64,
) -> DurableInventoryResult<ActiveDirectoryRecord> {
    connection
        .query_row(
            "SELECT path_identity, platform, encoding, path, object_identity,
                    directory_fingerprint, attempt_count, replay_count, next_retry_at_ms
             FROM directory_queue WHERE path_identity = ?1 AND state = ?2",
            params![identity.as_slice(), expected_state],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                ))
            },
        )
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)
        .and_then(decode_directory_record)
}

fn decode_directory_record(
    row: (
        Vec<u8>,
        i64,
        i64,
        Vec<u8>,
        Vec<u8>,
        Vec<u8>,
        i64,
        i64,
        Option<i64>,
    ),
) -> DurableInventoryResult<ActiveDirectoryRecord> {
    let (
        identity,
        platform,
        encoding,
        path,
        object_identity,
        fingerprint,
        attempts,
        replays,
        next_retry_at_ms,
    ) = row;
    let platform = decode_platform(platform)?;
    let encoding = decode_encoding(encoding)?;
    let stored_identity = fixed_identity(identity)?;
    if native_path_identity_hash(platform, encoding, &path) != stored_identity {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
    Ok(ActiveDirectoryRecord {
        path_identity: stored_identity,
        path: NativePathDescriptor {
            identity: NativePathIdentity {
                platform,
                encoding,
                sha256: stored_identity,
            },
            encoded: path,
        },
        object_identity,
        fingerprint: fixed_identity(fingerprint)?,
        attempt_count: nonnegative_u64(attempts)?,
        replay_count: nonnegative_u64(replays)?,
        next_retry_at_ms,
    })
}

fn clear_active_retry(
    connection: &mut Connection,
    owner: &DurableSourceInventoryOwner,
    identity: &[u8; 32],
) -> DurableInventoryResult<()> {
    let transaction = connection
        .transaction()
        .map_err(DurableSourceInventoryError::Scratch)?;
    assert_durable_owner_transaction(&transaction, owner)?;
    transaction
        .execute(
            "UPDATE directory_queue SET next_retry_at_ms = NULL
             WHERE path_identity = ?1 AND state = 1",
            params![identity.as_slice()],
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    transaction
        .execute(
            "UPDATE inventory_meta
             SET next_retry_at_ms = NULL, last_error = NULL WHERE singleton = 1",
            [],
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    transaction
        .commit()
        .map_err(DurableSourceInventoryError::Scratch)
}

fn observe_directory_entry(
    entry: &fs::DirEntry,
    request: &DurableSourceInventoryRequest,
) -> DurableInventoryResult<Option<DurableDirectoryObservation>> {
    let path = entry.path();
    crate::pace_current_filesystem_operation(path.as_os_str().len() as u64);
    let file_type = entry
        .file_type()
        .map_err(DurableSourceInventoryError::Filesystem)?;
    if file_type.is_dir() {
        let directory = observe_directory(&path)?;
        return Ok(Some(DurableDirectoryObservation::Directory {
            path: directory.path,
            object_identity: directory.object_identity,
        }));
    }
    let included_file = file_type.is_file()
        && (request.mode == DurableSourceInventoryMode::RegularFiles
            || path.extension() == Some(OsStr::new("jsonl")));
    if included_file {
        crate::common::io::ensure_regular_provider_transcript_file(&path)
            .map_err(|_| DurableSourceInventoryError::SourceChanged)?;
        let path = native_path_descriptor(&path)?;
        return Ok(Some(DurableDirectoryObservation::File {
            journal_identity: journal_identity(request, &path.identity.sha256),
            path,
        }));
    }
    if file_type.is_symlink()
        && request.mode == DurableSourceInventoryMode::Jsonl
        && path.extension() == Some(OsStr::new("jsonl"))
    {
        return Err(DurableSourceInventoryError::SourceChanged);
    }
    Ok(None)
}

fn flush_active_directory_page(
    connection: &mut Connection,
    owner: &DurableSourceInventoryOwner,
    checkpoint: &DurableSourceInventoryCheckpoint,
    active_identity: &[u8; 32],
    observations: &[DurableDirectoryObservation],
    observed_entries: u64,
    exhausted: bool,
) -> DurableInventoryResult<()> {
    let write_bytes = observations.iter().fold(256_u64, |total, observation| {
        let path_bytes = match observation {
            DurableDirectoryObservation::Directory { path, .. }
            | DurableDirectoryObservation::File { path, .. } => path.encoded.len() as u64,
        };
        total.saturating_add(path_bytes).saturating_add(256)
    });
    crate::pace_current_filesystem_operation(write_bytes);
    crate::pace_current_disk_io(write_bytes);
    let transaction = connection
        .transaction()
        .map_err(DurableSourceInventoryError::Scratch)?;
    assert_durable_owner_transaction(&transaction, owner)?;
    let active: Option<Vec<u8>> = transaction
        .query_row(
            "SELECT active_directory_identity FROM inventory_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    if active.as_deref() != Some(active_identity.as_slice()) {
        return Err(DurableSourceInventoryError::StaleOwner);
    }
    let mut queued_added = 0_u64;
    let mut files_added = 0_u64;
    for observation in observations {
        match observation {
            DurableDirectoryObservation::Directory {
                path,
                object_identity,
            } => {
                if insert_directory_observation(&transaction, checkpoint, path, object_identity)? {
                    queued_added = queued_added.saturating_add(1);
                }
            }
            DurableDirectoryObservation::File {
                path,
                journal_identity,
            } => {
                if insert_file_observation(&transaction, path, journal_identity, active_identity)? {
                    files_added = files_added.saturating_add(1);
                }
            }
        }
    }
    let pending_added = if checkpoint.mode == DurableSourceInventoryMode::Jsonl {
        files_added
    } else {
        0
    };
    transaction
        .execute(
            "UPDATE inventory_meta
                 SET active_observed_entries = active_observed_entries + ?1,
                     replay_high_water_entries = MAX(
                         replay_high_water_entries,
                         active_observed_entries + ?1
                     ),
                     queued_directories = queued_directories + ?2,
                     discovered_files = discovered_files + ?3,
                     pending_effects = pending_effects + ?4,
                 last_error = NULL
             WHERE singleton = 1",
            params![
                u64_i64(observed_entries)?,
                u64_i64(queued_added)?,
                u64_i64(files_added)?,
                u64_i64(pending_added)?,
            ],
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    if exhausted {
        let changed = transaction
            .execute(
                "UPDATE directory_queue
                 SET state = 2, next_retry_at_ms = NULL
                 WHERE path_identity = ?1 AND state = 1",
                params![active_identity.as_slice()],
            )
            .map_err(DurableSourceInventoryError::Scratch)?;
        if changed != 1 {
            return Err(DurableSourceInventoryError::CorruptScratch);
        }
        transaction
            .execute(
                "UPDATE inventory_meta
                 SET active_directory_identity = NULL, active_observed_entries = 0,
                     completed_directories = completed_directories + 1,
                     next_retry_at_ms = NULL
                 WHERE singleton = 1",
                [],
            )
            .map_err(DurableSourceInventoryError::Scratch)?;
    }
    maybe_fail_durable_inventory(DurableInventoryFailurePoint::ScratchFlushBeforeCommit)?;
    publish_traversal_complete_if_ready(&transaction, owner)?;
    transaction
        .commit()
        .map_err(DurableSourceInventoryError::Scratch)
}

fn insert_directory_observation(
    transaction: &Transaction<'_>,
    checkpoint: &DurableSourceInventoryCheckpoint,
    path: &NativePathDescriptor,
    object_identity: &[u8],
) -> DurableInventoryResult<bool> {
    let fingerprint =
        durable_directory_fingerprint(checkpoint, &path.identity.sha256, object_identity);
    let inserted = transaction
        .execute(
            "INSERT OR IGNORE INTO directory_queue (
                path_identity, platform, encoding, path, object_identity,
                directory_fingerprint, state, attempt_count, replay_count, next_retry_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, 0, NULL)",
            params![
                path.identity.sha256.as_slice(),
                encode_platform(path.identity.platform),
                encode_encoding(path.identity.encoding),
                path.encoded,
                object_identity,
                fingerprint.as_slice(),
            ],
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    if inserted == 1 {
        return Ok(true);
    }
    let existing = transaction
        .query_row(
            "SELECT platform, encoding, path, object_identity, directory_fingerprint
             FROM directory_queue WHERE path_identity = ?1",
            params![path.identity.sha256.as_slice()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)?;
    if existing
        != (
            encode_platform(path.identity.platform),
            encode_encoding(path.identity.encoding),
            path.encoded.clone(),
            object_identity.to_vec(),
            fingerprint.to_vec(),
        )
    {
        return Err(DurableSourceInventoryError::SourceChanged);
    }
    Ok(false)
}

fn insert_file_observation(
    transaction: &Transaction<'_>,
    path: &NativePathDescriptor,
    journal_identity: &[u8; 32],
    directory_identity: &[u8; 32],
) -> DurableInventoryResult<bool> {
    let inserted = transaction
        .execute(
            "INSERT OR IGNORE INTO path_journal (
                journal_identity, path_identity, platform, encoding, path,
                directory_identity, state
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
            params![
                journal_identity.as_slice(),
                path.identity.sha256.as_slice(),
                encode_platform(path.identity.platform),
                encode_encoding(path.identity.encoding),
                path.encoded,
                directory_identity.as_slice(),
            ],
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    if inserted == 1 {
        return Ok(true);
    }
    let existing = transaction
        .query_row(
            "SELECT journal_identity, platform, encoding, path, directory_identity
             FROM path_journal WHERE path_identity = ?1",
            params![path.identity.sha256.as_slice()],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)?;
    if existing
        != (
            journal_identity.to_vec(),
            encode_platform(path.identity.platform),
            encode_encoding(path.identity.encoding),
            path.encoded.clone(),
            directory_identity.to_vec(),
        )
    {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
    Ok(false)
}

fn publish_traversal_complete_if_ready(
    transaction: &Transaction<'_>,
    owner: &DurableSourceInventoryOwner,
) -> DurableInventoryResult<()> {
    assert_durable_owner_transaction(transaction, owner)?;
    let (queued, active): (i64, Option<Vec<u8>>) = transaction
        .query_row(
            "SELECT queued_directories, active_directory_identity
             FROM inventory_meta WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    if queued == 0 && active.is_none() {
        transaction
            .execute(
                "UPDATE inventory_meta
                 SET traversal_complete = 1,
                     phase = CASE
                         WHEN selection_complete = 0 THEN 5
                         WHEN pending_effects = 0 THEN 3
                         ELSE 2
                     END
                 WHERE singleton = 1 AND phase != 4",
                [],
            )
            .map_err(DurableSourceInventoryError::Scratch)?;
        publish_complete_if_ready(transaction, owner)?;
    }
    Ok(())
}

fn publish_complete_if_ready(
    transaction: &Transaction<'_>,
    owner: &DurableSourceInventoryOwner,
) -> DurableInventoryResult<()> {
    assert_durable_owner_transaction(transaction, owner)?;
    transaction
        .execute(
            "UPDATE inventory_meta
             SET complete = 1, phase = 3
             WHERE singleton = 1
               AND phase != 4
               AND traversal_complete = 1
               AND selection_complete = 1
               AND active_directory_identity IS NULL
               AND queued_directories = 0
               AND pending_effects = 0",
            [],
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    Ok(())
}

fn assert_durable_owner(
    connection: &Connection,
    owner: &DurableSourceInventoryOwner,
) -> DurableInventoryResult<()> {
    let matches = connection
        .query_row(
            "SELECT 1 FROM inventory_meta
             WHERE singleton = 1 AND owner_epoch = ?1 AND owner_token = ?2",
            params![owner_epoch_i64(owner.epoch)?, owner.token.as_slice()],
            |_| Ok(()),
        )
        .optional()
        .map_err(DurableSourceInventoryError::Scratch)?
        .is_some();
    if !matches {
        return Err(DurableSourceInventoryError::StaleOwner);
    }
    Ok(())
}

fn assert_durable_owner_transaction(
    transaction: &Transaction<'_>,
    owner: &DurableSourceInventoryOwner,
) -> DurableInventoryResult<()> {
    let matches = transaction
        .query_row(
            "SELECT 1 FROM inventory_meta
             WHERE singleton = 1 AND owner_epoch = ?1 AND owner_token = ?2",
            params![owner_epoch_i64(owner.epoch)?, owner.token.as_slice()],
            |_| Ok(()),
        )
        .optional()
        .map_err(DurableSourceInventoryError::Scratch)?
        .is_some();
    if !matches {
        return Err(DurableSourceInventoryError::StaleOwner);
    }
    Ok(())
}

fn persist_durable_failure(
    connection: &mut Connection,
    owner: &DurableSourceInventoryOwner,
    failure: DurableSourceInventoryFailureKind,
) -> DurableInventoryResult<()> {
    let transaction = connection
        .transaction()
        .map_err(DurableSourceInventoryError::Scratch)?;
    assert_durable_owner_transaction(&transaction, owner)?;
    let retry_at = matches!(
        failure,
        DurableSourceInventoryFailureKind::OpenDirectory
            | DurableSourceInventoryFailureKind::ReadDirectory
            | DurableSourceInventoryFailureKind::ScratchWrite
    )
    .then(|| now_ms().saturating_add(DURABLE_INVENTORY_RETRY_BASE_MS));
    transaction
        .execute(
            "UPDATE inventory_meta
             SET last_error = ?1, next_retry_at_ms = COALESCE(?2, next_retry_at_ms)
             WHERE singleton = 1",
            params![encode_failure_kind(failure), retry_at],
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    if let Some(retry_at) = retry_at {
        transaction
            .execute(
                "UPDATE directory_queue SET next_retry_at_ms = ?1
                 WHERE path_identity = (
                    SELECT active_directory_identity FROM inventory_meta WHERE singleton = 1
                 ) AND state = 1",
                params![retry_at],
            )
            .map_err(DurableSourceInventoryError::Scratch)?;
    }
    transaction
        .commit()
        .map_err(DurableSourceInventoryError::Scratch)
}

fn durable_inventory_scratch_bytes(path: &Path) -> DurableInventoryResult<u64> {
    let mut bytes = 0_u64;
    for name in [
        DURABLE_INVENTORY_DATABASE_NAME,
        "inventory.sqlite-journal",
        "inventory.sqlite-wal",
        "inventory.sqlite-shm",
    ] {
        let file = path.join(name);
        crate::pace_current_filesystem_operation(file.as_os_str().len() as u64);
        match fs::symlink_metadata(file) {
            Ok(metadata) if metadata.file_type().is_file() => {
                bytes = bytes.saturating_add(metadata.len());
            }
            Ok(_) => return Err(DurableSourceInventoryError::TamperedScratch),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(DurableSourceInventoryError::Filesystem(error)),
        }
    }
    Ok(bytes)
}

fn cleanup_durable_inventory_scratch(
    data_root: &Path,
    scratch_name: &str,
) -> DurableInventoryResult<DurableSourceInventoryCleanupOutcome> {
    let outcome = DurableCaptureScratch::cleanup_slice(data_root, scratch_name)
        .map_err(DurableSourceInventoryError::Filesystem)?;
    Ok(match outcome {
        DurableScratchCleanupOutcome::Complete => DurableSourceInventoryCleanupOutcome::Complete,
        DurableScratchCleanupOutcome::Pending => DurableSourceInventoryCleanupOutcome::Pending,
        DurableScratchCleanupOutcome::Busy => DurableSourceInventoryCleanupOutcome::Busy,
    })
}

fn map_durable_scratch_create_error(error: io::Error) -> DurableSourceInventoryError {
    match error.kind() {
        io::ErrorKind::AlreadyExists => DurableSourceInventoryError::CheckpointMismatch,
        io::ErrorKind::WouldBlock => DurableSourceInventoryError::Locked,
        _ => DurableSourceInventoryError::Filesystem(error),
    }
}

fn map_durable_scratch_open_error(error: io::Error) -> DurableSourceInventoryError {
    match error.kind() {
        io::ErrorKind::NotFound => DurableSourceInventoryError::MissingScratch,
        io::ErrorKind::WouldBlock => DurableSourceInventoryError::Locked,
        io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidData => {
            DurableSourceInventoryError::TamperedScratch
        }
        _ => DurableSourceInventoryError::Filesystem(error),
    }
}

fn retry_delay_ms(attempt: u64) -> i64 {
    let shift = u32::try_from(attempt.saturating_sub(1).min(20)).unwrap_or(20);
    DURABLE_INVENTORY_RETRY_BASE_MS
        .saturating_mul(1_i64.checked_shl(shift).unwrap_or(i64::MAX))
        .min(DURABLE_INVENTORY_RETRY_MAX_MS)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(i64::MAX)
}

fn owner_epoch_i64(value: u64) -> DurableInventoryResult<i64> {
    u64_i64(value)
}

fn u64_i64(value: u64) -> DurableInventoryResult<i64> {
    i64::try_from(value).map_err(|_| {
        DurableSourceInventoryError::InvalidRequest(
            "inventory integer exceeds SQLite's signed range",
        )
    })
}

fn nonnegative_u64(value: i64) -> DurableInventoryResult<u64> {
    u64::try_from(value).map_err(|_| DurableSourceInventoryError::CorruptScratch)
}

fn nonnegative_u32(value: i64) -> DurableInventoryResult<u32> {
    u32::try_from(value).map_err(|_| DurableSourceInventoryError::CorruptScratch)
}

fn fixed_identity(value: Vec<u8>) -> DurableInventoryResult<[u8; 32]> {
    value
        .try_into()
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)
}

fn fixed_token(value: Vec<u8>) -> DurableInventoryResult<[u8; 16]> {
    value
        .try_into()
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)
}

fn encode_mode(mode: DurableSourceInventoryMode) -> i64 {
    match mode {
        DurableSourceInventoryMode::Jsonl => 1,
        DurableSourceInventoryMode::RegularFiles => 2,
    }
}

fn decode_mode(value: i64) -> DurableInventoryResult<DurableSourceInventoryMode> {
    match value {
        1 => Ok(DurableSourceInventoryMode::Jsonl),
        2 => Ok(DurableSourceInventoryMode::RegularFiles),
        _ => Err(DurableSourceInventoryError::CorruptScratch),
    }
}

fn encode_platform(platform: NativePathPlatform) -> i64 {
    match platform {
        NativePathPlatform::Unix => 1,
        NativePathPlatform::Windows => 2,
    }
}

fn decode_platform(value: i64) -> DurableInventoryResult<NativePathPlatform> {
    match value {
        1 => Ok(NativePathPlatform::Unix),
        2 => Ok(NativePathPlatform::Windows),
        _ => Err(DurableSourceInventoryError::CorruptScratch),
    }
}

fn encode_encoding(encoding: NativePathEncoding) -> i64 {
    match encoding {
        NativePathEncoding::UnixBytes => 1,
        NativePathEncoding::WindowsUtf16Be => 2,
    }
}

fn decode_encoding(value: i64) -> DurableInventoryResult<NativePathEncoding> {
    match value {
        1 => Ok(NativePathEncoding::UnixBytes),
        2 => Ok(NativePathEncoding::WindowsUtf16Be),
        _ => Err(DurableSourceInventoryError::CorruptScratch),
    }
}

fn decode_native_path(
    platform: NativePathPlatform,
    encoding: NativePathEncoding,
    encoded: Vec<u8>,
) -> DurableInventoryResult<PathBuf> {
    let native = native_path_tags()?;
    if native != (platform, encoding) {
        return Err(DurableSourceInventoryError::CheckpointMismatch);
    }
    decode_path(encoded).map_err(|_| DurableSourceInventoryError::CorruptScratch)
}

fn native_path_identity_hash(
    platform: NativePathPlatform,
    encoding: NativePathEncoding,
    encoded: &[u8],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-native-path-v1\0");
    hasher.update([
        encode_platform(platform) as u8,
        encode_encoding(encoding) as u8,
    ]);
    hasher.update((encoded.len() as u64).to_be_bytes());
    hasher.update(encoded);
    hasher.finalize().into()
}

fn decode_phase(value: i64) -> DurableInventoryResult<DurableSourceInventoryPhase> {
    match value {
        1 => Ok(DurableSourceInventoryPhase::Traversal),
        2 => Ok(DurableSourceInventoryPhase::Effects),
        3 => Ok(DurableSourceInventoryPhase::Complete),
        4 => Ok(DurableSourceInventoryPhase::Abandoned),
        5 => Ok(DurableSourceInventoryPhase::Selection),
        _ => Err(DurableSourceInventoryError::CorruptScratch),
    }
}

fn encode_failure_kind(value: DurableSourceInventoryFailureKind) -> i64 {
    match value {
        DurableSourceInventoryFailureKind::OpenDirectory => 1,
        DurableSourceInventoryFailureKind::ReadDirectory => 2,
        DurableSourceInventoryFailureKind::ScratchWrite => 3,
        DurableSourceInventoryFailureKind::SourceChanged => 4,
    }
}

fn decode_failure_kind(value: i64) -> DurableInventoryResult<DurableSourceInventoryFailureKind> {
    match value {
        1 => Ok(DurableSourceInventoryFailureKind::OpenDirectory),
        2 => Ok(DurableSourceInventoryFailureKind::ReadDirectory),
        3 => Ok(DurableSourceInventoryFailureKind::ScratchWrite),
        4 => Ok(DurableSourceInventoryFailureKind::SourceChanged),
        _ => Err(DurableSourceInventoryError::CorruptScratch),
    }
}

fn decode_bool(value: i64) -> DurableInventoryResult<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(DurableSourceInventoryError::CorruptScratch),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurableInventoryFailurePoint {
    ScratchFlushBeforeCommit,
    ActiveTransitionAfterCommit,
    DirectoryCompleteAfterCommit,
    OpenAfterValidation,
    OwnerAdoptionAfterCommit,
}

#[cfg(test)]
thread_local! {
    static DURABLE_INVENTORY_FAILURE_ONCE: std::cell::Cell<Option<DurableInventoryFailurePoint>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn inject_durable_inventory_failure_once(point: DurableInventoryFailurePoint) {
    DURABLE_INVENTORY_FAILURE_ONCE.with(|slot| slot.set(Some(point)));
}

#[cfg(test)]
fn maybe_fail_durable_inventory(point: DurableInventoryFailurePoint) -> DurableInventoryResult<()> {
    let should_fail = DURABLE_INVENTORY_FAILURE_ONCE.with(|slot| {
        if slot.get() == Some(point) {
            slot.set(None);
            true
        } else {
            false
        }
    });
    if should_fail {
        return Err(DurableSourceInventoryError::Scratch(
            rusqlite::Error::InvalidQuery,
        ));
    }
    Ok(())
}

#[cfg(not(test))]
fn maybe_fail_durable_inventory(
    _point: DurableInventoryFailurePoint,
) -> DurableInventoryResult<()> {
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SortedPathInventoryMetrics {
    pub(crate) paths: usize,
    pub(crate) max_in_memory_batch: usize,
}

pub(crate) struct SortedJsonlPathInventory {
    storage: PathInventoryStorage,
    metrics: SortedPathInventoryMetrics,
}

struct PathInventoryStorage {
    connection: Connection,
    _scratch: CaptureScratchSpace,
}

impl PathInventoryStorage {
    fn create() -> Result<Self> {
        let scratch = CaptureScratchSpace::create("path-inventory")?;
        drop(scratch.create_file("paths.sqlite")?);
        let connection = Connection::open(scratch.path().join("paths.sqlite"))?;
        crate::pace_current_disk_io(4 * 1024);
        connection.execute_batch(
            "PRAGMA journal_mode = MEMORY;
             PRAGMA synchronous = OFF;
             CREATE TABLE paths (
                 sort_key BLOB PRIMARY KEY NOT NULL,
                 path BLOB NOT NULL
             ) WITHOUT ROWID;
             CREATE TABLE selected_paths (
                 group_key BLOB PRIMARY KEY NOT NULL,
                 rank INTEGER NOT NULL,
                 sort_key BLOB UNIQUE NOT NULL,
                 path BLOB NOT NULL
             ) WITHOUT ROWID;",
        )?;
        Ok(Self {
            connection,
            _scratch: scratch,
        })
    }

    fn for_each(&self, mut visitor: impl FnMut(PathBuf) -> Result<()>) -> Result<()> {
        let mut cursor = None;
        loop {
            let page = self.paths_page("paths", cursor.as_deref(), PATH_INSERT_BATCH)?;
            for path in page.paths {
                visitor(path)?;
            }
            cursor = page.next_cursor;
            if page.complete {
                return Ok(());
            }
            std::thread::yield_now();
        }
    }

    fn paths_page(
        &self,
        table: &str,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<SourcePathInventoryPage> {
        let row_limit = limit.clamp(1, PATH_INSERT_BATCH);
        self.pace_read(row_limit, after.map_or(0, |cursor| cursor.len()));
        let sql = match (table, after.is_some()) {
            ("paths", false) => "SELECT sort_key, path FROM paths ORDER BY sort_key LIMIT ?1",
            ("paths", true) => {
                "SELECT sort_key, path FROM paths WHERE sort_key > ?1 ORDER BY sort_key LIMIT ?2"
            }
            ("selected_paths", false) => {
                "SELECT sort_key, path FROM selected_paths ORDER BY sort_key LIMIT ?1"
            }
            ("selected_paths", true) => "SELECT sort_key, path FROM selected_paths WHERE sort_key > ?1 ORDER BY sort_key LIMIT ?2",
            _ => {
                return Err(crate::CaptureError::SystemInvariant(
                    "unknown source path inventory table",
                ))
            }
        };
        let mut statement = self.connection.prepare(sql)?;
        let sql_limit = i64::try_from(row_limit).unwrap_or(i64::MAX);
        let mut rows = match after {
            Some(after) => statement.query(params![after, sql_limit])?,
            None => statement.query(params![sql_limit])?,
        };
        let mut paths = Vec::with_capacity(row_limit);
        let mut next_cursor = None;
        while let Some(row) = rows.next()? {
            next_cursor = Some(row.get::<_, Vec<u8>>(0)?);
            paths.push(decode_path(row.get::<_, Vec<u8>>(1)?)?);
        }
        let complete = paths.len() < row_limit;
        Ok(SourcePathInventoryPage {
            paths,
            next_cursor,
            complete,
        })
    }

    fn contains_path(&self, table: &str, path: &Path) -> Result<bool> {
        let encoded = encode_path(path);
        self.pace_read(1, encoded.len());
        let sql = match table {
            "paths" => "SELECT 1 FROM paths WHERE sort_key = ?1",
            "selected_paths" => "SELECT 1 FROM selected_paths WHERE sort_key = ?1",
            _ => {
                return Err(crate::CaptureError::SystemInvariant(
                    "unknown source path inventory table",
                ))
            }
        };
        Ok(self
            .connection
            .query_row(sql, params![encoded], |_| Ok(()))
            .optional()?
            .is_some())
    }

    fn pace_read(&self, rows: usize, key_bytes: usize) {
        crate::pace_current_filesystem_operation(self._scratch.path().as_os_str().len() as u64);
        crate::pace_current_disk_io(
            SCRATCH_PATH_READ_BASE_BYTES
                .saturating_add(
                    SCRATCH_PATH_READ_BYTES_PER_ROW
                        .saturating_mul(u64::try_from(rows).unwrap_or(u64::MAX)),
                )
                .saturating_add(key_bytes as u64),
        );
    }
}

impl SortedJsonlPathInventory {
    pub(crate) fn build(root: &Path, mut include: impl FnMut(&Path) -> bool) -> Result<Self> {
        let mut storage = PathInventoryStorage::create()?;
        let mut cursor = FilesystemTraversalCursor::jsonl(root);
        let mut metrics = SortedPathInventoryMetrics::default();
        while !cursor.is_complete() {
            let mut pending = Vec::with_capacity(PATH_INSERT_BATCH);
            cursor.advance(FilesystemTraversalBudget::default(), &mut |path| {
                if include(path) {
                    let encoded = encode_path(path);
                    pending.push((encoded.clone(), encoded));
                }
                Ok(())
            })?;
            metrics.max_in_memory_batch = metrics.max_in_memory_batch.max(pending.len());
            if !pending.is_empty() {
                crate::pace_current_filesystem_operation(path_batch_write_bytes(&pending));
            }
            metrics.paths += flush_paths(&mut storage.connection, &mut pending)?;
            if !cursor.is_complete() {
                std::thread::yield_now();
            }
        }

        Ok(Self { storage, metrics })
    }

    pub(crate) fn metrics(&self) -> SortedPathInventoryMetrics {
        self.metrics
    }

    pub(crate) fn for_each(&self, mut visitor: impl FnMut(PathBuf) -> Result<()>) -> Result<()> {
        self.storage.for_each(&mut visitor)
    }
}

fn path_batch_write_bytes(pending: &[(Vec<u8>, Vec<u8>)]) -> u64 {
    pending.iter().fold(0_u64, |total, (sort_key, path)| {
        total
            .saturating_add(64)
            .saturating_add(sort_key.len() as u64)
            .saturating_add(path.len() as u64)
    })
}

fn flush_paths(
    connection: &mut Connection,
    pending: &mut Vec<(Vec<u8>, Vec<u8>)>,
) -> Result<usize> {
    if pending.is_empty() {
        return Ok(0);
    }
    let write_bytes = path_batch_write_bytes(pending);
    crate::pace_current_disk_io(write_bytes);
    let transaction = connection.transaction()?;
    let mut inserted = 0usize;
    {
        let mut statement =
            transaction.prepare("INSERT OR IGNORE INTO paths (sort_key, path) VALUES (?1, ?2)")?;
        for (sort_key, path) in pending.iter() {
            inserted += statement.execute(params![sort_key, path])?;
        }
    }
    transaction.commit()?;
    pending.clear();
    Ok(inserted)
}

#[cfg(unix)]
fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;

    path.as_os_str().as_bytes().to_vec()
}

#[cfg(unix)]
fn decode_path(encoded: Vec<u8>) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    Ok(PathBuf::from(std::ffi::OsString::from_vec(encoded)))
}

#[cfg(windows)]
fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_be_bytes)
        .collect()
}

#[cfg(windows)]
fn decode_path(encoded: Vec<u8>) -> Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;

    if encoded.len() % 2 != 0 {
        return Err(crate::CaptureError::SystemInvariant(
            "capture path inventory contains an invalid Windows path",
        ));
    }
    let wide = encoded
        .chunks_exact(2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&wide)))
}

#[cfg(not(any(unix, windows)))]
fn encode_path(_path: &Path) -> Vec<u8> {
    Vec::new()
}

#[cfg(not(any(unix, windows)))]
fn decode_path(_encoded: Vec<u8>) -> Result<PathBuf> {
    Err(crate::CaptureError::SystemInvariant(
        "capture path inventory is unsupported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs};

    use super::*;

    #[test]
    fn inventory_streams_lexicographically_with_a_bounded_insert_batch() {
        let temp = tempfile::tempdir().unwrap();
        for index in (0..513).rev() {
            fs::write(temp.path().join(format!("path-{index:04}.jsonl")), "{}\n").unwrap();
        }

        let inventory = SortedJsonlPathInventory::build(temp.path(), |_| true).unwrap();
        let mut observed = Vec::new();
        inventory
            .for_each(|path| {
                observed.push(path);
                Ok(())
            })
            .unwrap();

        let mut expected = observed.clone();
        expected.sort();
        assert_eq!(observed, expected);
        assert_eq!(inventory.metrics().paths, 513);
        assert_eq!(inventory.metrics().max_in_memory_batch, PATH_INSERT_BATCH);
    }

    #[test]
    fn bounded_inventory_returns_control_and_pages_without_materializing_the_tree() {
        let temp = tempfile::tempdir().unwrap();
        for index in (0..193).rev() {
            fs::write(temp.path().join(format!("path-{index:04}.jsonl")), "{}\n").unwrap();
        }
        let mut inventory = BoundedSourcePathInventory::new_jsonl(temp.path());
        let mut traversal_slices = 0usize;
        loop {
            let slice = inventory.advance().unwrap();
            traversal_slices = traversal_slices.saturating_add(1);
            if slice.complete {
                break;
            }
        }

        let mut cursor = None;
        let mut observed = Vec::new();
        let mut pages = 0usize;
        loop {
            let page = inventory
                .paths_page(cursor.as_deref(), PATH_INSERT_BATCH)
                .unwrap();
            assert!(page.paths.len() <= PATH_INSERT_BATCH);
            pages = pages.saturating_add(1);
            observed.extend(page.paths);
            cursor = page.next_cursor;
            if page.complete {
                break;
            }
        }

        assert!(traversal_slices > 1);
        assert!(pages > 1);
        assert_eq!(observed.len(), 193);
        assert!(observed.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(inventory.metrics().discovered_files, 193);
    }

    #[test]
    fn bounded_inventory_retries_consumed_paths_after_scratch_flush_failure() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..17 {
            fs::write(temp.path().join(format!("path-{index:04}.jsonl")), "{}\n").unwrap();
        }
        let mut inventory = BoundedSourcePathInventory::new_jsonl(temp.path());
        assert!(!inventory.advance().unwrap().complete);
        inventory
            .storage
            .as_ref()
            .unwrap()
            .connection
            .execute("DROP TABLE paths", [])
            .unwrap();

        assert!(inventory.advance().is_err());
        assert!(!inventory.pending_paths.is_empty());
        inventory
            .storage
            .as_ref()
            .unwrap()
            .connection
            .execute_batch(
                "CREATE TABLE paths (
                    sort_key BLOB PRIMARY KEY NOT NULL,
                    path BLOB NOT NULL
                 ) WITHOUT ROWID;",
            )
            .unwrap();

        while !inventory.advance().unwrap().complete {}
        let page = inventory.paths_page(None, PATH_INSERT_BATCH).unwrap();
        assert!(page.complete);
        assert_eq!(page.paths.len(), 17);
        assert!(page.paths.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(inventory.pending_paths.is_empty());
    }

    #[test]
    fn bounded_inventory_restarts_after_an_indeterminate_directory_read() {
        use crate::common::io::{inject_traversal_failure_after, TraversalFailurePoint};

        let temp = tempfile::tempdir().unwrap();
        let mut expected = Vec::new();
        for index in 0..193 {
            let path = temp.path().join(format!("path-{index:04}.jsonl"));
            fs::write(&path, "{}\n").unwrap();
            expected.push(path);
        }
        expected.sort();
        let mut inventory = BoundedSourcePathInventory::new_jsonl(temp.path());
        assert!(!inventory.advance().unwrap().complete);
        assert!(!inventory.advance().unwrap().complete);
        let first_scratch = inventory
            .storage
            .as_ref()
            .unwrap()
            ._scratch
            .path()
            .to_path_buf();
        let persisted_before_failure = inventory
            .storage
            .as_ref()
            .unwrap()
            .connection
            .query_row("SELECT COUNT(*) FROM paths", [], |row| {
                row.get::<_, usize>(0)
            })
            .unwrap();
        assert!(persisted_before_failure > 0);
        inject_traversal_failure_after(TraversalFailurePoint::AfterReadDirectoryEntry, 2);

        assert!(inventory.advance().is_err());
        assert!(inventory.restart_required);
        assert!(!inventory.pending_paths.is_empty());
        assert_eq!(
            inventory.storage.as_ref().unwrap()._scratch.path(),
            first_scratch
        );
        assert!(!inventory.advance().unwrap().complete);
        let restarted_scratch = inventory
            .storage
            .as_ref()
            .unwrap()
            ._scratch
            .path()
            .to_path_buf();
        assert_ne!(restarted_scratch, first_scratch);
        while !inventory.advance().unwrap().complete {}

        let mut cursor = None;
        let mut observed = Vec::new();
        loop {
            let page = inventory
                .paths_page(cursor.as_deref(), PATH_INSERT_BATCH)
                .unwrap();
            observed.extend(page.paths);
            cursor = page.next_cursor;
            if page.complete {
                break;
            }
        }
        assert_eq!(observed, expected);
        assert!(observed.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(inventory.metrics().discovered_files, 193);
    }

    #[test]
    fn bounded_inventory_retains_scratch_for_a_retryable_traversal_error() {
        use crate::common::io::{inject_traversal_failure_once, TraversalFailurePoint};

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("path.jsonl");
        fs::write(&path, "{}\n").unwrap();
        let mut inventory = BoundedSourcePathInventory::new_jsonl(temp.path());
        assert!(!inventory.advance().unwrap().complete);
        let first_scratch = inventory
            .storage
            .as_ref()
            .unwrap()
            ._scratch
            .path()
            .to_path_buf();
        inject_traversal_failure_once(TraversalFailurePoint::BeforeReadDirectoryEntry);

        assert!(inventory.advance().is_err());
        assert_eq!(
            inventory.storage.as_ref().unwrap()._scratch.path(),
            first_scratch
        );
        while !inventory.advance().unwrap().complete {}
        assert_eq!(
            inventory.paths_page(None, PATH_INSERT_BATCH).unwrap().paths,
            vec![path]
        );
    }

    #[test]
    fn scratch_inventory_page_and_membership_reads_charge_the_shared_pacer() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("path.jsonl");
        fs::write(&path, "{}\n").unwrap();
        let pacer = crate::DiskIoPacer::new(u64::MAX, u64::MAX);
        let _pacing = crate::install_disk_io_pacer(pacer.clone());
        let mut inventory = BoundedSourcePathInventory::new_jsonl(temp.path());
        while !inventory.advance().unwrap().complete {}
        let operations_before = pacer.filesystem_operation_count();
        let bytes_before = pacer.charged_bytes();

        let page = inventory.paths_page(None, PATH_INSERT_BATCH).unwrap();
        assert!(inventory.contains_path(&path).unwrap());

        assert_eq!(page.paths, vec![path]);
        assert!(pacer.filesystem_operation_count() >= operations_before + 2);
        assert!(pacer.charged_bytes() > bytes_before);
    }

    #[cfg(unix)]
    #[test]
    fn inventory_round_trips_non_utf8_paths() {
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(std::ffi::OsString::from_vec(
            b"non-utf8-\xff.jsonl".to_vec(),
        ));
        fs::write(&path, "{}\n").unwrap();

        let inventory = SortedJsonlPathInventory::build(temp.path(), |_| true).unwrap();
        let mut observed = Vec::new();
        inventory
            .for_each(|path| {
                observed.push(path);
                Ok(())
            })
            .unwrap();
        assert_eq!(observed, vec![path]);
    }

    fn durable_request(root: &Path, run: u8) -> DurableSourceInventoryRequest {
        DurableSourceInventoryRequest {
            build_identity: b"capture-test-build-v1".to_vec(),
            run_id: vec![b'r', run],
            source_id: b"source-1".to_vec(),
            generation: u64::from(run) + 1,
            root: root.to_path_buf(),
            mode: DurableSourceInventoryMode::Jsonl,
        }
    }

    fn durable_owner(epoch: u64, byte: u8) -> DurableSourceInventoryOwner {
        DurableSourceInventoryOwner {
            epoch,
            token: vec![byte; 16],
        }
    }

    fn create_owned_durable_inventory(
        data_root: &Path,
        request: DurableSourceInventoryRequest,
        owner: DurableSourceInventoryOwner,
    ) -> DurableSourcePathInventory {
        let mut inventory = DurableSourcePathInventory::create(data_root, request).unwrap();
        let state = inventory.open_state().unwrap();
        assert_eq!(state.current_owner, None);
        assert_eq!(state.scratch.identity, state.checkpoint.scratch_identity);
        assert_eq!(state.scratch.integrity, state.checkpoint.scratch_integrity);
        assert_eq!(
            state.scratch.lock_identity,
            state.checkpoint.scratch_lock_identity
        );
        inventory.adopt_owner(None, owner).unwrap();
        inventory
    }

    fn clear_durable_retry_for_test(
        inventory: &mut DurableSourcePathInventory,
        owner: &DurableSourceInventoryOwner,
    ) {
        let meta = read_durable_meta(inventory.connection().unwrap()).unwrap();
        if let Some(identity) = meta.active_directory_identity {
            clear_active_retry(inventory.connection_mut().unwrap(), owner, &identity).unwrap();
        }
    }

    fn drain_durable_inventory(
        inventory: &mut DurableSourcePathInventory,
        owner: &DurableSourceInventoryOwner,
    ) -> Vec<DurableSourceInventoryJournalEntry> {
        let mut observed = Vec::new();
        for _ in 0..1024 {
            clear_durable_retry_for_test(inventory, owner);
            inventory.advance(owner).unwrap();
            let page = inventory.next_effects_page(owner).unwrap();
            if !page.entries.is_empty() {
                for entry in &page.entries {
                    assert_eq!(entry.directory.scratch, inventory.scratch_state());
                }
                let identities = page
                    .entries
                    .iter()
                    .map(|entry| entry.journal_identity)
                    .collect::<Vec<_>>();
                observed.extend(page.entries);
                inventory.acknowledge_effects(owner, &identities).unwrap();
            }
            if inventory.completion_proof(owner).unwrap().is_some() {
                return observed;
            }
        }
        panic!("durable inventory did not converge within the source-test bound");
    }

    #[test]
    fn durable_inventory_bounds_a_flat_directory_and_converges_exactly_once() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("source");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&source_root).unwrap();
        let mut expected = BTreeSet::new();
        for index in 0..193 {
            let path = source_root.join(format!("path-{index:04}.jsonl"));
            fs::write(&path, "{}\n").unwrap();
            expected.insert(path);
        }
        fs::write(source_root.join("ignored.txt"), "ignored").unwrap();
        let request = durable_request(&source_root, 1);
        let owner = durable_owner(1, 0x11);
        let mut inventory = create_owned_durable_inventory(&data_root, request, owner.clone());

        let first = inventory.advance(&owner).unwrap();
        assert!(first.observed_entries <= DURABLE_INVENTORY_PAGE_ENTRIES as u64);
        assert!(first.discovered_files <= DURABLE_INVENTORY_PAGE_ENTRIES as u64);
        assert!(!first.traversal_complete);
        let status = inventory.status().unwrap();
        let active = status.active_directory.unwrap();
        assert_eq!(active.attempt_count, 1);
        assert_eq!(active.replay_count, 0);
        assert_eq!(active.authority.scratch, status.scratch);

        let observed = drain_durable_inventory(&mut inventory, &owner);
        let unique = observed
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(unique, expected);
        assert_eq!(observed.len(), expected.len());
        let proof = inventory.completion_proof(&owner).unwrap().unwrap();
        assert_eq!(proof.owner, owner);
        assert_eq!(proof.scratch, inventory.scratch_state());
        assert_eq!(proof.discovered_files, 193);
    }

    #[test]
    fn durable_inventory_replays_a_failed_scratch_flush_without_omission() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("source");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&source_root).unwrap();
        for index in 0..70 {
            fs::write(source_root.join(format!("path-{index:04}.jsonl")), "{}\n").unwrap();
        }
        let owner = durable_owner(1, 0x21);
        let mut inventory = create_owned_durable_inventory(
            &data_root,
            durable_request(&source_root, 2),
            owner.clone(),
        );
        inject_durable_inventory_failure_once(
            DurableInventoryFailurePoint::ScratchFlushBeforeCommit,
        );

        assert!(inventory.advance(&owner).is_err());
        let failed = inventory.status().unwrap();
        assert_eq!(failed.discovered_files, 0);
        assert_eq!(failed.pending_effects, 0);
        assert_eq!(
            failed.error,
            Some(DurableSourceInventoryFailureKind::ScratchWrite)
        );
        clear_durable_retry_for_test(&mut inventory, &owner);
        inventory.advance(&owner).unwrap();
        let first_page = inventory.next_effects_page(&owner).unwrap();
        assert_eq!(first_page, inventory.next_effects_page(&owner).unwrap());

        let observed = drain_durable_inventory(&mut inventory, &owner);
        assert_eq!(observed.len(), 70);
        let status = inventory.status().unwrap();
        assert!(status.replay_count >= 1);
    }

    #[test]
    fn durable_inventory_adopts_only_store_issued_owners_across_crash_gaps() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("source");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&source_root).unwrap();
        fs::write(source_root.join("path.jsonl"), "{}\n").unwrap();
        let request = durable_request(&source_root, 3);
        let first_owner = durable_owner(1, 0x31);
        let mut inventory =
            create_owned_durable_inventory(&data_root, request.clone(), first_owner.clone());
        inject_durable_inventory_failure_once(
            DurableInventoryFailurePoint::ActiveTransitionAfterCommit,
        );
        assert!(inventory.advance(&first_owner).is_err());
        let checkpoint = inventory.checkpoint().clone();
        assert!(inventory.status().unwrap().active_directory.is_some());
        drop(inventory);

        inject_durable_inventory_failure_once(DurableInventoryFailurePoint::OpenAfterValidation);
        assert!(
            DurableSourcePathInventory::open(&data_root, request.clone(), &checkpoint).is_err()
        );
        let unopened_adoption = durable_owner(2, 0x32);
        let inventory =
            DurableSourcePathInventory::open(&data_root, request.clone(), &checkpoint).unwrap();
        assert_eq!(
            inventory.open_state().unwrap().current_owner,
            Some(first_owner.clone())
        );
        drop(unopened_adoption);
        drop(inventory);

        let mut inventory =
            DurableSourcePathInventory::open(&data_root, request.clone(), &checkpoint).unwrap();
        let adopted_before_crash = durable_owner(3, 0x33);
        inject_durable_inventory_failure_once(
            DurableInventoryFailurePoint::OwnerAdoptionAfterCommit,
        );
        assert!(inventory
            .adopt_owner(Some(&first_owner), adopted_before_crash.clone())
            .is_err());
        drop(inventory);

        let mut inventory =
            DurableSourcePathInventory::open(&data_root, request, &checkpoint).unwrap();
        assert_eq!(
            inventory.open_state().unwrap().current_owner,
            Some(adopted_before_crash.clone())
        );
        let recovered_owner = durable_owner(4, 0x34);
        inventory
            .adopt_owner(Some(&adopted_before_crash), recovered_owner.clone())
            .unwrap();
        assert!(matches!(
            inventory.advance(&first_owner),
            Err(DurableSourceInventoryError::StaleOwner)
        ));
        clear_durable_retry_for_test(&mut inventory, &recovered_owner);
        let observed = drain_durable_inventory(&mut inventory, &recovered_owner);
        assert_eq!(observed.len(), 1);
        assert!(inventory.status().unwrap().replay_count >= 1);
    }

    #[test]
    fn durable_inventory_never_replays_a_committed_directory_completion() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("source");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&source_root).unwrap();
        fs::write(source_root.join("path.jsonl"), "{}\n").unwrap();
        let request = durable_request(&source_root, 4);
        let first_owner = durable_owner(1, 0x41);
        let mut inventory =
            create_owned_durable_inventory(&data_root, request.clone(), first_owner.clone());
        inject_durable_inventory_failure_once(
            DurableInventoryFailurePoint::DirectoryCompleteAfterCommit,
        );

        assert!(inventory.advance(&first_owner).is_err());
        let checkpoint = inventory.checkpoint().clone();
        let status = inventory.status().unwrap();
        assert_eq!(status.completed_directories, 1);
        assert!(status.active_directory.is_none());
        assert_eq!(status.replay_count, 0);
        drop(inventory);

        let mut inventory =
            DurableSourcePathInventory::open(&data_root, request, &checkpoint).unwrap();
        let recovered_owner = durable_owner(2, 0x42);
        inventory
            .adopt_owner(Some(&first_owner), recovered_owner.clone())
            .unwrap();
        let observed = drain_durable_inventory(&mut inventory, &recovered_owner);
        assert_eq!(observed.len(), 1);
        let status = inventory.status().unwrap();
        assert_eq!(status.completed_directories, 1);
        assert_eq!(status.replay_count, 0);
    }

    #[test]
    fn durable_manifest_selection_survives_restart_and_keeps_existing_winner_semantics() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("source");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&source_root).unwrap();
        let paths = (0..4)
            .map(|index| source_root.join(format!("path-{index}.json")))
            .collect::<Vec<_>>();
        for path in &paths {
            fs::write(path, "{}\n").unwrap();
        }
        let mut request = durable_request(&source_root, 7);
        request.mode = DurableSourceInventoryMode::RegularFiles;
        let first_owner = durable_owner(1, 0x71);
        let mut inventory =
            create_owned_durable_inventory(&data_root, request.clone(), first_owner.clone());
        while !inventory.advance(&first_owner).unwrap().traversal_complete {}
        let discovered = inventory.paths_page(&first_owner, None, 64).unwrap();
        assert!(discovered.complete);
        assert_eq!(discovered.entries.len(), 4);

        inventory
            .select_path_candidates(
                &first_owner,
                &[
                    (b"group-a".to_vec(), 1, paths[1].clone()),
                    (b"group-a".to_vec(), 1, paths[0].clone()),
                    (b"group-b".to_vec(), 2, paths[2].clone()),
                    (b"group-b".to_vec(), 3, paths[3].clone()),
                ],
            )
            .unwrap();
        assert_eq!(inventory.status().unwrap().selected_files, 2);
        let checkpoint = inventory.checkpoint().clone();
        drop(inventory);

        let mut inventory =
            DurableSourcePathInventory::open(&data_root, request, &checkpoint).unwrap();
        let recovered_owner = durable_owner(2, 0x72);
        inventory
            .adopt_owner(Some(&first_owner), recovered_owner.clone())
            .unwrap();
        assert!(!inventory.status().unwrap().selection_complete);
        inventory.complete_path_selection(&recovered_owner).unwrap();
        let selected = inventory
            .selected_paths_page(&recovered_owner, None, 64)
            .unwrap();
        assert!(selected.complete);
        assert_eq!(
            selected
                .entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([paths[0].clone(), paths[2].clone()])
        );
        assert!(inventory
            .contains_selected_path(&recovered_owner, &paths[0])
            .unwrap());
        assert!(!inventory
            .contains_selected_path(&recovered_owner, &paths[1])
            .unwrap());
        let effects = inventory.next_effects_page(&recovered_owner).unwrap();
        assert_eq!(effects.entries.len(), 2);
        inventory
            .acknowledge_effects(
                &recovered_owner,
                &effects
                    .entries
                    .iter()
                    .map(|entry| entry.journal_identity)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let proof = inventory
            .completion_proof(&recovered_owner)
            .unwrap()
            .unwrap();
        assert_eq!(proof.discovered_files, 4);
        assert_eq!(proof.selected_files, 2);
    }

    #[test]
    fn durable_codex_observation_page_preserves_per_journal_totals_and_failures() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("source");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&source_root).unwrap();
        let retained = source_root.join("retained.jsonl");
        let removed = source_root.join("removed.jsonl");
        fs::write(&retained, "{}\n").unwrap();
        fs::write(&removed, "{}\n").unwrap();
        let owner = durable_owner(1, 0x81);
        let mut inventory = create_owned_durable_inventory(
            &data_root,
            durable_request(&source_root, 8),
            owner.clone(),
        );
        while !inventory.advance(&owner).unwrap().traversal_complete {}
        let journals = inventory.next_effects_page(&owner).unwrap();
        assert_eq!(journals.entries.len(), 2);
        fs::remove_file(&removed).unwrap();

        let page = crate::provider::codex::catalog::observe_codex_catalog_journal_page(
            &journals.entries,
            source_root.to_str().unwrap(),
            123,
        )
        .unwrap();
        assert_eq!(page.observations.len(), 2);
        assert_eq!(page.summary.source_files, 1);
        assert_eq!(page.summary.cataloged_sessions, 1);
        assert_eq!(page.summary.failed_sessions, 1);
        assert_eq!(
            page.observations
                .iter()
                .map(|observation| observation.journal.journal_identity)
                .collect::<BTreeSet<_>>(),
            journals
                .entries
                .iter()
                .map(|entry| entry.journal_identity)
                .collect::<BTreeSet<_>>()
        );
        assert_eq!(
            page.observations
                .iter()
                .map(|observation| observation.source_files)
                .sum::<u64>(),
            1
        );
        assert!(page.observations.iter().any(|observation| matches!(
            observation.outcome,
            crate::provider::codex::catalog::CodexCatalogObservationOutcome::Failed(_)
        )));
        let retried = crate::provider::codex::catalog::observe_codex_catalog_journal_page(
            &journals.entries,
            source_root.to_str().unwrap(),
            456,
        )
        .unwrap();
        assert_eq!(
            page.observations
                .iter()
                .map(|observation| (
                    observation.journal.journal_identity,
                    observation.effect_fingerprint
                ))
                .collect::<BTreeSet<_>>(),
            retried
                .observations
                .iter()
                .map(|observation| (
                    observation.journal.journal_identity,
                    observation.effect_fingerprint
                ))
                .collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn durable_inventory_fails_closed_for_missing_corrupt_or_tampered_scratch() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("source");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&source_root).unwrap();
        fs::write(source_root.join("path.jsonl"), "{}\n").unwrap();
        let request = durable_request(&source_root, 5);
        let inventory = DurableSourcePathInventory::create(&data_root, request.clone()).unwrap();
        let checkpoint = inventory.checkpoint().clone();
        let database = inventory
            .scratch
            .as_ref()
            .unwrap()
            .path()
            .join(DURABLE_INVENTORY_DATABASE_NAME);
        drop(inventory);

        let mut tampered = checkpoint.clone();
        tampered.scratch_identity.push(0xff);
        assert!(matches!(
            DurableSourcePathInventory::open(&data_root, request.clone(), &tampered),
            Err(DurableSourceInventoryError::TamperedScratch)
        ));

        fs::write(&database, b"not a sqlite database").unwrap();
        assert!(matches!(
            DurableSourcePathInventory::open(&data_root, request.clone(), &checkpoint),
            Err(DurableSourceInventoryError::CorruptScratch)
        ));

        let missing = durable_request(&source_root, 6);
        assert!(matches!(
            DurableSourcePathInventory::open(&data_root, missing, &checkpoint),
            Err(DurableSourceInventoryError::MissingScratch)
        ));
    }
}
