use std::{
    ffi::OsStr,
    fs::{self, ReadDir},
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{limits::Limit, params, Connection, OpenFlags, OptionalExtension, Row, Transaction};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use ctx_history_core::CaptureProvider;
#[cfg(test)]
use ctx_history_store::ImportInventoryOwnedPathIdentity;
use ctx_history_store::{
    canonical_import_inventory_selection_step, import_inventory_selection_commitment_identity,
    import_inventory_selection_initial_prefix, ImportInventoryCanonicalEffect,
    ImportInventoryCheckpointCleanupProof,
    ImportInventoryCleanupAdvance as StoreImportInventoryCleanupAdvance,
    ImportInventoryCleanupDisposition, ImportInventoryEffectMembership,
    ImportInventoryFrozenSelectionCommitment, ImportInventoryNativePathIdentity,
    ImportInventorySelectionCanonicalizationRequest, ProviderFileInventoryFamily,
    IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES, IMPORT_INVENTORY_SELECTION_ALGORITHM_VERSION,
    IMPORT_INVENTORY_SELECTION_FORMAT_VERSION,
};

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
const DURABLE_INVENTORY_FORMAT_VERSION: u32 = 2;
const DURABLE_INVENTORY_APPLICATION_ID: i64 = 0x4354_5849;
const DURABLE_INVENTORY_DATABASE_NAME: &str = "inventory.sqlite";
const DURABLE_INVENTORY_MAX_ID_BYTES: usize = 1024;
const DURABLE_INVENTORY_PAGE_ENTRIES: usize = 64;
const DURABLE_INVENTORY_SLICE_MAX_ELAPSED: Duration = Duration::from_millis(25);
const DURABLE_INVENTORY_RETRY_BASE_MS: i64 = 250;
const DURABLE_INVENTORY_RETRY_MAX_MS: i64 = 5 * 60 * 1000;
const DURABLE_INVENTORY_SQLITE_LENGTH_LIMIT: i32 = 1024 * 1024;
const DURABLE_INVENTORY_SQLITE_SQL_LIMIT: i32 = 64 * 1024;
const DURABLE_INVENTORY_MAX_NATIVE_PATH_BYTES: usize = 256 * 1024;
const DURABLE_INVENTORY_MAX_OBJECT_ID_BYTES: usize = 1024;
const DURABLE_INVENTORY_MAX_DECODED_PAGE_BYTES: usize = 2 * 1024 * 1024;

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
    pub scratch_database_identity: Vec<u8>,
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
    pub database_identity: Vec<u8>,
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
    pub traversal_complete: bool,
    pub queued_directories: u64,
    pub completed_directories: u64,
    pub active_directory: Option<DurableSourceInventoryActiveDirectory>,
    pub active_observed_entries: u64,
    pub replay_high_water_entries: u64,
    pub discovered_files: u64,
    pub selected_files: u64,
    pub selection_complete: bool,
    pub selection_cursor: Option<Vec<u8>>,
    pub selection_eof: bool,
    pub selection_commitment: Option<ImportInventoryFrozenSelectionCommitment>,
    pub planned_bytes: u64,
    pub rejected_effects: u64,
    pub application_ordinal: u64,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSourceInventorySelectionCandidate {
    pub group_key: Vec<u8>,
    pub rank: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSourceInventorySelectionDecision {
    pub journal_identity: [u8; 32],
    pub path_identity: NativePathIdentity,
    pub candidate: Option<DurableSourceInventorySelectionCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSourceInventorySelectionAdvance {
    pub next_cursor: Option<Vec<u8>>,
    pub eof: bool,
    pub processed_entries: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct DurableSourceInventoryEffectScope<'a> {
    pub inventory_family: ProviderFileInventoryFamily,
    pub provider: CaptureProvider,
    pub source_format: &'a str,
    pub source_root: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub struct DurableSourceInventoryEffectPlan<'a> {
    pub journal_identity: [u8; 32],
    pub accounted_bytes: u64,
    pub effect: ImportInventoryCanonicalEffect<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSourceInventoryEffectEntry {
    pub journal: DurableSourceInventoryJournalEntry,
    pub membership: ImportInventoryEffectMembership,
    pub accounted_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DurableSourceInventoryEffectPage {
    pub entries: Vec<DurableSourceInventoryEffectEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurableSourceInventoryMembershipAdvance {
    pub processed_entries: u64,
    pub complete: bool,
    pub commitment: Option<ImportInventoryFrozenSelectionCommitment>,
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

/// Scratch-local evidence that bounded discovery, selection, and effects have converged.
///
/// This is not source publication authority. The main store must still revalidate its stronger
/// source/root fingerprint, current owner, run, source, and generation before atomically
/// publishing completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSourceInventoryCompletionProof {
    pub checkpoint: DurableSourceInventoryCheckpoint,
    pub owner: DurableSourceInventoryOwner,
    pub scratch: DurableSourceInventoryScratch,
    pub discovered_files: u64,
    pub selected_files: u64,
    pub completed_directories: u64,
    pub selection_commitment: ImportInventoryFrozenSelectionCommitment,
    pub planned_bytes: u64,
    pub rejected_effects: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSourceInventoryCleanupAdvance {
    pub expected_cleanup_keyset: Option<Vec<u8>>,
    pub cleanup_keyset: Option<Vec<u8>>,
    pub visited_rows_delta: u64,
    pub cleaned_rows_delta: u64,
    pub cleaned_bytes_delta: u64,
    pub complete: bool,
}

impl DurableSourceInventoryCleanupAdvance {
    pub fn store_advance(&self) -> StoreImportInventoryCleanupAdvance<'_> {
        StoreImportInventoryCleanupAdvance {
            expected_cleanup_keyset: self.expected_cleanup_keyset.as_deref(),
            cleanup_keyset: self.cleanup_keyset.as_deref(),
            visited_rows_delta: self.visited_rows_delta,
            cleaned_rows_delta: self.cleaned_rows_delta,
            cleaned_bytes_delta: self.cleaned_bytes_delta,
            disposition: if self.complete {
                ImportInventoryCleanupDisposition::Complete
            } else {
                ImportInventoryCleanupDisposition::Pending
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurableSourceInventoryCleanupOutcome {
    Busy,
    Advance(DurableSourceInventoryCleanupAdvance),
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
    #[error("durable source inventory store cleanup proof does not match scratch state")]
    CleanupProofMismatch,
    #[error("durable source inventory scratch value exceeds the defensive decode limit: {0}")]
    OversizedScratchValue(&'static str),
    #[error("durable source inventory scratch page exceeds the aggregate decode limit")]
    DecodeBudgetExceeded,
    #[error("durable source inventory filesystem operation failed")]
    Filesystem(#[source] io::Error),
    #[error("durable source inventory scratch operation failed")]
    Scratch(#[source] rusqlite::Error),
    #[error("durable source inventory selection canonicalization failed")]
    Selection(#[source] ctx_history_store::StoreError),
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
    PRAGMA application_id = 1129601097;
    PRAGMA user_version = 2;
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
        scratch_database_identity BLOB NOT NULL,
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
        selection_cursor BLOB,
        selection_eof INTEGER NOT NULL,
        selection_complete INTEGER NOT NULL,
        pending_effects INTEGER NOT NULL,
        replay_count INTEGER NOT NULL,
        next_retry_at_ms INTEGER,
        last_error INTEGER,
        traversal_complete INTEGER NOT NULL,
        complete INTEGER NOT NULL,
        state_integrity BLOB NOT NULL,
        CHECK (format_version = 2),
        CHECK (length(build_identity) BETWEEN 1 AND 1024),
        CHECK (length(run_id) BETWEEN 1 AND 1024),
        CHECK (length(source_id) BETWEEN 1 AND 1024),
        CHECK (generation >= 0 AND mode IN (1, 2)),
        CHECK (length(scratch_nonce) = 16),
        CHECK (length(scratch_identity) BETWEEN 1 AND 1024),
        CHECK (length(scratch_integrity) = 32),
        CHECK (length(scratch_lock_identity) BETWEEN 1 AND 1024),
        CHECK (length(scratch_database_identity) BETWEEN 1 AND 1024),
        CHECK (root_platform IN (1, 2) AND root_encoding IN (1, 2)),
        CHECK (length(root_path) BETWEEN 1 AND 262144),
        CHECK (length(root_path_sha256) = 32),
        CHECK (length(root_object_identity) BETWEEN 1 AND 1024),
        CHECK ((owner_epoch IS NULL AND owner_token IS NULL)
            OR (owner_epoch > 0 AND length(owner_token) BETWEEN 16 AND 64)),
        CHECK (phase IN (1, 2, 3, 5)),
        CHECK (active_directory_identity IS NULL OR length(active_directory_identity) = 32),
        CHECK (active_observed_entries >= 0 AND replay_high_water_entries >= 0),
        CHECK (queued_directories >= 0 AND completed_directories >= 0),
        CHECK (discovered_files >= 0 AND selected_files >= 0 AND pending_effects >= 0),
        CHECK (selection_cursor IS NULL OR length(selection_cursor) BETWEEN 1 AND 262144),
        CHECK (selection_eof IN (0, 1) AND selection_complete IN (0, 1)),
        CHECK (replay_count >= 0 AND traversal_complete IN (0, 1) AND complete IN (0, 1)),
        CHECK (selection_eof = 0 OR traversal_complete = 1),
        CHECK (selection_complete = 0 OR selection_eof = 1),
        CHECK (complete = 0 OR (phase = 3 AND traversal_complete = 1
            AND selection_complete = 1 AND pending_effects = 0)),
        CHECK (length(state_integrity) = 32)
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
        ,CHECK (length(path_identity) = 32)
        ,CHECK (platform IN (1, 2) AND encoding IN (1, 2))
        ,CHECK (length(path) BETWEEN 1 AND 262144)
        ,CHECK (length(object_identity) BETWEEN 1 AND 1024)
        ,CHECK (length(directory_fingerprint) = 32)
        ,CHECK (state IN (0, 1, 2) AND attempt_count >= 0 AND replay_count >= 0)
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
        ,CHECK (length(journal_identity) = 32 AND length(path_identity) = 32)
        ,CHECK (platform IN (1, 2) AND encoding IN (1, 2))
        ,CHECK (length(path) BETWEEN 1 AND 262144)
        ,CHECK (length(directory_identity) = 32 AND state IN (0, 1))
    );
    CREATE INDEX path_journal_state_sequence
        ON path_journal(state, sequence);
    CREATE UNIQUE INDEX path_journal_path ON path_journal(path);
    CREATE TABLE selected_paths (
        group_key BLOB PRIMARY KEY NOT NULL,
        rank INTEGER NOT NULL,
        sort_key BLOB UNIQUE NOT NULL,
        path_identity BLOB UNIQUE NOT NULL REFERENCES path_journal(path_identity),
        effect_state INTEGER NOT NULL,
        CHECK (length(group_key) BETWEEN 1 AND 1024),
        CHECK (rank >= 0),
        CHECK (length(sort_key) BETWEEN 1 AND 262144),
        CHECK (length(path_identity) = 32),
        CHECK (effect_state IN (0, 1))
    ) WITHOUT ROWID;
    CREATE INDEX selected_paths_sort_key ON selected_paths(sort_key);
    CREATE INDEX selected_paths_effect_state_sort_key
        ON selected_paths(effect_state, sort_key);
    CREATE TABLE selection_state (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        path_selection_complete INTEGER NOT NULL,
        planning_cursor BLOB,
        planned_count INTEGER NOT NULL,
        planned_keyset BLOB,
        planned_prefix BLOB NOT NULL,
        planned_bytes INTEGER NOT NULL,
        rejected_effects INTEGER NOT NULL,
        application_ordinal INTEGER NOT NULL,
        frozen INTEGER NOT NULL,
        format_version INTEGER NOT NULL,
        algorithm_version INTEGER NOT NULL,
        final_count INTEGER,
        final_keyset BLOB,
        final_prefix BLOB,
        commitment_identity BLOB,
        state_integrity BLOB NOT NULL,
        CHECK (path_selection_complete IN (0, 1)),
        CHECK (planning_cursor IS NULL OR length(planning_cursor) BETWEEN 1 AND 262144),
        CHECK (planned_count >= 0 AND planned_bytes >= 0 AND rejected_effects >= 0),
        CHECK (rejected_effects <= planned_count),
        CHECK (planned_keyset IS NULL OR length(planned_keyset) = 32),
        CHECK ((planned_count = 0 AND planned_keyset IS NULL)
            OR (planned_count > 0 AND planned_keyset IS NOT NULL)),
        CHECK (length(planned_prefix) = 32),
        CHECK (application_ordinal BETWEEN 0 AND planned_count),
        CHECK (frozen IN (0, 1)),
        CHECK (format_version > 0 AND algorithm_version > 0),
        CHECK (
            (frozen = 0 AND final_count IS NULL AND final_keyset IS NULL
                AND final_prefix IS NULL AND commitment_identity IS NULL)
            OR (frozen = 1 AND final_count = planned_count
                AND final_prefix IS NOT NULL AND length(final_prefix) = 32
                AND commitment_identity IS NOT NULL AND length(commitment_identity) = 32
                AND ((final_count = 0 AND final_keyset IS NULL)
                    OR (final_count > 0 AND length(final_keyset) = 32)))
        ),
        CHECK (length(state_integrity) = 32)
    );
    CREATE TABLE effect_membership (
        ordinal INTEGER PRIMARY KEY,
        journal_identity BLOB UNIQUE NOT NULL,
        path_identity BLOB UNIQUE NOT NULL REFERENCES path_journal(path_identity),
        prior_keyset BLOB,
        resulting_keyset BLOB UNIQUE NOT NULL,
        prior_prefix BLOB NOT NULL,
        resulting_prefix BLOB NOT NULL,
        payload_fingerprint BLOB NOT NULL,
        member_digest BLOB NOT NULL,
        accounted_bytes INTEGER NOT NULL,
        rejected INTEGER NOT NULL,
        effect_state INTEGER NOT NULL,
        CHECK (ordinal >= 0),
        CHECK (length(journal_identity) = 32 AND length(path_identity) = 32),
        CHECK (prior_keyset IS NULL OR length(prior_keyset) = 32),
        CHECK ((ordinal = 0 AND prior_keyset IS NULL)
            OR (ordinal > 0 AND prior_keyset IS NOT NULL)),
        CHECK (length(resulting_keyset) = 32 AND resulting_keyset = journal_identity),
        CHECK (length(prior_prefix) = 32 AND length(resulting_prefix) = 32),
        CHECK (length(payload_fingerprint) = 32 AND length(member_digest) = 32),
        CHECK (accounted_bytes >= 0),
        CHECK (rejected IN (0, 1) AND effect_state IN (0, 1))
    );
    CREATE INDEX effect_membership_state_ordinal
        ON effect_membership(effect_state, ordinal);
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

struct DirectoryCollisionRecord {
    platform: i64,
    encoding: i64,
    path: Vec<u8>,
    object_identity: Vec<u8>,
    fingerprint: Vec<u8>,
}

struct FileCollisionRecord {
    journal_identity: Vec<u8>,
    platform: i64,
    encoding: i64,
    path: Vec<u8>,
    directory_identity: Vec<u8>,
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
    selection_cursor: Option<Vec<u8>>,
    selection_eof: bool,
    selection_complete: bool,
    pending_effects: u64,
    replay_count: u64,
    next_retry_at_ms: Option<i64>,
    last_error: Option<DurableSourceInventoryFailureKind>,
    traversal_complete: bool,
    complete: bool,
    state_integrity: [u8; 32],
}

struct DurableSelectionState {
    path_selection_complete: bool,
    planning_cursor: Option<Vec<u8>>,
    planned_count: u64,
    planned_keyset: Option<[u8; 32]>,
    planned_prefix: [u8; 32],
    planned_bytes: u64,
    rejected_effects: u64,
    application_ordinal: u64,
    frozen: bool,
    format_version: u32,
    algorithm_version: u32,
    final_count: Option<u64>,
    final_keyset: Option<[u8; 32]>,
    final_prefix: Option<[u8; 32]>,
    commitment_identity: Option<[u8; 32]>,
    state_integrity: [u8; 32],
}

struct DurableSelectionStateRow {
    path_selection_complete: i64,
    planning_cursor: Option<Vec<u8>>,
    planned_count: i64,
    planned_keyset: Option<Vec<u8>>,
    planned_prefix: Vec<u8>,
    planned_bytes: i64,
    rejected_effects: i64,
    application_ordinal: i64,
    frozen: i64,
    format_version: i64,
    algorithm_version: i64,
    final_count: Option<i64>,
    final_keyset: Option<Vec<u8>>,
    final_prefix: Option<Vec<u8>>,
    commitment_identity: Option<Vec<u8>>,
    state_integrity: Vec<u8>,
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
        let scratch_database_identity = scratch
            .file_identity(DURABLE_INVENTORY_DATABASE_NAME)
            .map_err(DurableSourceInventoryError::Filesystem)?;
        let mut connection = Connection::open(scratch.path().join(DURABLE_INVENTORY_DATABASE_NAME))
            .map_err(DurableSourceInventoryError::Scratch)?;
        configure_durable_inventory_connection(&connection)?;
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
            &scratch_database_identity,
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
            scratch_database_identity,
            root_identity: root.path.identity.clone(),
            root_object_identity: root.object_identity.clone(),
        };
        initialize_durable_inventory(&mut connection, &request, &checkpoint, &scratch_nonce, root)?;
        Ok(Self {
            connection: Some(connection),
            scratch: Some(scratch),
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
        let current_scratch_database_identity = scratch
            .file_identity(DURABLE_INVENTORY_DATABASE_NAME)
            .map_err(DurableSourceInventoryError::Filesystem)?;
        if current_scratch_database_identity != checkpoint.scratch_database_identity {
            return Err(DurableSourceInventoryError::TamperedScratch);
        }
        let connection = Connection::open_with_flags(
            database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)?;
        configure_durable_inventory_connection(&connection)?;
        validate_durable_inventory_schema(&connection)?;
        let meta = read_durable_meta(&connection)?;
        let selection = read_selection_state(&connection)?;
        validate_selection_meta_consistency(&meta, &selection)?;
        validate_resume_contract(&request, checkpoint, &requested_root, &meta)?;
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
            &current_scratch_database_identity,
        );
        if recomputed_integrity != checkpoint.scratch_integrity {
            return Err(DurableSourceInventoryError::TamperedScratch);
        }
        maybe_fail_durable_inventory(DurableInventoryFailurePoint::OpenAfterValidation)?;
        Ok(Self {
            connection: Some(connection),
            scratch: Some(scratch),
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
        seal_durable_state(&transaction)?;
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
                    self.active_reader = Some(ActiveDirectoryReader {
                        path_identity: active.path_identity,
                        entries,
                    });
                    if let Err(error) = maybe_fail_durable_inventory(
                        DurableInventoryFailurePoint::DirectoryOpenAfterSuccess,
                    ) {
                        self.active_reader = None;
                        return Err(error);
                    }
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
        let selection = read_selection_state(self.connection()?)?;
        validate_selection_meta_consistency(&meta, &selection)?;
        Ok(DurableSourceInventoryStatus {
            phase: meta.phase,
            traversal_complete: meta.traversal_complete,
            queued_directories: meta.queued_directories,
            completed_directories: meta.completed_directories,
            active_directory: read_active_directory(self.connection()?, &self.checkpoint, &meta)?,
            active_observed_entries: meta.active_observed_entries,
            replay_high_water_entries: meta.replay_high_water_entries,
            discovered_files: meta.discovered_files,
            selected_files: meta.selected_files,
            selection_complete: meta.selection_complete,
            selection_cursor: meta.selection_cursor,
            selection_eof: meta.selection_eof,
            selection_commitment: selection.commitment()?,
            planned_bytes: selection.planned_bytes,
            rejected_effects: selection.rejected_effects,
            application_ordinal: selection.application_ordinal,
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

    pub fn apply_path_selection_page(
        &mut self,
        owner: &DurableSourceInventoryOwner,
        expected_cursor: Option<&[u8]>,
        decisions: &[DurableSourceInventorySelectionDecision],
    ) -> DurableInventoryResult<DurableSourceInventorySelectionAdvance> {
        if decisions.len() > DURABLE_INVENTORY_PAGE_ENTRIES {
            return Err(DurableSourceInventoryError::InvalidRequest(
                "path selection page exceeds the internal row bound",
            ));
        }
        let write_bytes = decisions.iter().try_fold(0_u64, |total, decision| {
            let candidate_bytes = match &decision.candidate {
                Some(candidate)
                    if candidate.group_key.is_empty()
                        || candidate.group_key.len() > DURABLE_INVENTORY_MAX_ID_BYTES =>
                {
                    return Err(DurableSourceInventoryError::InvalidRequest(
                        "path selection group identity is not bounded",
                    ));
                }
                Some(candidate) => candidate.group_key.len() as u64,
                None => 0,
            };
            Ok(total.saturating_add(candidate_bytes).saturating_add(160))
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
        let meta = read_durable_meta(&transaction)?;
        if meta.checkpoint.mode != DurableSourceInventoryMode::RegularFiles
            || !meta.traversal_complete
            || meta.selection_eof
            || meta.selection_complete
            || meta.selection_cursor.as_deref() != expected_cursor
        {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        let page = read_durable_path_page(
            &transaction,
            &self.request,
            &self.checkpoint,
            expected_cursor,
            DURABLE_INVENTORY_PAGE_ENTRIES,
            false,
        )?;
        if page.entries.len() != decisions.len()
            || page.entries.iter().zip(decisions).any(|(entry, decision)| {
                entry.journal_identity != decision.journal_identity
                    || entry.path_identity != decision.path_identity
            })
        {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        let mut inserted_groups = 0_u64;
        for (entry, decision) in page.entries.iter().zip(decisions) {
            let Some(candidate) = &decision.candidate else {
                continue;
            };
            let encoded_path = encode_path(&entry.path);
            let rank = i64::try_from(candidate.rank).map_err(|_| {
                DurableSourceInventoryError::InvalidRequest("path selection rank exceeds SQLite")
            })?;
            let existed = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM selected_paths WHERE group_key = ?1)",
                    params![candidate.group_key],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(DurableSourceInventoryError::Scratch)?;
            transaction
                .execute(
                    "INSERT INTO selected_paths (
                        group_key, rank, sort_key, path_identity, effect_state
                     ) VALUES (?1, ?2, ?3, ?4, 0)
                     ON CONFLICT(group_key) DO UPDATE SET
                        rank = excluded.rank,
                        sort_key = excluded.sort_key,
                        path_identity = excluded.path_identity,
                        effect_state = 0
                     WHERE excluded.rank < selected_paths.rank
                        OR (excluded.rank = selected_paths.rank
                            AND excluded.sort_key < selected_paths.sort_key)",
                    params![
                        candidate.group_key,
                        rank,
                        encoded_path,
                        entry.path_identity.sha256.as_slice()
                    ],
                )
                .map_err(DurableSourceInventoryError::Scratch)?;
            if !existed {
                inserted_groups = inserted_groups.saturating_add(1);
            }
        }
        let next_cursor = page
            .next_keyset
            .clone()
            .or_else(|| expected_cursor.map(|cursor| cursor.to_vec()));
        let changed = transaction
            .execute(
                "UPDATE inventory_meta
                 SET selected_files = selected_files + ?1,
                     selection_cursor = ?2, selection_eof = ?3
                 WHERE singleton = 1 AND selection_cursor IS ?4 AND selection_eof = 0",
                params![
                    u64_i64(inserted_groups)?,
                    next_cursor,
                    i64::from(page.complete),
                    expected_cursor,
                ],
            )
            .map_err(DurableSourceInventoryError::Scratch)?;
        if changed != 1 {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        maybe_fail_durable_inventory(DurableInventoryFailurePoint::SelectionPageBeforeCommit)?;
        seal_durable_state(&transaction)?;
        transaction
            .commit()
            .map_err(DurableSourceInventoryError::Scratch)?;
        Ok(DurableSourceInventorySelectionAdvance {
            next_cursor,
            eof: page.complete,
            processed_entries: u64::try_from(page.entries.len()).unwrap_or(u64::MAX),
        })
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
        let meta = read_durable_meta(&transaction)?;
        if meta.checkpoint.mode != DurableSourceInventoryMode::RegularFiles
            || !meta.traversal_complete
            || !meta.selection_eof
        {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        let selection = read_selection_state(&transaction)?;
        if selection.path_selection_complete {
            transaction
                .commit()
                .map_err(DurableSourceInventoryError::Scratch)?;
            return Ok(());
        }
        if selection.planned_count != 0 || selection.frozen {
            return Err(DurableSourceInventoryError::CorruptScratch);
        }
        let has_unselected = match meta.selection_cursor.as_deref() {
            Some(cursor) => transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM path_journal WHERE path > ?1 LIMIT 1)",
                params![cursor],
                |row| row.get::<_, bool>(0),
            ),
            None => transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM path_journal LIMIT 1)",
                [],
                |row| row.get::<_, bool>(0),
            ),
        }
        .map_err(DurableSourceInventoryError::Scratch)?;
        if has_unselected {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        let changed = transaction
            .execute(
                "UPDATE selection_state
                 SET path_selection_complete = 1
                 WHERE singleton = 1 AND path_selection_complete = 0 AND frozen = 0",
                [],
            )
            .map_err(DurableSourceInventoryError::Scratch)?;
        if changed != 1 {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        seal_selection_state(&transaction)?;
        transaction
            .commit()
            .map_err(DurableSourceInventoryError::Scratch)
    }

    /// Freezes effects for current scratch journal members. Source rows absent from this inventory
    /// are handled by the Store checkpoint's separate bounded reconciliation phase.
    pub fn plan_effect_membership_page(
        &mut self,
        owner: &DurableSourceInventoryOwner,
        scope: DurableSourceInventoryEffectScope<'_>,
        plans: &[DurableSourceInventoryEffectPlan<'_>],
    ) -> DurableInventoryResult<DurableSourceInventoryMembershipAdvance> {
        if plans.len() > DURABLE_INVENTORY_PAGE_ENTRIES {
            return Err(DurableSourceInventoryError::InvalidRequest(
                "effect membership page exceeds the internal row bound",
            ));
        }
        let estimated_write_bytes = plans.iter().try_fold(0_u64, |total, plan| {
            if plan.accounted_bytes > DURABLE_INVENTORY_MAX_DECODED_PAGE_BYTES as u64 {
                return Err(DurableSourceInventoryError::InvalidRequest(
                    "effect membership accounted bytes exceed the page envelope",
                ));
            }
            Ok(total.saturating_add(320))
        })?;
        let accounted_page_bytes = plans.iter().try_fold(0_u64, |total, plan| {
            total.checked_add(plan.accounted_bytes).ok_or(
                DurableSourceInventoryError::InvalidRequest(
                    "effect membership page byte counter overflow",
                ),
            )
        })?;
        if accounted_page_bytes > IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES as u64 {
            return Err(DurableSourceInventoryError::InvalidRequest(
                "effect membership page exceeds the shared byte envelope",
            ));
        }
        crate::pace_current_filesystem_operation(estimated_write_bytes);
        crate::pace_current_disk_io(estimated_write_bytes);

        let mode = self.request.mode;
        let request = self.request.clone();
        let checkpoint = self.checkpoint.clone();
        let transaction = self
            .connection_mut()?
            .transaction()
            .map_err(DurableSourceInventoryError::Scratch)?;
        assert_durable_owner_transaction(&transaction, owner)?;
        let meta = read_durable_meta(&transaction)?;
        let selection = read_selection_state(&transaction)?;
        validate_selection_meta_consistency(&meta, &selection)?;
        if !meta.traversal_complete
            || !selection.path_selection_complete
            || selection.frozen
            || meta.selection_complete
        {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        let page = read_durable_path_page(
            &transaction,
            &request,
            &checkpoint,
            selection.planning_cursor.as_deref(),
            DURABLE_INVENTORY_PAGE_ENTRIES,
            mode == DurableSourceInventoryMode::RegularFiles,
        )?;
        if page.entries.len() != plans.len()
            || page
                .entries
                .iter()
                .zip(plans)
                .any(|(entry, plan)| entry.journal_identity != plan.journal_identity)
        {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }

        let mut prior_keyset = selection.planned_keyset;
        let mut prior_prefix = selection.planned_prefix;
        let mut planned_bytes = selection.planned_bytes;
        let mut rejected_effects = selection.rejected_effects;
        for (index, (entry, plan)) in page.entries.iter().zip(plans).enumerate() {
            let ordinal = selection
                .planned_count
                .checked_add(u64::try_from(index).unwrap_or(u64::MAX))
                .ok_or(DurableSourceInventoryError::InvalidRequest(
                    "effect membership ordinal overflow",
                ))?;
            let resulting_keyset = entry.journal_identity;
            let canonical = canonical_import_inventory_selection_step(
                ImportInventorySelectionCanonicalizationRequest {
                    format_version: selection.format_version,
                    algorithm_version: selection.algorithm_version,
                    ordinal,
                    capture_journal_identity: &entry.journal_identity,
                    native_path: ImportInventoryNativePathIdentity {
                        platform_tag: entry.path_identity.platform.as_str(),
                        encoding_tag: entry.path_identity.encoding.as_str(),
                        opaque_hash: &entry.path_identity.sha256,
                    },
                    inventory_family: scope.inventory_family,
                    provider: scope.provider,
                    source_format: scope.source_format,
                    source_root: scope.source_root,
                    prior_keyset: prior_keyset.as_ref(),
                    resulting_keyset: &resulting_keyset,
                    prior_prefix: &prior_prefix,
                    accounted_bytes: plan.accounted_bytes,
                    effect: plan.effect,
                },
            )
            .map_err(DurableSourceInventoryError::Selection)?;
            let rejected = canonical_effect_is_rejected(plan.effect);
            transaction
                .execute(
                    "INSERT INTO effect_membership (
                        ordinal, journal_identity, path_identity, prior_keyset,
                        resulting_keyset, prior_prefix, resulting_prefix,
                        payload_fingerprint, member_digest, accounted_bytes,
                        rejected, effect_state
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0)",
                    params![
                        u64_i64(ordinal)?,
                        entry.journal_identity.as_slice(),
                        entry.path_identity.sha256.as_slice(),
                        prior_keyset.as_ref().map(<[u8; 32]>::as_slice),
                        resulting_keyset.as_slice(),
                        prior_prefix.as_slice(),
                        canonical.resulting_prefix.as_slice(),
                        canonical.payload_fingerprint.as_slice(),
                        canonical.member_digest.as_slice(),
                        u64_i64(plan.accounted_bytes)?,
                        i64::from(rejected),
                    ],
                )
                .map_err(DurableSourceInventoryError::Scratch)?;
            planned_bytes = planned_bytes.checked_add(plan.accounted_bytes).ok_or(
                DurableSourceInventoryError::InvalidRequest(
                    "effect membership byte counter overflow",
                ),
            )?;
            rejected_effects = rejected_effects.saturating_add(u64::from(rejected));
            prior_keyset = Some(resulting_keyset);
            prior_prefix = canonical.resulting_prefix;
        }

        let processed = u64::try_from(page.entries.len()).unwrap_or(u64::MAX);
        let planned_count = selection.planned_count.checked_add(processed).ok_or(
            DurableSourceInventoryError::InvalidRequest("effect membership count overflow"),
        )?;
        let planning_cursor = page
            .next_keyset
            .or_else(|| selection.planning_cursor.clone());
        let commitment = if page.complete {
            let expected_count = match mode {
                DurableSourceInventoryMode::Jsonl => meta.discovered_files,
                DurableSourceInventoryMode::RegularFiles => meta.selected_files,
            };
            if planned_count != expected_count {
                return Err(DurableSourceInventoryError::CorruptScratch);
            }
            let commitment = ImportInventoryFrozenSelectionCommitment {
                format_version: selection.format_version,
                algorithm_version: selection.algorithm_version,
                total_count: planned_count,
                final_keyset: prior_keyset,
                final_prefix: prior_prefix,
            };
            let identity = import_inventory_selection_commitment_identity(commitment)
                .map_err(DurableSourceInventoryError::Selection)?;
            let selection_changed = transaction
                .execute(
                    "UPDATE selection_state
                     SET planning_cursor = ?1, planned_count = ?2, planned_keyset = ?3,
                         planned_prefix = ?4, planned_bytes = ?5, rejected_effects = ?6,
                         frozen = 1, final_count = ?2, final_keyset = ?3,
                         final_prefix = ?4, commitment_identity = ?7
                     WHERE singleton = 1 AND frozen = 0 AND planned_count = ?8",
                    params![
                        planning_cursor,
                        u64_i64(planned_count)?,
                        prior_keyset.as_ref().map(<[u8; 32]>::as_slice),
                        prior_prefix.as_slice(),
                        u64_i64(planned_bytes)?,
                        u64_i64(rejected_effects)?,
                        identity.as_slice(),
                        u64_i64(selection.planned_count)?,
                    ],
                )
                .map_err(DurableSourceInventoryError::Scratch)?;
            if selection_changed != 1 {
                return Err(DurableSourceInventoryError::CheckpointMismatch);
            }
            let meta_changed = transaction
                .execute(
                    "UPDATE inventory_meta
                     SET selection_complete = 1, pending_effects = ?1,
                         phase = CASE WHEN ?1 = 0 THEN 3 ELSE 2 END,
                         complete = CASE WHEN ?1 = 0 THEN 1 ELSE 0 END
                     WHERE singleton = 1 AND selection_complete = 0 AND pending_effects = 0",
                    params![u64_i64(planned_count)?],
                )
                .map_err(DurableSourceInventoryError::Scratch)?;
            if meta_changed != 1 {
                return Err(DurableSourceInventoryError::CheckpointMismatch);
            }
            Some(commitment)
        } else {
            let changed = transaction
                .execute(
                    "UPDATE selection_state
                     SET planning_cursor = ?1, planned_count = ?2, planned_keyset = ?3,
                         planned_prefix = ?4, planned_bytes = ?5, rejected_effects = ?6
                     WHERE singleton = 1 AND frozen = 0 AND planned_count = ?7",
                    params![
                        planning_cursor,
                        u64_i64(planned_count)?,
                        prior_keyset.as_ref().map(<[u8; 32]>::as_slice),
                        prior_prefix.as_slice(),
                        u64_i64(planned_bytes)?,
                        u64_i64(rejected_effects)?,
                        u64_i64(selection.planned_count)?,
                    ],
                )
                .map_err(DurableSourceInventoryError::Scratch)?;
            if changed != 1 {
                return Err(DurableSourceInventoryError::CheckpointMismatch);
            }
            None
        };
        maybe_fail_durable_inventory(DurableInventoryFailurePoint::MembershipPageBeforeCommit)?;
        seal_selection_state(&transaction)?;
        seal_durable_state(&transaction)?;
        transaction
            .commit()
            .map_err(DurableSourceInventoryError::Scratch)?;
        maybe_fail_durable_inventory(DurableInventoryFailurePoint::MembershipPageAfterCommit)?;
        Ok(DurableSourceInventoryMembershipAdvance {
            processed_entries: processed,
            complete: page.complete,
            commitment,
        })
    }

    pub fn next_membership_candidates_page(
        &self,
        owner: &DurableSourceInventoryOwner,
    ) -> DurableInventoryResult<DurableSourceInventoryPathPage> {
        let connection = self.connection()?;
        assert_durable_owner(connection, owner)?;
        let meta = read_durable_meta(connection)?;
        let selection = read_selection_state(connection)?;
        validate_selection_meta_consistency(&meta, &selection)?;
        if !meta.traversal_complete || !selection.path_selection_complete || selection.frozen {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        read_durable_path_page(
            connection,
            &self.request,
            &self.checkpoint,
            selection.planning_cursor.as_deref(),
            DURABLE_INVENTORY_PAGE_ENTRIES,
            self.request.mode == DurableSourceInventoryMode::RegularFiles,
        )
    }

    pub fn selected_paths_page(
        &self,
        owner: &DurableSourceInventoryOwner,
        after: Option<&[u8]>,
        limit: usize,
    ) -> DurableInventoryResult<DurableSourceInventoryPathPage> {
        let connection = self.connection()?;
        assert_durable_owner(connection, owner)?;
        if self.request.mode != DurableSourceInventoryMode::RegularFiles
            || !read_selection_state(connection)?.path_selection_complete
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
        if self.request.mode != DurableSourceInventoryMode::RegularFiles
            || !read_selection_state(connection)?.path_selection_complete
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
    ) -> DurableInventoryResult<DurableSourceInventoryEffectPage> {
        let connection = self.connection()?;
        assert_durable_owner(connection, owner)?;
        let meta = read_durable_meta(connection)?;
        let selection = read_selection_state(connection)?;
        validate_selection_meta_consistency(&meta, &selection)?;
        let Some(commitment) = selection.commitment()? else {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        };
        let commitment_identity = selection
            .commitment_identity
            .ok_or(DurableSourceInventoryError::CorruptScratch)?;
        crate::pace_current_disk_io(SCRATCH_PATH_READ_BASE_BYTES.saturating_add(
            SCRATCH_PATH_READ_BYTES_PER_ROW.saturating_mul(DURABLE_INVENTORY_PAGE_ENTRIES as u64),
        ));
        let (mut expected_keyset, mut expected_prefix) = if selection.application_ordinal == 0 {
            (
                None,
                import_inventory_selection_initial_prefix(
                    commitment.format_version,
                    commitment.algorithm_version,
                )
                .map_err(DurableSourceInventoryError::Selection)?,
            )
        } else {
            let previous = connection
                .query_row(
                    "SELECT resulting_keyset, resulting_prefix
                     FROM effect_membership
                     WHERE ordinal = ?1 AND effect_state = 1",
                    params![u64_i64(selection.application_ordinal - 1)?],
                    |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .optional()
                .map_err(DurableSourceInventoryError::Scratch)?
                .ok_or(DurableSourceInventoryError::CorruptScratch)?;
            (
                Some(fixed_identity(previous.0)?),
                fixed_identity(previous.1)?,
            )
        };
        let mut statement = connection
            .prepare(
                "SELECT j.journal_identity, j.path_identity, j.platform, j.encoding, j.path,
                        d.path_identity, d.platform, d.encoding, d.object_identity,
                        d.directory_fingerprint, m.ordinal, m.prior_keyset,
                        m.resulting_keyset, m.prior_prefix, m.resulting_prefix,
                        m.payload_fingerprint, m.member_digest, m.accounted_bytes, m.rejected
                 FROM effect_membership AS m
                 JOIN path_journal AS j ON j.path_identity = m.path_identity
                 JOIN directory_queue AS d ON d.path_identity = j.directory_identity
                 WHERE m.effect_state = 0 AND m.ordinal >= ?1
                 ORDER BY m.ordinal LIMIT 64",
            )
            .map_err(DurableSourceInventoryError::Scratch)?;
        let mut rows = statement
            .query(params![u64_i64(selection.application_ordinal)?])
            .map_err(DurableSourceInventoryError::Scratch)?;
        let mut entries = Vec::with_capacity(DURABLE_INVENTORY_PAGE_ENTRIES);
        let mut decoded = DecodeBudget::new(DURABLE_INVENTORY_MAX_DECODED_PAGE_BYTES);
        while let Some(row) = rows.next().map_err(DurableSourceInventoryError::Scratch)? {
            let journal =
                decode_durable_journal_entry(row, &self.request, &self.checkpoint, &mut decoded)?;
            let ordinal = nonnegative_u64(row_i64(row, 10)?)?;
            let prior_keyset =
                row_optional_bounded_blob(row, 11, 32, "effect prior keyset", &mut decoded)?
                    .map(fixed_identity)
                    .transpose()?;
            let resulting_keyset = fixed_identity(row_bounded_blob(
                row,
                12,
                32,
                "effect resulting keyset",
                &mut decoded,
            )?)?;
            let prior_prefix = fixed_identity(row_bounded_blob(
                row,
                13,
                32,
                "effect prior prefix",
                &mut decoded,
            )?)?;
            let resulting_prefix = fixed_identity(row_bounded_blob(
                row,
                14,
                32,
                "effect resulting prefix",
                &mut decoded,
            )?)?;
            let _payload_fingerprint = fixed_identity(row_bounded_blob(
                row,
                15,
                32,
                "effect payload fingerprint",
                &mut decoded,
            )?)?;
            let _member_digest = fixed_identity(row_bounded_blob(
                row,
                16,
                32,
                "effect member digest",
                &mut decoded,
            )?)?;
            let accounted_bytes = nonnegative_u64(row_i64(row, 17)?)?;
            let _rejected = decode_bool(row_i64(row, 18)?)?;
            let expected_ordinal = selection
                .application_ordinal
                .checked_add(u64::try_from(entries.len()).unwrap_or(u64::MAX))
                .ok_or(DurableSourceInventoryError::CorruptScratch)?;
            if ordinal != expected_ordinal
                || prior_keyset != expected_keyset
                || prior_prefix != expected_prefix
                || resulting_keyset != journal.journal_identity
                || ordinal >= commitment.total_count
                || accounted_bytes > DURABLE_INVENTORY_MAX_DECODED_PAGE_BYTES as u64
            {
                return Err(DurableSourceInventoryError::CorruptScratch);
            }
            entries.push(DurableSourceInventoryEffectEntry {
                journal,
                membership: ImportInventoryEffectMembership {
                    commitment_identity,
                    ordinal,
                    prior_keyset,
                    resulting_keyset,
                    prior_prefix,
                    resulting_prefix,
                },
                accounted_bytes,
            });
            expected_keyset = Some(resulting_keyset);
            expected_prefix = resulting_prefix;
        }
        let page_end = selection
            .application_ordinal
            .checked_add(u64::try_from(entries.len()).unwrap_or(u64::MAX))
            .ok_or(DurableSourceInventoryError::CorruptScratch)?;
        if (page_end < commitment.total_count && entries.is_empty())
            || (page_end == commitment.total_count
                && (expected_keyset != commitment.final_keyset
                    || expected_prefix != commitment.final_prefix))
            || page_end > commitment.total_count
        {
            return Err(DurableSourceInventoryError::CorruptScratch);
        }
        Ok(DurableSourceInventoryEffectPage { entries })
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
        let meta = read_durable_meta(&transaction)?;
        let selection = read_selection_state(&transaction)?;
        validate_selection_meta_consistency(&meta, &selection)?;
        if !selection.frozen {
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        if journal_identities.is_empty() {
            transaction
                .commit()
                .map_err(DurableSourceInventoryError::Scratch)?;
            return Ok(());
        }
        let count = u64::try_from(journal_identities.len()).unwrap_or(u64::MAX);
        let current_matches = effect_identity_range_matches(
            &transaction,
            selection.application_ordinal,
            journal_identities,
            0,
        )?;
        if !current_matches {
            if selection.application_ordinal >= count
                && effect_identity_range_matches(
                    &transaction,
                    selection.application_ordinal - count,
                    journal_identities,
                    1,
                )?
            {
                transaction
                    .commit()
                    .map_err(DurableSourceInventoryError::Scratch)?;
                return Ok(());
            }
            return Err(DurableSourceInventoryError::CheckpointMismatch);
        }
        for (offset, identity) in journal_identities.iter().enumerate() {
            let ordinal = selection
                .application_ordinal
                .checked_add(u64::try_from(offset).unwrap_or(u64::MAX))
                .ok_or(DurableSourceInventoryError::CorruptScratch)?;
            let membership_changed = transaction
                .execute(
                    "UPDATE effect_membership SET effect_state = 1
                     WHERE ordinal = ?1 AND journal_identity = ?2 AND effect_state = 0",
                    params![u64_i64(ordinal)?, identity.as_slice()],
                )
                .map_err(DurableSourceInventoryError::Scratch)?;
            let journal_changed = transaction
                .execute(
                    "UPDATE path_journal SET state = 1
                     WHERE journal_identity = ?1 AND state = 0",
                    params![identity.as_slice()],
                )
                .map_err(DurableSourceInventoryError::Scratch)?;
            let selected_changed = if mode == DurableSourceInventoryMode::RegularFiles {
                transaction
                    .execute(
                        "UPDATE selected_paths SET effect_state = 1
                         WHERE effect_state = 0 AND path_identity = (
                            SELECT path_identity FROM path_journal WHERE journal_identity = ?1
                         )",
                        params![identity.as_slice()],
                    )
                    .map_err(DurableSourceInventoryError::Scratch)?
            } else {
                1
            };
            if membership_changed != 1 || journal_changed != 1 || selected_changed != 1 {
                return Err(DurableSourceInventoryError::CorruptScratch);
            }
        }
        let next_ordinal = selection
            .application_ordinal
            .checked_add(count)
            .ok_or(DurableSourceInventoryError::CorruptScratch)?;
        let selection_changed = transaction
            .execute(
                "UPDATE selection_state SET application_ordinal = ?1
                 WHERE singleton = 1 AND application_ordinal = ?2 AND frozen = 1",
                params![
                    u64_i64(next_ordinal)?,
                    u64_i64(selection.application_ordinal)?
                ],
            )
            .map_err(DurableSourceInventoryError::Scratch)?;
        let meta_changed = transaction
            .execute(
                "UPDATE inventory_meta SET pending_effects = pending_effects - ?1
                 WHERE singleton = 1 AND pending_effects >= ?1",
                params![u64_i64(count)?],
            )
            .map_err(DurableSourceInventoryError::Scratch)?;
        if selection_changed != 1 || meta_changed != 1 {
            return Err(DurableSourceInventoryError::CorruptScratch);
        }
        publish_complete_if_ready(&transaction, owner)?;
        seal_selection_state(&transaction)?;
        seal_durable_state(&transaction)?;
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
            || !meta.selection_eof
            || !meta.selection_complete
            || meta.active_directory_identity.is_some()
            || meta.queued_directories != 0
            || meta.pending_effects != 0
        {
            return Ok(None);
        }
        verify_scratch_completion(self.connection()?, &meta)?;
        let selection = read_selection_state(self.connection()?)?;
        let selection_commitment = selection
            .commitment()?
            .ok_or(DurableSourceInventoryError::CorruptScratch)?;
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
            selection_commitment,
            planned_bytes: selection.planned_bytes,
            rejected_effects: selection.rejected_effects,
        }))
    }

    pub fn cleanup_checkpoint(
        data_root: &Path,
        request: &DurableSourceInventoryRequest,
        proof: &ImportInventoryCheckpointCleanupProof,
        expected_cleanup_keyset: Option<&[u8]>,
    ) -> DurableInventoryResult<DurableSourceInventoryCleanupOutcome> {
        validate_durable_request(data_root, request)?;
        let root = native_path_descriptor(&request.root)?;
        let scratch_name = durable_inventory_scratch_name(request, &root.identity);
        validate_cleanup_proof(request, proof, expected_cleanup_keyset, &root.identity)?;
        let scratch = match DurableCaptureScratch::open_for_cleanup(data_root, &scratch_name) {
            Ok(scratch) => scratch,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(DurableSourceInventoryCleanupOutcome::Advance(
                    cleanup_advance(proof, expected_cleanup_keyset, 0, 0, true),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(DurableSourceInventoryCleanupOutcome::Busy);
            }
            Err(error) => return Err(map_durable_scratch_open_error(error)),
        };
        if scratch
            .directory_identity()
            .map_err(DurableSourceInventoryError::Filesystem)?
            != proof.scratch_identity
            || scratch
                .lock_identity()
                .map_err(DurableSourceInventoryError::Filesystem)?
                != proof.scratch_lock_identity
        {
            return Err(DurableSourceInventoryError::CleanupProofMismatch);
        }
        validate_cleanup_scratch(&scratch, request, proof, &root)?;
        let progress = scratch
            .cleanup_owned_slice(IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES as u64)
            .map_err(DurableSourceInventoryError::Filesystem)?;
        Ok(match progress.outcome {
            DurableScratchCleanupOutcome::Complete => {
                DurableSourceInventoryCleanupOutcome::Advance(cleanup_advance(
                    proof,
                    expected_cleanup_keyset,
                    progress.cleaned_entries,
                    progress.cleaned_bytes,
                    true,
                ))
            }
            DurableScratchCleanupOutcome::Pending
                if progress.cleaned_entries == 0 && progress.cleaned_bytes == 0 =>
            {
                DurableSourceInventoryCleanupOutcome::Busy
            }
            DurableScratchCleanupOutcome::Pending => {
                DurableSourceInventoryCleanupOutcome::Advance(cleanup_advance(
                    proof,
                    expected_cleanup_keyset,
                    progress.cleaned_entries,
                    progress.cleaned_bytes,
                    false,
                ))
            }
            DurableScratchCleanupOutcome::Busy => DurableSourceInventoryCleanupOutcome::Busy,
        })
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
        let selection = read_selection_state(self.connection()?)?;
        validate_selection_meta_consistency(&meta, &selection)?;
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
            database_identity: self.checkpoint.scratch_database_identity.clone(),
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
        RootKind::File => (5_i64, 0_i64, 1_i64, 1_i64, 0_i64, 1_i64),
    };
    let selection_eof =
        i64::from(request.mode == DurableSourceInventoryMode::Jsonl && traversal_complete == 1);
    let selection_complete = 0_i64;
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
                scratch_lock_identity, scratch_database_identity,
                root_platform, root_encoding, root_path,
                root_path_sha256, root_object_identity, owner_epoch, owner_token, phase,
                active_directory_identity, active_observed_entries, replay_high_water_entries,
                queued_directories,
                completed_directories, discovered_files, selected_files,
                selection_cursor, selection_eof, selection_complete,
                pending_effects, replay_count,
                next_retry_at_ms, last_error, traversal_complete, complete, state_integrity
             ) VALUES (
                1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, NULL, NULL, ?17, NULL, 0, 0, ?18, ?19, ?20, 0, NULL,
                ?21, ?22, ?23, 0, NULL, NULL, ?24, 0, ?25
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
                checkpoint.scratch_database_identity,
                encode_platform(root.path.identity.platform),
                encode_encoding(root.path.identity.encoding),
                root.path.encoded,
                root.path.identity.sha256.as_slice(),
                root.object_identity,
                phase,
                queued_directories,
                completed_directories,
                discovered_files,
                selection_eof,
                selection_complete,
                pending_effects,
                traversal_complete,
                [0_u8; 32].as_slice(),
            ],
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    let initial_prefix = import_inventory_selection_initial_prefix(
        IMPORT_INVENTORY_SELECTION_FORMAT_VERSION,
        IMPORT_INVENTORY_SELECTION_ALGORITHM_VERSION,
    )
    .map_err(DurableSourceInventoryError::Selection)?;
    transaction
        .execute(
            "INSERT INTO selection_state (
                singleton, path_selection_complete, planning_cursor, planned_count,
                planned_keyset, planned_prefix, planned_bytes, rejected_effects,
                application_ordinal, frozen, format_version, algorithm_version,
                final_count, final_keyset, final_prefix, commitment_identity, state_integrity
             ) VALUES (
                1, ?1, NULL, 0, NULL, ?2, 0, 0, 0, 0, ?3, ?4,
                NULL, NULL, NULL, NULL, ?5
             )",
            params![
                i64::from(request.mode == DurableSourceInventoryMode::Jsonl),
                initial_prefix.as_slice(),
                i64::from(IMPORT_INVENTORY_SELECTION_FORMAT_VERSION),
                i64::from(IMPORT_INVENTORY_SELECTION_ALGORITHM_VERSION),
                [0_u8; 32].as_slice(),
            ],
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    seal_selection_state(&transaction)?;
    let directory_state = match root.kind {
        RootKind::Directory => 0_i64,
        RootKind::File => 2_i64,
    };
    seal_durable_state(&transaction)?;
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

fn configure_durable_inventory_connection(connection: &Connection) -> DurableInventoryResult<()> {
    connection.set_limit(
        Limit::SQLITE_LIMIT_LENGTH,
        DURABLE_INVENTORY_SQLITE_LENGTH_LIMIT,
    );
    connection.set_limit(
        Limit::SQLITE_LIMIT_SQL_LENGTH,
        DURABLE_INVENTORY_SQLITE_SQL_LIMIT,
    );
    connection.set_limit(Limit::SQLITE_LIMIT_COLUMN, 64);
    connection.set_limit(Limit::SQLITE_LIMIT_EXPR_DEPTH, 64);
    connection.set_limit(Limit::SQLITE_LIMIT_COMPOUND_SELECT, 8);
    connection.set_limit(Limit::SQLITE_LIMIT_VDBE_OP, 100_000);
    connection.set_limit(Limit::SQLITE_LIMIT_FUNCTION_ARG, 32);
    connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0);
    connection.set_limit(Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH, 1024);
    connection.set_limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 128);
    connection.set_limit(Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 0);
    connection.set_limit(Limit::SQLITE_LIMIT_WORKER_THREADS, 0);
    connection
        .execute_batch("PRAGMA foreign_keys = ON; PRAGMA trusted_schema = OFF;")
        .map_err(DurableSourceInventoryError::Scratch)
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
               AND name IN (
                 'inventory_meta', 'directory_queue', 'path_journal', 'selected_paths',
                 'selection_state', 'effect_membership'
               )",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)?;
    if application_id != DURABLE_INVENTORY_APPLICATION_ID
        || user_version != i64::from(DURABLE_INVENTORY_FORMAT_VERSION)
        || table_count != 6
    {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
    Ok(())
}

fn read_durable_meta(connection: &Connection) -> DurableInventoryResult<DurableInventoryMeta> {
    let meta = read_durable_meta_unverified(connection)?;
    if durable_state_integrity(&meta) != meta.state_integrity {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
    Ok(meta)
}

impl DurableSelectionState {
    fn commitment(
        &self,
    ) -> DurableInventoryResult<Option<ImportInventoryFrozenSelectionCommitment>> {
        if !self.frozen {
            if self.final_count.is_some()
                || self.final_keyset.is_some()
                || self.final_prefix.is_some()
                || self.commitment_identity.is_some()
            {
                return Err(DurableSourceInventoryError::CorruptScratch);
            }
            return Ok(None);
        }
        let commitment = ImportInventoryFrozenSelectionCommitment {
            format_version: self.format_version,
            algorithm_version: self.algorithm_version,
            total_count: self
                .final_count
                .ok_or(DurableSourceInventoryError::CorruptScratch)?,
            final_keyset: self.final_keyset,
            final_prefix: self
                .final_prefix
                .ok_or(DurableSourceInventoryError::CorruptScratch)?,
        };
        let identity = import_inventory_selection_commitment_identity(commitment)
            .map_err(DurableSourceInventoryError::Selection)?;
        if self.commitment_identity != Some(identity) {
            return Err(DurableSourceInventoryError::CorruptScratch);
        }
        Ok(Some(commitment))
    }
}

fn read_selection_state(connection: &Connection) -> DurableInventoryResult<DurableSelectionState> {
    let mut statement = connection
        .prepare(
            "SELECT path_selection_complete, planning_cursor, planned_count, planned_keyset,
                    planned_prefix, planned_bytes, rejected_effects, application_ordinal,
                    frozen, format_version, algorithm_version, final_count, final_keyset,
                    final_prefix, commitment_identity, state_integrity
             FROM selection_state WHERE singleton = 1",
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    let mut rows = statement
        .query([])
        .map_err(DurableSourceInventoryError::Scratch)?;
    let row = rows
        .next()
        .map_err(DurableSourceInventoryError::Scratch)?
        .ok_or(DurableSourceInventoryError::CorruptScratch)?;
    let mut decoded = DecodeBudget::new(DURABLE_INVENTORY_MAX_DECODED_PAGE_BYTES);
    let state = DurableSelectionState {
        path_selection_complete: decode_bool(row_i64(row, 0)?)?,
        planning_cursor: row_optional_bounded_blob(
            row,
            1,
            DURABLE_INVENTORY_MAX_NATIVE_PATH_BYTES,
            "selection planning cursor",
            &mut decoded,
        )?,
        planned_count: nonnegative_u64(row_i64(row, 2)?)?,
        planned_keyset: row_optional_bounded_blob(
            row,
            3,
            32,
            "selection planned keyset",
            &mut decoded,
        )?
        .map(fixed_identity)
        .transpose()?,
        planned_prefix: fixed_identity(row_bounded_blob(
            row,
            4,
            32,
            "selection planned prefix",
            &mut decoded,
        )?)?,
        planned_bytes: nonnegative_u64(row_i64(row, 5)?)?,
        rejected_effects: nonnegative_u64(row_i64(row, 6)?)?,
        application_ordinal: nonnegative_u64(row_i64(row, 7)?)?,
        frozen: decode_bool(row_i64(row, 8)?)?,
        format_version: nonnegative_u32(row_i64(row, 9)?)?,
        algorithm_version: nonnegative_u32(row_i64(row, 10)?)?,
        final_count: row_optional_i64(row, 11)?
            .map(nonnegative_u64)
            .transpose()?,
        final_keyset: row_optional_bounded_blob(
            row,
            12,
            32,
            "selection final keyset",
            &mut decoded,
        )?
        .map(fixed_identity)
        .transpose()?,
        final_prefix: row_optional_bounded_blob(
            row,
            13,
            32,
            "selection final prefix",
            &mut decoded,
        )?
        .map(fixed_identity)
        .transpose()?,
        commitment_identity: row_optional_bounded_blob(
            row,
            14,
            32,
            "selection commitment identity",
            &mut decoded,
        )?
        .map(fixed_identity)
        .transpose()?,
        state_integrity: fixed_identity(row_bounded_blob(
            row,
            15,
            32,
            "selection state integrity",
            &mut decoded,
        )?)?,
    };
    if rows
        .next()
        .map_err(DurableSourceInventoryError::Scratch)?
        .is_some()
        || selection_state_integrity(&state) != state.state_integrity
        || state.format_version != IMPORT_INVENTORY_SELECTION_FORMAT_VERSION
        || state.algorithm_version != IMPORT_INVENTORY_SELECTION_ALGORITHM_VERSION
        || (state.planned_count == 0) != state.planned_keyset.is_none()
        || state.rejected_effects > state.planned_count
        || state.application_ordinal > state.planned_count
        || (state.frozen && !state.path_selection_complete)
    {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
    if let Some(commitment) = state.commitment()? {
        if commitment.total_count != state.planned_count
            || commitment.final_keyset != state.planned_keyset
            || commitment.final_prefix != state.planned_prefix
        {
            return Err(DurableSourceInventoryError::CorruptScratch);
        }
    }
    Ok(state)
}

fn selection_state_integrity(state: &DurableSelectionState) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-durable-inventory-selection-state-v1\0");
    hasher.update([u8::from(state.path_selection_complete)]);
    hash_optional_inventory_field(&mut hasher, state.planning_cursor.as_deref());
    hasher.update(state.planned_count.to_be_bytes());
    hash_optional_inventory_field(
        &mut hasher,
        state.planned_keyset.as_ref().map(<[u8; 32]>::as_slice),
    );
    hasher.update(state.planned_prefix);
    hasher.update(state.planned_bytes.to_be_bytes());
    hasher.update(state.rejected_effects.to_be_bytes());
    hasher.update(state.application_ordinal.to_be_bytes());
    hasher.update([u8::from(state.frozen)]);
    hasher.update(state.format_version.to_be_bytes());
    hasher.update(state.algorithm_version.to_be_bytes());
    hash_optional_inventory_field(
        &mut hasher,
        state
            .final_count
            .as_ref()
            .map(|value| value.to_be_bytes())
            .as_deref(),
    );
    hash_optional_inventory_field(
        &mut hasher,
        state.final_keyset.as_ref().map(<[u8; 32]>::as_slice),
    );
    hash_optional_inventory_field(
        &mut hasher,
        state.final_prefix.as_ref().map(<[u8; 32]>::as_slice),
    );
    hash_optional_inventory_field(
        &mut hasher,
        state.commitment_identity.as_ref().map(<[u8; 32]>::as_slice),
    );
    hasher.finalize().into()
}

fn validate_selection_meta_consistency(
    meta: &DurableInventoryMeta,
    selection: &DurableSelectionState,
) -> DurableInventoryResult<()> {
    let expected_paths = match meta.checkpoint.mode {
        DurableSourceInventoryMode::Jsonl => meta.discovered_files,
        DurableSourceInventoryMode::RegularFiles => meta.selected_files,
    };
    if meta.selection_complete != selection.frozen
        || selection.application_ordinal > selection.planned_count
        || (selection.frozen
            && meta.pending_effects
                != selection
                    .planned_count
                    .saturating_sub(selection.application_ordinal))
        || (!selection.frozen && (selection.application_ordinal != 0 || meta.pending_effects != 0))
        || (selection.frozen && selection.planned_count != expected_paths)
        || (meta.complete && selection.application_ordinal != selection.planned_count)
    {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
    Ok(())
}

fn seal_selection_state(transaction: &Transaction<'_>) -> DurableInventoryResult<()> {
    let state = read_selection_state_unverified(transaction)?;
    let integrity = selection_state_integrity(&state);
    let changed = transaction
        .execute(
            "UPDATE selection_state SET state_integrity = ?1 WHERE singleton = 1",
            params![integrity.as_slice()],
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    if changed != 1 {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
    Ok(())
}

fn read_selection_state_unverified(
    connection: &Connection,
) -> DurableInventoryResult<DurableSelectionState> {
    let state = connection
        .query_row(
            "SELECT path_selection_complete, planning_cursor, planned_count, planned_keyset,
                    planned_prefix, planned_bytes, rejected_effects, application_ordinal,
                    frozen, format_version, algorithm_version, final_count, final_keyset,
                    final_prefix, commitment_identity, state_integrity
             FROM selection_state WHERE singleton = 1",
            [],
            |row| {
                Ok(DurableSelectionStateRow {
                    path_selection_complete: row.get(0)?,
                    planning_cursor: row.get(1)?,
                    planned_count: row.get(2)?,
                    planned_keyset: row.get(3)?,
                    planned_prefix: row.get(4)?,
                    planned_bytes: row.get(5)?,
                    rejected_effects: row.get(6)?,
                    application_ordinal: row.get(7)?,
                    frozen: row.get(8)?,
                    format_version: row.get(9)?,
                    algorithm_version: row.get(10)?,
                    final_count: row.get(11)?,
                    final_keyset: row.get(12)?,
                    final_prefix: row.get(13)?,
                    commitment_identity: row.get(14)?,
                    state_integrity: row.get(15)?,
                })
            },
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    Ok(DurableSelectionState {
        path_selection_complete: decode_bool(state.path_selection_complete)?,
        planning_cursor: state.planning_cursor,
        planned_count: nonnegative_u64(state.planned_count)?,
        planned_keyset: state.planned_keyset.map(fixed_identity).transpose()?,
        planned_prefix: fixed_identity(state.planned_prefix)?,
        planned_bytes: nonnegative_u64(state.planned_bytes)?,
        rejected_effects: nonnegative_u64(state.rejected_effects)?,
        application_ordinal: nonnegative_u64(state.application_ordinal)?,
        frozen: decode_bool(state.frozen)?,
        format_version: nonnegative_u32(state.format_version)?,
        algorithm_version: nonnegative_u32(state.algorithm_version)?,
        final_count: state.final_count.map(nonnegative_u64).transpose()?,
        final_keyset: state.final_keyset.map(fixed_identity).transpose()?,
        final_prefix: state.final_prefix.map(fixed_identity).transpose()?,
        commitment_identity: state.commitment_identity.map(fixed_identity).transpose()?,
        state_integrity: fixed_identity(state.state_integrity)?,
    })
}

fn read_durable_meta_unverified(
    connection: &Connection,
) -> DurableInventoryResult<DurableInventoryMeta> {
    let mut statement = connection
        .prepare(
            "SELECT
                format_version, build_identity, run_id, source_id, generation, mode,
                scratch_nonce, scratch_identity, scratch_integrity, scratch_lock_identity,
                scratch_database_identity, root_platform, root_encoding, root_path,
                root_path_sha256, root_object_identity, owner_epoch, owner_token, phase,
                active_directory_identity, active_observed_entries, replay_high_water_entries,
                queued_directories, completed_directories, discovered_files, selected_files,
                selection_cursor, selection_eof, selection_complete, pending_effects, replay_count,
                next_retry_at_ms, last_error, traversal_complete, complete, state_integrity
             FROM inventory_meta WHERE singleton = 1",
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    let mut rows = statement
        .query([])
        .map_err(DurableSourceInventoryError::Scratch)?;
    let row = rows
        .next()
        .map_err(DurableSourceInventoryError::Scratch)?
        .ok_or(DurableSourceInventoryError::CorruptScratch)?;
    let mut decoded = DecodeBudget::new(DURABLE_INVENTORY_MAX_DECODED_PAGE_BYTES);
    let owner_epoch = row_optional_i64(row, 16)?;
    let owner_token = row_optional_bounded_blob(row, 17, 64, "owner token", &mut decoded)?;
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
    let platform = decode_platform(row_i64(row, 11)?)?;
    let encoding = decode_encoding(row_i64(row, 12)?)?;
    let meta = DurableInventoryMeta {
        checkpoint: DurableSourceInventoryCheckpoint {
            format_version: nonnegative_u32(row_i64(row, 0)?)?,
            build_identity: row_bounded_blob(
                row,
                1,
                DURABLE_INVENTORY_MAX_ID_BYTES,
                "build identity",
                &mut decoded,
            )?,
            run_id: row_bounded_blob(
                row,
                2,
                DURABLE_INVENTORY_MAX_ID_BYTES,
                "run identity",
                &mut decoded,
            )?,
            source_id: row_bounded_blob(
                row,
                3,
                DURABLE_INVENTORY_MAX_ID_BYTES,
                "source identity",
                &mut decoded,
            )?,
            generation: nonnegative_u64(row_i64(row, 4)?)?,
            mode: decode_mode(row_i64(row, 5)?)?,
            scratch_identity: row_bounded_blob(
                row,
                7,
                DURABLE_INVENTORY_MAX_ID_BYTES,
                "scratch identity",
                &mut decoded,
            )?,
            scratch_integrity: fixed_identity(row_bounded_blob(
                row,
                8,
                32,
                "scratch integrity",
                &mut decoded,
            )?)?,
            scratch_lock_identity: row_bounded_blob(
                row,
                9,
                DURABLE_INVENTORY_MAX_ID_BYTES,
                "scratch lock identity",
                &mut decoded,
            )?,
            scratch_database_identity: row_bounded_blob(
                row,
                10,
                DURABLE_INVENTORY_MAX_ID_BYTES,
                "scratch database identity",
                &mut decoded,
            )?,
            root_identity: NativePathIdentity {
                platform,
                encoding,
                sha256: fixed_identity(row_bounded_blob(
                    row,
                    14,
                    32,
                    "root path identity",
                    &mut decoded,
                )?)?,
            },
            root_object_identity: row_bounded_blob(
                row,
                15,
                DURABLE_INVENTORY_MAX_OBJECT_ID_BYTES,
                "root object identity",
                &mut decoded,
            )?,
        },
        scratch_nonce: fixed_token(row_bounded_blob(row, 6, 16, "scratch nonce", &mut decoded)?)?,
        owner,
        root_path: row_bounded_blob(
            row,
            13,
            DURABLE_INVENTORY_MAX_NATIVE_PATH_BYTES,
            "root path",
            &mut decoded,
        )?,
        phase: decode_phase(row_i64(row, 18)?)?,
        active_directory_identity: row_optional_bounded_blob(
            row,
            19,
            32,
            "active directory identity",
            &mut decoded,
        )?
        .map(fixed_identity)
        .transpose()?,
        active_observed_entries: nonnegative_u64(row_i64(row, 20)?)?,
        replay_high_water_entries: nonnegative_u64(row_i64(row, 21)?)?,
        queued_directories: nonnegative_u64(row_i64(row, 22)?)?,
        completed_directories: nonnegative_u64(row_i64(row, 23)?)?,
        discovered_files: nonnegative_u64(row_i64(row, 24)?)?,
        selected_files: nonnegative_u64(row_i64(row, 25)?)?,
        selection_cursor: row_optional_bounded_blob(
            row,
            26,
            DURABLE_INVENTORY_MAX_NATIVE_PATH_BYTES,
            "selection cursor",
            &mut decoded,
        )?,
        selection_eof: decode_bool(row_i64(row, 27)?)?,
        selection_complete: decode_bool(row_i64(row, 28)?)?,
        pending_effects: nonnegative_u64(row_i64(row, 29)?)?,
        replay_count: nonnegative_u64(row_i64(row, 30)?)?,
        next_retry_at_ms: row_optional_i64(row, 31)?,
        last_error: row_optional_i64(row, 32)?
            .map(decode_failure_kind)
            .transpose()?,
        traversal_complete: decode_bool(row_i64(row, 33)?)?,
        complete: decode_bool(row_i64(row, 34)?)?,
        state_integrity: fixed_identity(row_bounded_blob(
            row,
            35,
            32,
            "state integrity",
            &mut decoded,
        )?)?,
    };
    if rows
        .next()
        .map_err(DurableSourceInventoryError::Scratch)?
        .is_some()
    {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
    validate_durable_meta_shape(&meta)?;
    Ok(meta)
}

struct DecodeBudget {
    remaining: usize,
}

impl DecodeBudget {
    fn new(limit: usize) -> Self {
        Self { remaining: limit }
    }

    fn reserve(&mut self, bytes: usize) -> DurableInventoryResult<()> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or(DurableSourceInventoryError::DecodeBudgetExceeded)?;
        Ok(())
    }
}

fn row_i64(row: &Row<'_>, index: usize) -> DurableInventoryResult<i64> {
    row.get(index)
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)
}

fn row_optional_i64(row: &Row<'_>, index: usize) -> DurableInventoryResult<Option<i64>> {
    row.get(index)
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)
}

fn row_bounded_blob(
    row: &Row<'_>,
    index: usize,
    max_bytes: usize,
    label: &'static str,
    budget: &mut DecodeBudget,
) -> DurableInventoryResult<Vec<u8>> {
    let value = row
        .get_ref(index)
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)?;
    let bytes = value
        .as_blob()
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)?;
    if bytes.len() > max_bytes {
        return Err(DurableSourceInventoryError::OversizedScratchValue(label));
    }
    budget.reserve(bytes.len())?;
    Ok(bytes.to_vec())
}

fn row_optional_bounded_blob(
    row: &Row<'_>,
    index: usize,
    max_bytes: usize,
    label: &'static str,
    budget: &mut DecodeBudget,
) -> DurableInventoryResult<Option<Vec<u8>>> {
    let value = row
        .get_ref(index)
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)?;
    if matches!(value, rusqlite::types::ValueRef::Null) {
        return Ok(None);
    }
    let bytes = value
        .as_blob()
        .map_err(|_| DurableSourceInventoryError::CorruptScratch)?;
    if bytes.len() > max_bytes {
        return Err(DurableSourceInventoryError::OversizedScratchValue(label));
    }
    budget.reserve(bytes.len())?;
    Ok(Some(bytes.to_vec()))
}

fn validate_durable_meta_shape(meta: &DurableInventoryMeta) -> DurableInventoryResult<()> {
    if meta.checkpoint.format_version != DURABLE_INVENTORY_FORMAT_VERSION
        || meta.selected_files > meta.discovered_files
        || meta.active_observed_entries > meta.replay_high_water_entries
        || (meta.active_directory_identity.is_none() && meta.active_observed_entries != 0)
        || (meta.selection_eof && !meta.traversal_complete)
        || (meta.selection_complete && !meta.selection_eof)
        || (meta.complete && meta.pending_effects != 0)
        || (meta.complete != (meta.phase == DurableSourceInventoryPhase::Complete))
    {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
    match meta.checkpoint.mode {
        DurableSourceInventoryMode::Jsonl => {
            if meta.selection_cursor.is_some()
                || (meta.traversal_complete && !meta.selection_eof)
                || (!meta.traversal_complete && meta.selection_eof)
            {
                return Err(DurableSourceInventoryError::CorruptScratch);
            }
            if meta.pending_effects > meta.discovered_files {
                return Err(DurableSourceInventoryError::CorruptScratch);
            }
        }
        DurableSourceInventoryMode::RegularFiles => {
            if meta.pending_effects > meta.selected_files {
                return Err(DurableSourceInventoryError::CorruptScratch);
            }
        }
    }
    match meta.phase {
        DurableSourceInventoryPhase::Traversal if meta.traversal_complete => {
            return Err(DurableSourceInventoryError::CorruptScratch)
        }
        DurableSourceInventoryPhase::Selection
            if !meta.traversal_complete || meta.selection_complete =>
        {
            return Err(DurableSourceInventoryError::CorruptScratch)
        }
        DurableSourceInventoryPhase::Effects
            if !meta.traversal_complete
                || !meta.selection_complete
                || meta.pending_effects == 0 =>
        {
            return Err(DurableSourceInventoryError::CorruptScratch)
        }
        DurableSourceInventoryPhase::Complete
            if !meta.traversal_complete
                || !meta.selection_complete
                || meta.pending_effects != 0 =>
        {
            return Err(DurableSourceInventoryError::CorruptScratch)
        }
        _ => {}
    }
    Ok(())
}

fn durable_state_integrity(meta: &DurableInventoryMeta) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-durable-inventory-state-v1\0");
    hasher.update(meta.checkpoint.format_version.to_be_bytes());
    hash_inventory_field(&mut hasher, &meta.checkpoint.build_identity);
    hash_inventory_field(&mut hasher, &meta.checkpoint.run_id);
    hash_inventory_field(&mut hasher, &meta.checkpoint.source_id);
    hasher.update(meta.checkpoint.generation.to_be_bytes());
    hasher.update([encode_mode(meta.checkpoint.mode) as u8]);
    hash_inventory_field(&mut hasher, &meta.checkpoint.scratch_identity);
    hasher.update(meta.checkpoint.scratch_integrity);
    hash_inventory_field(&mut hasher, &meta.checkpoint.scratch_lock_identity);
    hash_inventory_field(&mut hasher, &meta.checkpoint.scratch_database_identity);
    hasher.update([encode_platform(meta.checkpoint.root_identity.platform) as u8]);
    hasher.update([encode_encoding(meta.checkpoint.root_identity.encoding) as u8]);
    hasher.update(meta.checkpoint.root_identity.sha256);
    hash_inventory_field(&mut hasher, &meta.checkpoint.root_object_identity);
    hasher.update(meta.scratch_nonce);
    hash_inventory_field(&mut hasher, &meta.root_path);
    match &meta.owner {
        Some(owner) => {
            hasher.update([1]);
            hasher.update(owner.epoch.to_be_bytes());
            hash_inventory_field(&mut hasher, &owner.token);
        }
        None => hasher.update([0]),
    }
    hasher.update([encode_phase(meta.phase) as u8]);
    hash_optional_inventory_field(
        &mut hasher,
        meta.active_directory_identity
            .as_ref()
            .map(|value| value.as_slice()),
    );
    for value in [
        meta.active_observed_entries,
        meta.replay_high_water_entries,
        meta.queued_directories,
        meta.completed_directories,
        meta.discovered_files,
        meta.selected_files,
        meta.pending_effects,
        meta.replay_count,
    ] {
        hasher.update(value.to_be_bytes());
    }
    hash_optional_inventory_field(&mut hasher, meta.selection_cursor.as_deref());
    hasher.update([
        u8::from(meta.selection_eof),
        u8::from(meta.selection_complete),
        u8::from(meta.traversal_complete),
        u8::from(meta.complete),
    ]);
    hash_optional_i64(&mut hasher, meta.next_retry_at_ms);
    hash_optional_i64(&mut hasher, meta.last_error.map(encode_failure_kind));
    hasher.finalize().into()
}

fn hash_optional_inventory_field(hasher: &mut Sha256, value: Option<&[u8]>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_inventory_field(hasher, value);
        }
        None => hasher.update([0]),
    }
}

fn hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_be_bytes());
        }
        None => hasher.update([0]),
    }
}

fn seal_durable_state(transaction: &Transaction<'_>) -> DurableInventoryResult<()> {
    let meta = read_durable_meta_unverified(transaction)?;
    let integrity = durable_state_integrity(&meta);
    let changed = transaction
        .execute(
            "UPDATE inventory_meta SET state_integrity = ?1 WHERE singleton = 1",
            params![integrity.as_slice()],
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    if changed != 1 {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
    Ok(())
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
        database_identity: checkpoint.scratch_database_identity.clone(),
    }
}

fn decode_durable_journal_entry(
    row: &Row<'_>,
    request: &DurableSourceInventoryRequest,
    checkpoint: &DurableSourceInventoryCheckpoint,
    decoded: &mut DecodeBudget,
) -> DurableInventoryResult<DurableSourceInventoryJournalEntry> {
    let journal_id = fixed_identity(row_bounded_blob(row, 0, 32, "journal identity", decoded)?)?;
    let path_sha256 = fixed_identity(row_bounded_blob(row, 1, 32, "path identity", decoded)?)?;
    let platform = decode_platform(row_i64(row, 2)?)?;
    let encoding = decode_encoding(row_i64(row, 3)?)?;
    let path_bytes = row_bounded_blob(
        row,
        4,
        DURABLE_INVENTORY_MAX_NATIVE_PATH_BYTES,
        "journal path",
        decoded,
    )?;
    let directory_sha256 = fixed_identity(row_bounded_blob(
        row,
        5,
        32,
        "directory path identity",
        decoded,
    )?)?;
    let directory_platform = decode_platform(row_i64(row, 6)?)?;
    let directory_encoding = decode_encoding(row_i64(row, 7)?)?;
    let directory_identity = row_bounded_blob(
        row,
        8,
        DURABLE_INVENTORY_MAX_OBJECT_ID_BYTES,
        "directory object identity",
        decoded,
    )?;
    let directory_fingerprint = fixed_identity(row_bounded_blob(
        row,
        9,
        32,
        "directory fingerprint",
        decoded,
    )?)?;
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
    if after.is_some_and(|cursor| cursor.len() > DURABLE_INVENTORY_MAX_NATIVE_PATH_BYTES) {
        return Err(DurableSourceInventoryError::InvalidRequest(
            "durable path page cursor exceeds its byte bound",
        ));
    }
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
    let mut decoded = DecodeBudget::new(DURABLE_INVENTORY_MAX_DECODED_PAGE_BYTES);
    while let Some(row) = rows.next().map_err(DurableSourceInventoryError::Scratch)? {
        entries.push(decode_durable_journal_entry(
            row,
            request,
            checkpoint,
            &mut decoded,
        )?);
        next_keyset = Some(row_bounded_blob(
            row,
            10,
            DURABLE_INVENTORY_MAX_NATIVE_PATH_BYTES,
            "path page keyset",
            &mut decoded,
        )?);
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

fn canonical_effect_is_rejected(effect: ImportInventoryCanonicalEffect<'_>) -> bool {
    matches!(
        effect,
        ImportInventoryCanonicalEffect::CatalogObservationRejected { .. }
            | ImportInventoryCanonicalEffect::SourceImportObservationRejected { .. }
    )
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
    scratch_database_identity: &[u8],
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
    hash_inventory_field(&mut hasher, scratch_database_identity);
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
        seal_durable_state(&transaction)?;
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
        seal_durable_state(&transaction)?;
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
    seal_durable_state(&transaction)?;
    transaction
        .commit()
        .map_err(DurableSourceInventoryError::Scratch)?;
    maybe_fail_durable_inventory(DurableInventoryFailurePoint::ActiveTransitionAfterCommit)?;
    Ok(ActiveDirectoryPreparation::Ready(active))
}

fn read_first_queued_directory(
    connection: &Connection,
) -> DurableInventoryResult<Option<ActiveDirectoryRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT path_identity, platform, encoding, path, object_identity,
                    directory_fingerprint, attempt_count, replay_count, next_retry_at_ms
             FROM directory_queue WHERE state = 0 ORDER BY sequence LIMIT 1",
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    let mut rows = statement
        .query([])
        .map_err(DurableSourceInventoryError::Scratch)?;
    rows.next()
        .map_err(DurableSourceInventoryError::Scratch)?
        .map(decode_directory_record_row)
        .transpose()
}

fn read_directory_record(
    connection: &Connection,
    identity: &[u8; 32],
    expected_state: i64,
) -> DurableInventoryResult<ActiveDirectoryRecord> {
    let mut statement = connection
        .prepare(
            "SELECT path_identity, platform, encoding, path, object_identity,
                    directory_fingerprint, attempt_count, replay_count, next_retry_at_ms
             FROM directory_queue WHERE path_identity = ?1 AND state = ?2",
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    let mut rows = statement
        .query(params![identity.as_slice(), expected_state])
        .map_err(DurableSourceInventoryError::Scratch)?;
    let row = rows
        .next()
        .map_err(DurableSourceInventoryError::Scratch)?
        .ok_or(DurableSourceInventoryError::CorruptScratch)?;
    decode_directory_record_row(row)
}

fn decode_directory_record_row(row: &Row<'_>) -> DurableInventoryResult<ActiveDirectoryRecord> {
    let mut decoded = DecodeBudget::new(DURABLE_INVENTORY_MAX_DECODED_PAGE_BYTES);
    let identity = row_bounded_blob(row, 0, 32, "directory path identity", &mut decoded)?;
    let platform = decode_platform(row_i64(row, 1)?)?;
    let encoding = decode_encoding(row_i64(row, 2)?)?;
    let path = row_bounded_blob(
        row,
        3,
        DURABLE_INVENTORY_MAX_NATIVE_PATH_BYTES,
        "directory path",
        &mut decoded,
    )?;
    let object_identity = row_bounded_blob(
        row,
        4,
        DURABLE_INVENTORY_MAX_OBJECT_ID_BYTES,
        "directory object identity",
        &mut decoded,
    )?;
    let fingerprint = row_bounded_blob(row, 5, 32, "directory fingerprint", &mut decoded)?;
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
        attempt_count: nonnegative_u64(row_i64(row, 6)?)?,
        replay_count: nonnegative_u64(row_i64(row, 7)?)?,
        next_retry_at_ms: row_optional_i64(row, 8)?,
    })
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
    let mut statement = transaction
        .prepare("SELECT active_directory_identity FROM inventory_meta WHERE singleton = 1")
        .map_err(DurableSourceInventoryError::Scratch)?;
    let mut rows = statement
        .query([])
        .map_err(DurableSourceInventoryError::Scratch)?;
    let row = rows
        .next()
        .map_err(DurableSourceInventoryError::Scratch)?
        .ok_or(DurableSourceInventoryError::CorruptScratch)?;
    let mut decoded = DecodeBudget::new(32);
    let active = row_optional_bounded_blob(row, 0, 32, "active directory identity", &mut decoded)?;
    drop(rows);
    drop(statement);
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
                     last_error = NULL
             WHERE singleton = 1",
            params![
                u64_i64(observed_entries)?,
                u64_i64(queued_added)?,
                u64_i64(files_added)?,
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
    seal_durable_state(&transaction)?;
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
    let mut statement = transaction
        .prepare(
            "SELECT platform, encoding, path, object_identity, directory_fingerprint
             FROM directory_queue WHERE path_identity = ?1",
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    let mut rows = statement
        .query(params![path.identity.sha256.as_slice()])
        .map_err(DurableSourceInventoryError::Scratch)?;
    let row = rows
        .next()
        .map_err(DurableSourceInventoryError::Scratch)?
        .ok_or(DurableSourceInventoryError::CorruptScratch)?;
    let mut decoded = DecodeBudget::new(DURABLE_INVENTORY_MAX_DECODED_PAGE_BYTES);
    let existing = DirectoryCollisionRecord {
        platform: row_i64(row, 0)?,
        encoding: row_i64(row, 1)?,
        path: row_bounded_blob(
            row,
            2,
            DURABLE_INVENTORY_MAX_NATIVE_PATH_BYTES,
            "directory collision path",
            &mut decoded,
        )?,
        object_identity: row_bounded_blob(
            row,
            3,
            DURABLE_INVENTORY_MAX_OBJECT_ID_BYTES,
            "directory collision identity",
            &mut decoded,
        )?,
        fingerprint: row_bounded_blob(row, 4, 32, "directory collision fingerprint", &mut decoded)?,
    };
    if existing.platform != encode_platform(path.identity.platform)
        || existing.encoding != encode_encoding(path.identity.encoding)
        || existing.path.as_slice() != path.encoded.as_slice()
        || existing.object_identity.as_slice() != object_identity
        || existing.fingerprint.as_slice() != fingerprint.as_slice()
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
    let mut statement = transaction
        .prepare(
            "SELECT journal_identity, platform, encoding, path, directory_identity
             FROM path_journal WHERE path_identity = ?1",
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    let mut rows = statement
        .query(params![path.identity.sha256.as_slice()])
        .map_err(DurableSourceInventoryError::Scratch)?;
    let row = rows
        .next()
        .map_err(DurableSourceInventoryError::Scratch)?
        .ok_or(DurableSourceInventoryError::CorruptScratch)?;
    let mut decoded = DecodeBudget::new(DURABLE_INVENTORY_MAX_DECODED_PAGE_BYTES);
    let existing = FileCollisionRecord {
        journal_identity: row_bounded_blob(row, 0, 32, "journal collision identity", &mut decoded)?,
        platform: row_i64(row, 1)?,
        encoding: row_i64(row, 2)?,
        path: row_bounded_blob(
            row,
            3,
            DURABLE_INVENTORY_MAX_NATIVE_PATH_BYTES,
            "journal collision path",
            &mut decoded,
        )?,
        directory_identity: row_bounded_blob(
            row,
            4,
            32,
            "journal collision directory",
            &mut decoded,
        )?,
    };
    if existing.journal_identity.as_slice() != journal_identity.as_slice()
        || existing.platform != encode_platform(path.identity.platform)
        || existing.encoding != encode_encoding(path.identity.encoding)
        || existing.path.as_slice() != path.encoded.as_slice()
        || existing.directory_identity.as_slice() != directory_identity.as_slice()
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
    let mut statement = transaction
        .prepare(
            "SELECT queued_directories, active_directory_identity
             FROM inventory_meta WHERE singleton = 1",
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    let mut rows = statement
        .query([])
        .map_err(DurableSourceInventoryError::Scratch)?;
    let row = rows
        .next()
        .map_err(DurableSourceInventoryError::Scratch)?
        .ok_or(DurableSourceInventoryError::CorruptScratch)?;
    let queued = row_i64(row, 0)?;
    let mut decoded = DecodeBudget::new(32);
    let active = row_optional_bounded_blob(row, 1, 32, "active directory identity", &mut decoded)?;
    let has_queued = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM directory_queue WHERE state = 0 LIMIT 1)",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    let has_active = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM directory_queue WHERE state = 1 LIMIT 1)",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    if (queued == 0) == has_queued || active.is_some() != has_active {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
    if let Some(active) = active.as_deref() {
        let active_matches = transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM directory_queue WHERE path_identity = ?1 AND state = 1
                 )",
                params![active],
                |row| row.get::<_, bool>(0),
            )
            .map_err(DurableSourceInventoryError::Scratch)?;
        if !active_matches {
            return Err(DurableSourceInventoryError::CorruptScratch);
        }
    }
    if !has_queued && !has_active {
        transaction
            .execute(
                "UPDATE inventory_meta
                 SET traversal_complete = 1,
                     selection_eof = CASE WHEN mode = 1 THEN 1 ELSE selection_eof END,
                     phase = CASE
                         WHEN selection_complete = 0 THEN 5
                         WHEN pending_effects = 0 THEN 3
                         ELSE 2
                     END
                 WHERE singleton = 1",
                [],
            )
            .map_err(DurableSourceInventoryError::Scratch)?;
    }
    Ok(())
}

fn publish_complete_if_ready(
    transaction: &Transaction<'_>,
    owner: &DurableSourceInventoryOwner,
) -> DurableInventoryResult<()> {
    assert_durable_owner_transaction(transaction, owner)?;
    let (mode, pending_effects): (i64, i64) = transaction
        .query_row(
            "SELECT mode, pending_effects FROM inventory_meta WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    let has_noncomplete_directory = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM directory_queue WHERE state IN (0, 1) LIMIT 1
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    let has_pending_effect = if decode_mode(mode)? == DurableSourceInventoryMode::RegularFiles {
        transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM selected_paths WHERE effect_state = 0 LIMIT 1
                 )",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(DurableSourceInventoryError::Scratch)?
    } else {
        transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM path_journal WHERE state = 0 LIMIT 1
                 )",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(DurableSourceInventoryError::Scratch)?
    };
    let selection = read_selection_state(transaction)?;
    let has_pending_membership = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM effect_membership WHERE effect_state = 0 LIMIT 1
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    if !selection.frozen
        || (pending_effects == 0) == has_pending_effect
        || has_pending_effect != has_pending_membership
        || nonnegative_u64(pending_effects)?
            != selection
                .planned_count
                .saturating_sub(selection.application_ordinal)
    {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
    if has_noncomplete_directory || has_pending_effect {
        return Ok(());
    }
    transaction
        .execute(
            "UPDATE inventory_meta
             SET complete = 1, phase = 3
             WHERE singleton = 1
               AND traversal_complete = 1
               AND selection_complete = 1
               AND active_directory_identity IS NULL
               AND queued_directories = 0
               AND pending_effects = 0
               AND EXISTS (
                   SELECT 1 FROM selection_state
                   WHERE singleton = 1 AND frozen = 1
                     AND application_ordinal = planned_count
               )",
            [],
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    Ok(())
}

fn effect_identity_range_matches(
    transaction: &Transaction<'_>,
    start_ordinal: u64,
    identities: &[[u8; 32]],
    expected_state: i64,
) -> DurableInventoryResult<bool> {
    for (offset, identity) in identities.iter().enumerate() {
        let ordinal = start_ordinal
            .checked_add(u64::try_from(offset).unwrap_or(u64::MAX))
            .ok_or(DurableSourceInventoryError::CorruptScratch)?;
        let matches = transaction
            .query_row(
                "SELECT 1 FROM effect_membership
                 WHERE ordinal = ?1 AND journal_identity = ?2 AND effect_state = ?3",
                params![u64_i64(ordinal)?, identity.as_slice(), expected_state],
                |_| Ok(()),
            )
            .optional()
            .map_err(DurableSourceInventoryError::Scratch)?
            .is_some();
        if !matches {
            return Ok(false);
        }
    }
    Ok(true)
}

fn verify_scratch_completion(
    connection: &Connection,
    meta: &DurableInventoryMeta,
) -> DurableInventoryResult<()> {
    let selection = read_selection_state(connection)?;
    validate_selection_meta_consistency(meta, &selection)?;
    let has_noncomplete_directory = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM directory_queue WHERE state IN (0, 1) LIMIT 1
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    let has_pending_effect = match meta.checkpoint.mode {
        DurableSourceInventoryMode::RegularFiles => connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM selected_paths WHERE effect_state = 0 LIMIT 1
                 )",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(DurableSourceInventoryError::Scratch)?,
        DurableSourceInventoryMode::Jsonl => connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM path_journal WHERE state = 0 LIMIT 1
                 )",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(DurableSourceInventoryError::Scratch)?,
    };
    let has_pending_membership = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM effect_membership WHERE effect_state = 0 LIMIT 1
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(DurableSourceInventoryError::Scratch)?;
    if has_noncomplete_directory
        || has_pending_effect
        || has_pending_membership
        || meta.active_directory_identity.is_some()
        || meta.queued_directories != 0
        || meta.pending_effects != 0
        || !meta.selection_eof
        || !meta.selection_complete
        || !meta.traversal_complete
        || !meta.complete
        || meta.phase != DurableSourceInventoryPhase::Complete
        || selection.commitment()?.is_none()
        || selection.application_ordinal != selection.planned_count
    {
        return Err(DurableSourceInventoryError::CorruptScratch);
    }
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
    let retry_at = if matches!(
        failure,
        DurableSourceInventoryFailureKind::OpenDirectory
            | DurableSourceInventoryFailureKind::ReadDirectory
            | DurableSourceInventoryFailureKind::ScratchWrite
    ) {
        let attempt = transaction
            .query_row(
                "SELECT attempt_count FROM directory_queue
                 WHERE path_identity = (
                    SELECT active_directory_identity FROM inventory_meta WHERE singleton = 1
                 ) AND state = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|_| DurableSourceInventoryError::CorruptScratch)?;
        Some(now_ms().saturating_add(retry_delay_ms(nonnegative_u64(attempt)?)))
    } else {
        None
    };
    transaction
        .execute(
            "UPDATE inventory_meta
             SET last_error = ?1, next_retry_at_ms = ?2
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
    seal_durable_state(&transaction)?;
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

fn validate_cleanup_proof(
    request: &DurableSourceInventoryRequest,
    proof: &ImportInventoryCheckpointCleanupProof,
    expected_cleanup_keyset: Option<&[u8]>,
    root_identity: &NativePathIdentity,
) -> DurableInventoryResult<()> {
    if proof.checkpoint_format_version == 0
        || proof.producer_build_id != request.build_identity
        || proof.store_schema_version == 0
        || proof.run_id != request.run_id
        || proof.source_identity != request.source_id
        || proof.source_fingerprint.is_empty()
        || proof.source_fingerprint.len() > DURABLE_INVENTORY_MAX_ID_BYTES
        || proof.root_path.platform_tag.is_empty()
        || proof.root_path.platform_tag.len() > 64
        || proof.root_path.encoding_tag.is_empty()
        || proof.root_path.encoding_tag.len() > 64
        || proof.root_path.opaque_hash.len() != 32
        || proof.root_path.platform_tag != root_identity.platform.as_str()
        || proof.root_path.encoding_tag != root_identity.encoding.as_str()
        || proof.root_path.opaque_hash != root_identity.sha256
        || proof.inventory_generation != request.generation
        || proof.source_format.is_empty()
        || proof.source_format.len() > DURABLE_INVENTORY_MAX_ID_BYTES
        || proof.source_root.is_empty()
        || proof.source_root.len() > DURABLE_INVENTORY_MAX_NATIVE_PATH_BYTES
        || proof.scratch_identity.is_empty()
        || proof.scratch_identity.len() > DURABLE_INVENTORY_MAX_ID_BYTES
        || proof.scratch_integrity.len() != 32
        || proof.scratch_lock_identity.is_empty()
        || proof.scratch_lock_identity.len() > DURABLE_INVENTORY_MAX_ID_BYTES
        || proof.scratch_database_identity.is_empty()
        || proof.scratch_database_identity.len() > DURABLE_INVENTORY_MAX_ID_BYTES
        || expected_cleanup_keyset.is_some_and(|keyset| keyset.len() != 32)
    {
        return Err(DurableSourceInventoryError::CleanupProofMismatch);
    }
    Ok(())
}

fn validate_cleanup_scratch(
    scratch: &DurableCaptureScratch,
    request: &DurableSourceInventoryRequest,
    proof: &ImportInventoryCheckpointCleanupProof,
    requested_root: &NativePathDescriptor,
) -> DurableInventoryResult<()> {
    let database_identity = scratch
        .file_identity(DURABLE_INVENTORY_DATABASE_NAME)
        .map_err(DurableSourceInventoryError::Filesystem)?;
    if database_identity != proof.scratch_database_identity {
        return Err(DurableSourceInventoryError::CleanupProofMismatch);
    }
    let database_path = scratch.path().join(DURABLE_INVENTORY_DATABASE_NAME);
    validate_durable_database_path(&database_path)?;
    let connection = Connection::open_with_flags(
        database_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| DurableSourceInventoryError::CorruptScratch)?;
    configure_durable_inventory_connection(&connection)?;
    validate_durable_inventory_schema(&connection)?;
    let meta = read_durable_meta(&connection)?;
    let selection = read_selection_state(&connection)?;
    validate_selection_meta_consistency(&meta, &selection)?;
    validate_resume_contract(request, &meta.checkpoint, requested_root, &meta)?;
    if meta.checkpoint.format_version != DURABLE_INVENTORY_FORMAT_VERSION
        || meta.checkpoint.build_identity != proof.producer_build_id
        || meta.checkpoint.run_id != proof.run_id
        || meta.checkpoint.source_id != proof.source_identity
        || meta.checkpoint.generation != proof.inventory_generation
        || meta.checkpoint.root_identity.platform.as_str() != proof.root_path.platform_tag
        || meta.checkpoint.root_identity.encoding.as_str() != proof.root_path.encoding_tag
        || meta.checkpoint.root_identity.sha256.as_slice() != proof.root_path.opaque_hash
        || meta.checkpoint.scratch_identity != proof.scratch_identity
        || meta.checkpoint.scratch_integrity.as_slice() != proof.scratch_integrity
        || meta.checkpoint.scratch_lock_identity != proof.scratch_lock_identity
        || meta.checkpoint.scratch_database_identity != proof.scratch_database_identity
    {
        return Err(DurableSourceInventoryError::CleanupProofMismatch);
    }
    Ok(())
}

fn cleanup_advance(
    proof: &ImportInventoryCheckpointCleanupProof,
    expected_cleanup_keyset: Option<&[u8]>,
    cleaned_rows_delta: u64,
    cleaned_bytes_delta: u64,
    complete: bool,
) -> DurableSourceInventoryCleanupAdvance {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-durable-inventory-cleanup-advance-v1\0");
    hash_inventory_field(&mut hasher, &proof.run_id);
    hash_inventory_field(
        &mut hasher,
        match proof.inventory_family {
            ProviderFileInventoryFamily::Catalog => b"catalog",
            ProviderFileInventoryFamily::SourceImport => b"source_import",
        },
    );
    hash_inventory_field(&mut hasher, proof.provider.as_str().as_bytes());
    hash_inventory_field(&mut hasher, proof.source_format.as_bytes());
    hash_inventory_field(&mut hasher, proof.source_root.as_bytes());
    hash_inventory_field(&mut hasher, &proof.source_identity);
    hasher.update(proof.inventory_generation.to_be_bytes());
    hash_inventory_field(&mut hasher, &proof.scratch_identity);
    hash_inventory_field(&mut hasher, &proof.scratch_integrity);
    hash_optional_inventory_field(&mut hasher, expected_cleanup_keyset);
    hasher.update([u8::from(complete)]);
    if !complete {
        hasher.update(cleaned_rows_delta.to_be_bytes());
        hasher.update(cleaned_bytes_delta.to_be_bytes());
    }
    DurableSourceInventoryCleanupAdvance {
        expected_cleanup_keyset: expected_cleanup_keyset.map(<[u8]>::to_vec),
        cleanup_keyset: Some(hasher.finalize().to_vec()),
        visited_rows_delta: cleaned_rows_delta,
        cleaned_rows_delta,
        cleaned_bytes_delta,
        complete,
    }
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
        5 => Ok(DurableSourceInventoryPhase::Selection),
        _ => Err(DurableSourceInventoryError::CorruptScratch),
    }
}

fn encode_phase(value: DurableSourceInventoryPhase) -> i64 {
    match value {
        DurableSourceInventoryPhase::Traversal => 1,
        DurableSourceInventoryPhase::Effects => 2,
        DurableSourceInventoryPhase::Complete => 3,
        DurableSourceInventoryPhase::Selection => 5,
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
    DirectoryOpenAfterSuccess,
    OpenAfterValidation,
    OwnerAdoptionAfterCommit,
    SelectionPageBeforeCommit,
    MembershipPageBeforeCommit,
    MembershipPageAfterCommit,
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
            let transaction = inventory.connection_mut().unwrap().transaction().unwrap();
            assert_durable_owner_transaction(&transaction, owner).unwrap();
            assert_eq!(
                transaction
                    .execute(
                        "UPDATE directory_queue SET next_retry_at_ms = 0
                         WHERE path_identity = ?1 AND state = 1",
                        params![identity.as_slice()],
                    )
                    .unwrap(),
                1
            );
            assert_eq!(
                transaction
                    .execute(
                        "UPDATE inventory_meta SET next_retry_at_ms = 0 WHERE singleton = 1",
                        [],
                    )
                    .unwrap(),
                1
            );
            seal_durable_state(&transaction).unwrap();
            transaction.commit().unwrap();
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
            let status = inventory.status().unwrap();
            if status.traversal_complete && !status.selection_complete {
                plan_rejected_inventory_page(inventory, owner);
                continue;
            }
            let page = inventory.next_effects_page(owner).unwrap();
            if !page.entries.is_empty() {
                for entry in &page.entries {
                    assert_eq!(entry.journal.directory.scratch, inventory.scratch_state());
                }
                let identities = page
                    .entries
                    .iter()
                    .map(|entry| entry.journal.journal_identity)
                    .collect::<Vec<_>>();
                observed.extend(page.entries.into_iter().map(|entry| entry.journal));
                inventory.acknowledge_effects(owner, &identities).unwrap();
            }
            if inventory.completion_proof(owner).unwrap().is_some() {
                return observed;
            }
        }
        panic!("durable inventory did not converge within the source-test bound");
    }

    fn plan_rejected_inventory_page(
        inventory: &mut DurableSourcePathInventory,
        owner: &DurableSourceInventoryOwner,
    ) -> DurableSourceInventoryMembershipAdvance {
        let page = inventory.next_membership_candidates_page(owner).unwrap();
        let source_paths = page
            .entries
            .iter()
            .map(|entry| entry.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let plans = page
            .entries
            .iter()
            .zip(&source_paths)
            .map(|(entry, source_path)| DurableSourceInventoryEffectPlan {
                journal_identity: entry.journal_identity,
                accounted_bytes: 1,
                effect: ImportInventoryCanonicalEffect::CatalogObservationRejected { source_path },
            })
            .collect::<Vec<_>>();
        let source_root = inventory.request.root.to_string_lossy();
        inventory
            .plan_effect_membership_page(
                owner,
                DurableSourceInventoryEffectScope {
                    inventory_family: ProviderFileInventoryFamily::Catalog,
                    provider: CaptureProvider::Codex,
                    source_format: "jsonl",
                    source_root: &source_root,
                },
                &plans,
            )
            .unwrap()
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
        assert!(matches!(
            inventory.next_effects_page(&owner),
            Err(DurableSourceInventoryError::CheckpointMismatch)
        ));

        let observed = drain_durable_inventory(&mut inventory, &owner);
        assert_eq!(observed.len(), 70);
        let status = inventory.status().unwrap();
        assert!(status.replay_count >= 1);
    }

    #[test]
    fn durable_inventory_persists_exponential_retry_before_directory_open() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("source");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&source_root).unwrap();
        fs::write(source_root.join("path.jsonl"), "{}\n").unwrap();
        let request = durable_request(&source_root, 9);
        let first_owner = durable_owner(1, 0x91);
        let mut inventory =
            create_owned_durable_inventory(&data_root, request.clone(), first_owner.clone());

        inject_durable_inventory_failure_once(
            DurableInventoryFailurePoint::DirectoryOpenAfterSuccess,
        );
        assert!(inventory.advance(&first_owner).is_err());
        let first = inventory.status().unwrap().active_directory.unwrap();
        assert_eq!(first.attempt_count, 1);
        let first_retry = first.next_retry_at_ms.unwrap();
        let checkpoint = inventory.checkpoint().clone();
        drop(inventory);

        let mut inventory =
            DurableSourcePathInventory::open(&data_root, request, &checkpoint).unwrap();
        let recovered_owner = durable_owner(2, 0x92);
        inventory
            .adopt_owner(Some(&first_owner), recovered_owner.clone())
            .unwrap();
        clear_durable_retry_for_test(&mut inventory, &recovered_owner);
        inject_durable_inventory_failure_once(
            DurableInventoryFailurePoint::DirectoryOpenAfterSuccess,
        );
        assert!(inventory.advance(&recovered_owner).is_err());
        let second = inventory.status().unwrap().active_directory.unwrap();
        assert_eq!(second.attempt_count, 2);
        assert_eq!(second.replay_count, 1);
        assert!(second.next_retry_at_ms.unwrap() > first_retry);

        clear_durable_retry_for_test(&mut inventory, &recovered_owner);
        let observed = drain_durable_inventory(&mut inventory, &recovered_owner);
        assert_eq!(observed.len(), 1);
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

        let decisions = discovered
            .entries
            .iter()
            .map(|entry| {
                let (group_key, rank) = if entry.path == paths[0] || entry.path == paths[1] {
                    (b"group-a".to_vec(), 1)
                } else if entry.path == paths[2] {
                    (b"group-b".to_vec(), 2)
                } else {
                    (b"group-b".to_vec(), 3)
                };
                DurableSourceInventorySelectionDecision {
                    journal_identity: entry.journal_identity,
                    path_identity: entry.path_identity.clone(),
                    candidate: Some(DurableSourceInventorySelectionCandidate { group_key, rank }),
                }
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            inventory.apply_path_selection_page(&first_owner, None, &decisions[..3]),
            Err(DurableSourceInventoryError::CheckpointMismatch)
        ));
        assert_eq!(inventory.status().unwrap().selection_cursor, None);
        inject_durable_inventory_failure_once(
            DurableInventoryFailurePoint::SelectionPageBeforeCommit,
        );
        assert!(inventory
            .apply_path_selection_page(&first_owner, None, &decisions)
            .is_err());
        let status = inventory.status().unwrap();
        assert_eq!(status.selected_files, 0);
        assert_eq!(status.selection_cursor, None);
        assert!(!status.selection_eof);
        let checkpoint = inventory.checkpoint().clone();
        drop(inventory);

        let mut inventory =
            DurableSourcePathInventory::open(&data_root, request, &checkpoint).unwrap();
        let recovered_owner = durable_owner(2, 0x72);
        inventory
            .adopt_owner(Some(&first_owner), recovered_owner.clone())
            .unwrap();
        assert!(!inventory.status().unwrap().selection_complete);
        let advance = inventory
            .apply_path_selection_page(&recovered_owner, None, &decisions)
            .unwrap();
        assert!(advance.eof);
        assert_eq!(advance.processed_entries, 4);
        assert_eq!(inventory.status().unwrap().selected_files, 2);
        inventory.complete_path_selection(&recovered_owner).unwrap();
        let membership = plan_rejected_inventory_page(&mut inventory, &recovered_owner);
        assert!(membership.complete);
        assert_eq!(membership.commitment.unwrap().total_count, 2);
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
                    .map(|entry| entry.journal.journal_identity)
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
    fn durable_membership_freeze_rejects_omission_reorder_and_substitution_across_restart() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("source");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&source_root).unwrap();
        for index in 0..3 {
            fs::write(source_root.join(format!("path-{index}.jsonl")), "{}\n").unwrap();
        }
        let request = durable_request(&source_root, 16);
        let first_owner = durable_owner(1, 0xd1);
        let mut inventory =
            create_owned_durable_inventory(&data_root, request.clone(), first_owner.clone());
        while !inventory.advance(&first_owner).unwrap().traversal_complete {}
        let candidates = inventory
            .next_membership_candidates_page(&first_owner)
            .unwrap();
        assert!(candidates.complete);
        assert_eq!(candidates.entries.len(), 3);
        assert!(matches!(
            inventory.next_effects_page(&first_owner),
            Err(DurableSourceInventoryError::CheckpointMismatch)
        ));
        let source_paths = candidates
            .entries
            .iter()
            .map(|entry| entry.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let plans = candidates
            .entries
            .iter()
            .zip(&source_paths)
            .enumerate()
            .map(
                |(index, (entry, source_path))| DurableSourceInventoryEffectPlan {
                    journal_identity: entry.journal_identity,
                    accounted_bytes: 7,
                    effect: match index {
                        0 => ImportInventoryCanonicalEffect::CatalogStale {
                            source_path,
                            observed_at_ms: 123,
                        },
                        1 => ImportInventoryCanonicalEffect::CatalogRescan { source_path },
                        _ => ImportInventoryCanonicalEffect::CatalogObservationRejected {
                            source_path,
                        },
                    },
                },
            )
            .collect::<Vec<_>>();
        let root = source_root.to_string_lossy();
        let scope = DurableSourceInventoryEffectScope {
            inventory_family: ProviderFileInventoryFamily::Catalog,
            provider: CaptureProvider::Codex,
            source_format: "jsonl",
            source_root: &root,
        };
        assert!(matches!(
            inventory.plan_effect_membership_page(&first_owner, scope, &plans[..2]),
            Err(DurableSourceInventoryError::CheckpointMismatch)
        ));
        let mut reordered = plans.clone();
        reordered.swap(0, 1);
        assert!(matches!(
            inventory.plan_effect_membership_page(&first_owner, scope, &reordered),
            Err(DurableSourceInventoryError::CheckpointMismatch)
        ));
        let mut substituted = plans.clone();
        substituted[0].journal_identity = [0xee; 32];
        assert!(matches!(
            inventory.plan_effect_membership_page(&first_owner, scope, &substituted),
            Err(DurableSourceInventoryError::CheckpointMismatch)
        ));

        inject_durable_inventory_failure_once(
            DurableInventoryFailurePoint::MembershipPageBeforeCommit,
        );
        assert!(inventory
            .plan_effect_membership_page(&first_owner, scope, &plans)
            .is_err());
        let before_commit = inventory.status().unwrap();
        assert!(!before_commit.selection_complete);
        assert_eq!(before_commit.pending_effects, 0);

        inject_durable_inventory_failure_once(
            DurableInventoryFailurePoint::MembershipPageAfterCommit,
        );
        assert!(inventory
            .plan_effect_membership_page(&first_owner, scope, &plans)
            .is_err());
        let status = inventory.status().unwrap();
        assert!(status.selection_complete);
        let commitment = status.selection_commitment.unwrap();
        assert_eq!(commitment.total_count, 3);
        assert_eq!(commitment.final_keyset, Some(plans[2].journal_identity));
        let checkpoint = inventory.checkpoint().clone();
        drop(inventory);

        let mut inventory =
            DurableSourcePathInventory::open(&data_root, request, &checkpoint).unwrap();
        let recovered_owner = durable_owner(2, 0xd2);
        inventory
            .adopt_owner(Some(&first_owner), recovered_owner.clone())
            .unwrap();
        let effects = inventory.next_effects_page(&recovered_owner).unwrap();
        assert_eq!(effects.entries.len(), 3);
        for (ordinal, entry) in effects.entries.iter().enumerate() {
            assert_eq!(entry.membership.ordinal, ordinal as u64);
            assert_eq!(
                entry.membership.prior_keyset,
                ordinal
                    .checked_sub(1)
                    .map(|prior| plans[prior].journal_identity)
            );
            assert_eq!(
                entry.membership.resulting_keyset,
                entry.journal.journal_identity
            );
        }

        inventory
            .connection_mut()
            .unwrap()
            .execute(
                "UPDATE effect_membership SET resulting_prefix = zeroblob(32)
                 WHERE ordinal = 2",
                [],
            )
            .unwrap();
        assert!(matches!(
            inventory.next_effects_page(&recovered_owner),
            Err(DurableSourceInventoryError::CorruptScratch)
        ));
    }

    #[test]
    fn durable_empty_membership_has_no_keyset_and_the_canonical_initial_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("source");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&source_root).unwrap();
        let owner = durable_owner(1, 0xd3);
        let mut inventory = create_owned_durable_inventory(
            &data_root,
            durable_request(&source_root, 17),
            owner.clone(),
        );
        while !inventory.advance(&owner).unwrap().traversal_complete {}
        let advance = plan_rejected_inventory_page(&mut inventory, &owner);
        let commitment = advance.commitment.unwrap();
        assert!(advance.complete);
        assert_eq!(commitment.total_count, 0);
        assert_eq!(commitment.final_keyset, None);
        assert_eq!(
            commitment.final_prefix,
            import_inventory_selection_initial_prefix(
                IMPORT_INVENTORY_SELECTION_FORMAT_VERSION,
                IMPORT_INVENTORY_SELECTION_ALGORITHM_VERSION,
            )
            .unwrap()
        );
        assert!(inventory.completion_proof(&owner).unwrap().is_some());
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
        let journals = inventory.next_membership_candidates_page(&owner).unwrap();
        assert_eq!(journals.entries.len(), 2);
        fs::remove_file(&removed).unwrap();

        let page = crate::provider::codex::catalog::observe_codex_catalog_journal_page(
            crate::provider::codex::catalog::CodexCatalogObservationRequest {
                entries: &journals.entries,
                after_journal_identity: None,
                source_root: source_root.to_str().unwrap(),
                cataloged_at_ms: 123,
            },
        )
        .unwrap();
        assert!(page.complete);
        assert_eq!(
            page.stop_reason,
            crate::provider::codex::catalog::CodexCatalogObservationStopReason::Complete
        );
        assert_eq!(page.usage.rows, 2);
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
            &observation.outcome,
            crate::provider::codex::catalog::CodexCatalogObservationOutcome::Failed(_)
        )));
        for observation in &page.observations {
            assert_eq!(
                observation.membership_accounted_bytes(),
                observation.serialized_bytes
            );
            assert!(matches!(
                (
                    &observation.outcome,
                    observation.canonical_effect().unwrap()
                ),
                (
                    crate::provider::codex::catalog::CodexCatalogObservationOutcome::Cataloged(_),
                    ImportInventoryCanonicalEffect::CatalogUpsert(_)
                ) | (
                    crate::provider::codex::catalog::CodexCatalogObservationOutcome::Failed(_),
                    ImportInventoryCanonicalEffect::CatalogObservationRejected { .. }
                )
            ));
        }
        let retried = crate::provider::codex::catalog::observe_codex_catalog_journal_page(
            crate::provider::codex::catalog::CodexCatalogObservationRequest {
                entries: &journals.entries,
                after_journal_identity: None,
                source_root: source_root.to_str().unwrap(),
                cataloged_at_ms: 456,
            },
        )
        .unwrap();
        assert_eq!(
            page.observations
                .iter()
                .map(|observation| (
                    observation.journal.journal_identity,
                    observation.source_files,
                    observation.source_bytes,
                    observation.retained_bytes,
                    observation.serialized_bytes,
                    matches!(
                        &observation.outcome,
                        crate::provider::codex::catalog::CodexCatalogObservationOutcome::Cataloged(
                            _
                        )
                    )
                ))
                .collect::<BTreeSet<_>>(),
            retried
                .observations
                .iter()
                .map(|observation| (
                    observation.journal.journal_identity,
                    observation.source_files,
                    observation.source_bytes,
                    observation.retained_bytes,
                    observation.serialized_bytes,
                    matches!(
                        &observation.outcome,
                        crate::provider::codex::catalog::CodexCatalogObservationOutcome::Cataloged(
                            _
                        )
                    )
                ))
                .collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn durable_codex_observation_bounds_near_limit_metadata_without_retaining_source() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("source");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&source_root).unwrap();
        let padding = "x".repeat(60 * 1024);
        for index in 0..64 {
            let record = serde_json::json!({
                "type": "session_meta",
                "payload": {
                    "id": format!("session-{index}"),
                    "source": {"opaque": padding.as_str()},
                }
            });
            fs::write(
                source_root.join(format!("path-{index:04}.jsonl")),
                format!("{record}\n"),
            )
            .unwrap();
        }
        let owner = durable_owner(1, 0x82);
        let mut inventory = create_owned_durable_inventory(
            &data_root,
            durable_request(&source_root, 13),
            owner.clone(),
        );
        while !inventory.advance(&owner).unwrap().traversal_complete {}
        let journals = inventory.next_membership_candidates_page(&owner).unwrap();
        assert_eq!(journals.entries.len(), 64);

        let page = crate::provider::codex::catalog::observe_codex_catalog_journal_page(
            crate::provider::codex::catalog::CodexCatalogObservationRequest {
                entries: &journals.entries,
                after_journal_identity: None,
                source_root: source_root.to_str().unwrap(),
                cataloged_at_ms: 789,
            },
        )
        .unwrap();
        assert!(page.complete);
        assert_eq!(page.usage.rows, 64);
        assert!(page.usage.source_read_bytes <= 16 * 1024 * 1024);
        assert!(page.usage.retained_bytes <= IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES as u64);
        assert!(page.usage.serialized_bytes <= IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES as u64);
        assert_eq!(
            page.observations
                .iter()
                .map(|observation| observation.source_read_bytes)
                .sum::<u64>(),
            page.usage.source_read_bytes
        );
        assert_eq!(
            page.observations
                .iter()
                .map(|observation| observation.retained_bytes)
                .sum::<u64>(),
            page.usage.retained_bytes
        );
        assert_eq!(
            page.observations
                .iter()
                .map(|observation| observation.serialized_bytes)
                .sum::<u64>(),
            page.usage.serialized_bytes
        );
        for observation in page.observations {
            let crate::provider::codex::catalog::CodexCatalogObservationOutcome::Cataloged(session) =
                observation.outcome
            else {
                panic!("near-limit Codex metadata should remain catalogable");
            };
            assert!(session.metadata.get("source").is_none());
        }
    }

    #[test]
    fn durable_codex_observation_stops_at_the_oversized_prefix_read_bound() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("source");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&source_root).unwrap();
        fs::write(
            source_root.join("oversized.jsonl"),
            vec![b'x'; 3 * 1024 * 1024],
        )
        .unwrap();
        let owner = durable_owner(1, 0x83);
        let mut inventory = create_owned_durable_inventory(
            &data_root,
            durable_request(&source_root, 14),
            owner.clone(),
        );
        while !inventory.advance(&owner).unwrap().traversal_complete {}
        let journals = inventory.next_membership_candidates_page(&owner).unwrap();
        let page = crate::provider::codex::catalog::observe_codex_catalog_journal_page(
            crate::provider::codex::catalog::CodexCatalogObservationRequest {
                entries: &journals.entries,
                after_journal_identity: None,
                source_root: source_root.to_str().unwrap(),
                cataloged_at_ms: 790,
            },
        )
        .unwrap();
        assert!(page.complete);
        assert_eq!(page.usage.rows, 1);
        assert_eq!(page.usage.source_read_bytes, 2 * 1024 * 1024);
        assert!(matches!(
            &page.observations[0].outcome,
            crate::provider::codex::catalog::CodexCatalogObservationOutcome::Failed(_)
        ));
    }

    #[test]
    fn durable_codex_observation_resumes_from_a_payload_budget_keyset() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("source");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&source_root).unwrap();
        let cwd = "x".repeat(150 * 1024);
        for index in 0..64 {
            let record = serde_json::json!({
                "type": "session_meta",
                "payload": {"id": format!("session-{index}"), "cwd": cwd.as_str()}
            });
            fs::write(
                source_root.join(format!("path-{index:04}.jsonl")),
                format!("{record}\n"),
            )
            .unwrap();
        }
        let owner = durable_owner(1, 0x84);
        let mut inventory = create_owned_durable_inventory(
            &data_root,
            durable_request(&source_root, 15),
            owner.clone(),
        );
        while !inventory.advance(&owner).unwrap().traversal_complete {}
        let journals = inventory.next_membership_candidates_page(&owner).unwrap();
        let mut after = None;
        let mut observed = BTreeSet::new();
        let mut pages = 0_u64;
        loop {
            let page = crate::provider::codex::catalog::observe_codex_catalog_journal_page(
                crate::provider::codex::catalog::CodexCatalogObservationRequest {
                    entries: &journals.entries,
                    after_journal_identity: after,
                    source_root: source_root.to_str().unwrap(),
                    cataloged_at_ms: 791,
                },
            )
            .unwrap();
            assert!(page.usage.source_read_bytes <= 16 * 1024 * 1024);
            assert!(page.usage.retained_bytes <= IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES as u64);
            assert!(
                page.usage.serialized_bytes <= IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES as u64
            );
            for observation in page.observations {
                assert!(observed.insert(observation.journal.journal_identity));
            }
            pages = pages.saturating_add(1);
            after = page.next_keyset;
            if page.complete {
                break;
            }
            assert!(after.is_some());
        }
        assert!(pages >= 2);
        assert_eq!(observed.len(), 64);
    }

    #[test]
    fn durable_inventory_cleanup_requires_store_authority_and_reports_cas_progress() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("source");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&source_root).unwrap();
        fs::write(source_root.join("path.jsonl"), "{}\n").unwrap();
        let request = durable_request(&source_root, 10);
        let previous_owner = durable_owner(1, 0xa1);
        let inventory =
            create_owned_durable_inventory(&data_root, request.clone(), previous_owner.clone());
        let checkpoint = inventory.checkpoint().clone();
        let scratch = inventory.scratch_state();
        let proof = ImportInventoryCheckpointCleanupProof {
            checkpoint_format_version: 1,
            producer_build_id: checkpoint.build_identity.clone(),
            store_schema_version: 57,
            run_id: checkpoint.run_id.clone(),
            inventory_family: ProviderFileInventoryFamily::Catalog,
            provider: CaptureProvider::Codex,
            source_format: "jsonl".to_owned(),
            source_root: source_root.to_string_lossy().into_owned(),
            source_identity: checkpoint.source_id.clone(),
            source_fingerprint: vec![0xa2; 32],
            root_path: ImportInventoryOwnedPathIdentity {
                platform_tag: checkpoint.root_identity.platform.as_str().to_owned(),
                encoding_tag: checkpoint.root_identity.encoding.as_str().to_owned(),
                opaque_hash: checkpoint.root_identity.sha256.to_vec(),
            },
            inventory_generation: checkpoint.generation,
            scratch_identity: scratch.identity.clone(),
            scratch_integrity: scratch.integrity.to_vec(),
            scratch_lock_identity: scratch.lock_identity.clone(),
            scratch_database_identity: scratch.database_identity.clone(),
        };
        assert_eq!(
            DurableSourcePathInventory::cleanup_checkpoint(&data_root, &request, &proof, None)
                .unwrap(),
            DurableSourceInventoryCleanupOutcome::Busy
        );
        drop(inventory);

        let mut mismatched = proof.clone();
        mismatched.scratch_database_identity.push(0xff);
        assert!(matches!(
            DurableSourcePathInventory::cleanup_checkpoint(&data_root, &request, &mismatched, None,),
            Err(DurableSourceInventoryError::CleanupProofMismatch)
        ));
        let mut stale = proof.clone();
        stale.run_id.push(0xa4);
        assert!(matches!(
            DurableSourcePathInventory::cleanup_checkpoint(&data_root, &request, &stale, None),
            Err(DurableSourceInventoryError::CleanupProofMismatch)
        ));

        let mut expected_cleanup_keyset = None;
        for _ in 0..32 {
            match DurableSourcePathInventory::cleanup_checkpoint(
                &data_root,
                &request,
                &proof,
                expected_cleanup_keyset.as_deref(),
            )
            .unwrap()
            {
                DurableSourceInventoryCleanupOutcome::Busy => continue,
                DurableSourceInventoryCleanupOutcome::Advance(advance) => {
                    assert_eq!(advance.expected_cleanup_keyset, expected_cleanup_keyset);
                    assert_eq!(advance.cleanup_keyset.as_ref().unwrap().len(), 32);
                    assert_eq!(advance.visited_rows_delta, advance.cleaned_rows_delta);
                    assert!(advance.cleaned_rows_delta <= 1024);
                    assert!(
                        advance.cleaned_bytes_delta
                            <= IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES as u64
                    );
                    let store_advance = advance.store_advance();
                    assert_eq!(
                        store_advance.expected_cleanup_keyset,
                        expected_cleanup_keyset.as_deref()
                    );
                    assert_eq!(
                        store_advance.cleanup_keyset,
                        advance.cleanup_keyset.as_deref()
                    );
                    assert_eq!(
                        store_advance.disposition,
                        if advance.complete {
                            ImportInventoryCleanupDisposition::Complete
                        } else {
                            ImportInventoryCleanupDisposition::Pending
                        }
                    );
                    expected_cleanup_keyset = advance.cleanup_keyset;
                    if advance.complete {
                        let already_removed = DurableSourcePathInventory::cleanup_checkpoint(
                            &data_root,
                            &request,
                            &proof,
                            expected_cleanup_keyset.as_deref(),
                        )
                        .unwrap();
                        assert!(matches!(
                            already_removed,
                            DurableSourceInventoryCleanupOutcome::Advance(
                                DurableSourceInventoryCleanupAdvance { complete: true, .. }
                            )
                        ));
                        return;
                    }
                }
            }
        }
        panic!("authorized durable inventory cleanup did not converge");
    }

    #[test]
    fn durable_inventory_rejects_oversized_rows_and_pending_effect_tamper() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("source");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&source_root).unwrap();
        fs::write(source_root.join("path.jsonl"), "{}\n").unwrap();
        let request = durable_request(&source_root, 11);
        let owner = durable_owner(1, 0xb1);
        let mut inventory =
            create_owned_durable_inventory(&data_root, request.clone(), owner.clone());
        while !inventory.advance(&owner).unwrap().traversal_complete {}
        let checkpoint = inventory.checkpoint().clone();
        let database = inventory
            .scratch
            .as_ref()
            .unwrap()
            .path()
            .join(DURABLE_INVENTORY_DATABASE_NAME);
        drop(inventory);

        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch("PRAGMA ignore_check_constraints = ON;")
            .unwrap();
        connection
            .execute("UPDATE path_journal SET path = zeroblob(300000)", [])
            .unwrap();
        drop(connection);
        let inventory =
            DurableSourcePathInventory::open(&data_root, request.clone(), &checkpoint).unwrap();
        assert!(matches!(
            inventory.paths_page(&owner, None, 64),
            Err(DurableSourceInventoryError::OversizedScratchValue(_))
                | Err(DurableSourceInventoryError::DecodeBudgetExceeded)
        ));
        drop(inventory);

        fs::remove_dir_all(temp.path()).unwrap();

        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("source");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&source_root).unwrap();
        fs::write(source_root.join("path.jsonl"), "{}\n").unwrap();
        let request = durable_request(&source_root, 12);
        let owner = durable_owner(1, 0xb2);
        let mut inventory =
            create_owned_durable_inventory(&data_root, request.clone(), owner.clone());
        let _ = drain_durable_inventory(&mut inventory, &owner);
        let checkpoint = inventory.checkpoint().clone();
        let database = inventory
            .scratch
            .as_ref()
            .unwrap()
            .path()
            .join(DURABLE_INVENTORY_DATABASE_NAME);
        drop(inventory);
        let connection = Connection::open(&database).unwrap();
        connection
            .execute("UPDATE path_journal SET state = 0", [])
            .unwrap();
        drop(connection);
        let inventory = DurableSourcePathInventory::open(&data_root, request, &checkpoint).unwrap();
        assert!(matches!(
            inventory.completion_proof(&owner),
            Err(DurableSourceInventoryError::CorruptScratch)
        ));
    }

    #[test]
    fn durable_inventory_rejects_counter_and_phase_tamper() {
        for (tamper_sql, run) in [
            (
                "UPDATE inventory_meta SET discovered_files = discovered_files + 1",
                13,
            ),
            ("UPDATE inventory_meta SET phase = 1", 14),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let data_root = temp.path().join("data");
            let source_root = temp.path().join("source");
            fs::create_dir(&data_root).unwrap();
            fs::create_dir(&source_root).unwrap();
            fs::write(source_root.join("path.jsonl"), "{}\n").unwrap();
            let request = durable_request(&source_root, run);
            let owner = durable_owner(1, 0xc0 + run);
            let mut inventory = create_owned_durable_inventory(&data_root, request.clone(), owner);
            while !inventory
                .advance(&durable_owner(1, 0xc0 + run))
                .unwrap()
                .traversal_complete
            {}
            let checkpoint = inventory.checkpoint().clone();
            let database = inventory
                .scratch
                .as_ref()
                .unwrap()
                .path()
                .join(DURABLE_INVENTORY_DATABASE_NAME);
            drop(inventory);

            Connection::open(&database)
                .unwrap()
                .execute(tamper_sql, [])
                .unwrap();
            assert!(matches!(
                DurableSourcePathInventory::open(&data_root, request, &checkpoint),
                Err(DurableSourceInventoryError::CorruptScratch)
            ));
        }
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
