use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use crate::common::io::{
    FilesystemTraversalBudget, FilesystemTraversalCursor, FilesystemTraversalErrorRecovery,
};
use crate::common::scratch::CaptureScratchSpace;
use crate::Result;

const PATH_INSERT_BATCH: usize = 64;
const SCRATCH_PATH_READ_BYTES_PER_ROW: u64 = 8 * 1024;
const SCRATCH_PATH_READ_BASE_BYTES: u64 = 4 * 1024;

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
    use std::fs;

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
}
