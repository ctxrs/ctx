use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use rusqlite::{ffi, Connection};

#[cfg(windows)]
use crate::object_store::windows_file_identity;
use crate::object_store::{restrict_private_dir, restrict_private_file};
use crate::{Result, Store, StoreError};

#[cfg(test)]
type TestDiskSpaceProbe = Box<dyn FnMut(&Path, &'static str) -> io::Result<u64>>;

#[cfg(test)]
thread_local! {
    static TEST_DISK_SPACE_PROBE: std::cell::RefCell<Option<TestDiskSpaceProbe>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct TestDiskSpaceProbeGuard {
    previous: Option<TestDiskSpaceProbe>,
}

#[cfg(test)]
impl Drop for TestDiskSpaceProbeGuard {
    fn drop(&mut self) {
        TEST_DISK_SPACE_PROBE.with(|slot| {
            *slot.borrow_mut() = self.previous.take();
        });
    }
}

#[cfg(test)]
pub(crate) fn install_test_disk_space_probe(
    probe: impl FnMut(&Path, &'static str) -> io::Result<u64> + 'static,
) -> TestDiskSpaceProbeGuard {
    let previous = TEST_DISK_SPACE_PROBE.with(|slot| slot.replace(Some(Box::new(probe))));
    TestDiskSpaceProbeGuard { previous }
}

pub const INDEXING_WAL_DELTA_BYTES: u64 = 8 * 1024 * 1024;
pub const WAL_PASSIVE_MIN_BYTES: u64 = 8 * 1024 * 1024;
pub const WAL_RESTART_MIN_BYTES: u64 = 32 * 1024 * 1024;
pub const WAL_TRUNCATE_MIN_BYTES: u64 = 64 * 1024 * 1024;
pub const INDEXING_DISK_RESERVE_BYTES: u64 = 1024 * 1024 * 1024;

// A background writer checks for foreground demand after every unit and never
// intentionally keeps one transaction open beyond this admission handoff SLO.
pub const INDEXING_TRANSACTION_MAX: Duration = Duration::from_millis(250);

// One full 8 MiB slice followed by the 3x quiet rest occupies one second. Use
// that existing invariant as the portable bandwidth ceiling instead of adding
// another tuning value.
const QUIET_INDEXING_BANDWIDTH_BYTES_PER_SEC: u64 = INDEXING_WAL_DELTA_BYTES;

const WRITER_LOCK_SUFFIX: &str = ".indexing-writer.lock";
const FOREGROUND_LOCK_SUFFIX: &str = ".indexing-foreground.lock";
const BACKGROUND_RESERVATION_LOCK_SUFFIX: &str = ".indexing-background-reservation.lock";
const RATE_RESERVATION_LOCK_SUFFIX: &str = ".indexing-rate-reservation.lock";
const RATE_RESERVATION_VERSION: &str = "ctx-indexing-rate-v1";
// Keep retry wakeups well inside the 250 ms transaction handoff boundary.
const BACKGROUND_ADMISSION_MAX_BACKOFF: Duration = Duration::from_millis(25);
const BACKGROUND_PROGRESS_AFTER: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_secs(2)
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexingWorkClass {
    Foreground,
    Background,
}

impl IndexingWorkClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexingPressure {
    Normal,
    Constrained,
    Unknown,
}

impl IndexingPressure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Constrained => "constrained",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IndexingResourceSnapshot {
    pub available_memory_bytes: Option<u64>,
    pub available_disk_bytes: Option<u64>,
    pub wal_bytes: Option<u64>,
}

impl IndexingResourceSnapshot {
    pub fn current(store_path: &Path, wal_bytes: Option<u64>) -> Self {
        let (_, available_memory_bytes) = system_memory();
        let available_disk_bytes = store_path
            .parent()
            .and_then(|parent| fs2::available_space(parent).ok());
        Self {
            available_memory_bytes,
            available_disk_bytes,
            wal_bytes,
        }
    }

    pub fn pressure(self) -> IndexingPressure {
        let memory = match self.available_memory_bytes {
            Some(available) if available < WAL_TRUNCATE_MIN_BYTES.saturating_mul(2) => {
                IndexingPressure::Constrained
            }
            Some(_) => IndexingPressure::Normal,
            None => IndexingPressure::Unknown,
        };
        let disk = match self.available_disk_bytes {
            Some(available) if available < WAL_TRUNCATE_MIN_BYTES.saturating_mul(2) => {
                IndexingPressure::Constrained
            }
            Some(_) => IndexingPressure::Normal,
            None => IndexingPressure::Unknown,
        };
        let wal = match self.wal_bytes {
            Some(bytes) if bytes >= WAL_TRUNCATE_MIN_BYTES => IndexingPressure::Constrained,
            Some(_) => IndexingPressure::Normal,
            None => IndexingPressure::Unknown,
        };
        if [memory, disk, wal].contains(&IndexingPressure::Constrained) {
            IndexingPressure::Constrained
        } else if [memory, disk, wal].contains(&IndexingPressure::Unknown) {
            IndexingPressure::Unknown
        } else {
            IndexingPressure::Normal
        }
    }

    pub fn wal_band(self) -> &'static str {
        match self.wal_bytes {
            Some(bytes) if bytes >= WAL_TRUNCATE_MIN_BYTES => "truncate",
            Some(bytes) if bytes >= WAL_RESTART_MIN_BYTES => "restart",
            Some(bytes) if bytes >= WAL_PASSIVE_MIN_BYTES => "checkpoint",
            Some(_) => "normal",
            None => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct IndexingSlice {
    started: Instant,
    wal_bytes: Option<u64>,
    cache_writes: Option<u64>,
    page_size_bytes: Option<u64>,
}

/// Internal pacing policy for source-side I/O performed before relational work.
#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexingIoPacer {
    quiet: bool,
    rate_reservation_path: Option<PathBuf>,
}

impl Default for IndexingIoPacer {
    fn default() -> Self {
        Self {
            quiet: true,
            rate_reservation_path: None,
        }
    }
}

impl IndexingIoPacer {
    #[doc(hidden)]
    pub fn source_io_slice_should_rotate(&self, started: Instant, bytes: u64) -> bool {
        started.elapsed() >= INDEXING_TRANSACTION_MAX || bytes >= INDEXING_WAL_DELTA_BYTES
    }

    #[doc(hidden)]
    pub fn finish_source_io_slice(&self, active: Duration, bytes: u64) {
        let rest = source_io_rest(active, bytes, self.quiet);
        self.rest(rest);
    }

    /// Coordinate source copies which write outside the ctx data filesystem,
    /// such as provider SQLite snapshots in the process temp directory.
    #[doc(hidden)]
    pub fn for_destination_filesystem(&self, destination: &Path) -> Self {
        if !self.quiet {
            return self.clone();
        }
        let rate_reservation_path = filesystem_rate_reservation_path(destination).ok();
        Self {
            quiet: self.quiet,
            rate_reservation_path,
        }
    }

    fn rest(&self, rest: Duration) {
        if rest.is_zero() {
            return;
        }
        if let Some(path) = &self.rate_reservation_path {
            if sleep_for_rate_reservation(path, rest).is_ok() {
                return;
            }
        }
        // A pacing-coordinator failure must not turn quiet work into an
        // unthrottled path. Local sleeping cannot coordinate other processes,
        // but it remains the conservative behavior available to a caller whose
        // filesystem stopped accepting lock-state I/O.
        thread::sleep(rest);
    }
}

#[derive(Clone)]
pub struct IndexingAdmission {
    store_path: PathBuf,
    identity_path: PathBuf,
    class: IndexingWorkClass,
}

impl std::fmt::Debug for IndexingAdmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IndexingAdmission")
            .field("class", &self.work_class())
            .finish_non_exhaustive()
    }
}

pub(crate) struct IndexingWriterLease {
    writer: File,
}

/// A lease for a heavy SQLite sidecar write which shares ctx's global writer
/// admission and quiet-background pacing with the relational store.
#[doc(hidden)]
pub struct ExternalIndexingWriterLease {
    admission: IndexingAdmission,
    lease: Option<IndexingWriterLease>,
    started: Instant,
    wal_path: PathBuf,
    wal_bytes_before: Option<u64>,
    sqlite_footprint_before: Option<u64>,
    sqlite_path: PathBuf,
    estimated_write_bytes: u64,
}

/// A bounded non-SQLite copy slice admitted through the canonical ctx writer
/// lane. The destination may live on another filesystem; pacing for that
/// filesystem is handled by `IndexingIoPacer` after this lease is released.
#[doc(hidden)]
pub struct ExternalIndexingCopyLease {
    _lease: IndexingWriterLease,
}

impl ExternalIndexingWriterLease {
    /// Raises the admitted growth budget after an existing sidecar has been
    /// opened under a zero-growth lease and inspected. This lets reclaiming
    /// cleanup proceed on a full disk without allowing discovered schema work
    /// to bypass fail-closed preflight.
    #[doc(hidden)]
    pub fn require_growth(
        &mut self,
        estimated_write_bytes: u64,
        operation: &'static str,
    ) -> Result<()> {
        ensure_disk_headroom(&self.sqlite_path, estimated_write_bytes, operation)?;
        self.estimated_write_bytes = self.estimated_write_bytes.max(estimated_write_bytes);
        Ok(())
    }

    /// Recheck the current growth budget at the latest safe point before a
    /// sidecar transaction commits.
    #[doc(hidden)]
    pub fn revalidate_growth(&self, operation: &'static str) -> Result<()> {
        ensure_disk_headroom(&self.sqlite_path, self.estimated_write_bytes, operation)
    }
}

impl Drop for IndexingWriterLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.writer);
    }
}

impl Drop for ExternalIndexingWriterLease {
    fn drop(&mut self) {
        self.lease.take();
        let measured = monotonic_delta(self.wal_bytes_before, file_size_if_present(&self.wal_path));
        let footprint_delta = monotonic_delta(
            self.sqlite_footprint_before,
            sqlite_footprint_if_known(&self.sqlite_path),
        );
        self.admission.rest_background(
            self.started.elapsed(),
            Some(
                measured
                    .unwrap_or(0)
                    .max(footprint_delta.unwrap_or(0))
                    .max(self.estimated_write_bytes),
            ),
            None,
        );
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IndexingAdmissionStatus {
    pub writer_active: bool,
    pub foreground_pending: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WalCheckpointStatus {
    pub attempted: bool,
    pub busy: bool,
    pub log_frames: i64,
    pub checkpointed_frames: i64,
    pub wal_bytes: u64,
}

impl WalCheckpointStatus {
    pub fn pinned(self) -> bool {
        self.busy || self.checkpointed_frames < self.log_frames
    }
}

impl IndexingAdmission {
    pub fn acquire(store_path: &Path, class: IndexingWorkClass) -> Result<Self> {
        let store_path = prepare_store_path(store_path)?;
        let identity_path = store_identity_path(&store_path);
        let admission = Self {
            store_path,
            identity_path,
            class,
        };
        open_lock_file(&admission.writer_lock_path())?;
        open_lock_file(&admission.foreground_lock_path())?;
        open_lock_file(&admission.background_reservation_lock_path())?;
        open_lock_file(&admission.rate_reservation_lock_path())?;
        Ok(admission)
    }

    pub fn status(store_path: &Path) -> Result<IndexingAdmissionStatus> {
        let Some(store_path) = canonical_store_path_for_status(store_path)? else {
            return Ok(IndexingAdmissionStatus::default());
        };
        let store_path = store_identity_path(&store_path);
        let Some(writer) = open_existing_lock_file(&lock_path(&store_path, WRITER_LOCK_SUFFIX))?
        else {
            return Ok(IndexingAdmissionStatus::default());
        };
        let foreground = open_existing_lock_file(&lock_path(&store_path, FOREGROUND_LOCK_SUFFIX))?;
        let writer_active = lock_is_held(&writer)?;
        let foreground_pending = foreground
            .as_ref()
            .map(lock_is_held)
            .transpose()?
            .unwrap_or(false);
        Ok(IndexingAdmissionStatus {
            writer_active,
            foreground_pending,
        })
    }

    pub fn work_class(&self) -> IndexingWorkClass {
        self.class
    }

    pub(crate) fn ensure_store_path(&self, store_path: &Path) -> Result<PathBuf> {
        let store_path = prepare_store_path(store_path)?;
        let candidate_writer_lock =
            lock_path(&store_identity_path(&store_path), WRITER_LOCK_SUFFIX);
        if store_paths_share_identity(&self.store_path, &store_path)
            || files_share_identity(&self.writer_lock_path(), &candidate_writer_lock)
        {
            return Ok(store_path);
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "indexing admission belongs to a different ctx index",
        )
        .into())
    }

    pub fn foreground_pending(&self) -> bool {
        if self.class == IndexingWorkClass::Foreground {
            return false;
        }
        let Ok(foreground) = open_lock_file(&self.foreground_lock_path()) else {
            return true;
        };
        match FileExt::try_lock_shared(&foreground) {
            Ok(()) => {
                let _ = FileExt::unlock(&foreground);
                false
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => true,
            Err(_) => true,
        }
    }

    pub(crate) fn lease(&self) -> Result<IndexingWriterLease> {
        match self.class {
            IndexingWorkClass::Foreground => self.acquire_foreground_lease(),
            IndexingWorkClass::Background => {
                let mut backoff = Duration::from_millis(1);
                let waiting_since = Instant::now();
                let mut next_aged_attempt = waiting_since + BACKGROUND_PROGRESS_AFTER;
                loop {
                    if let Some(lease) = self.try_background_lease()? {
                        return Ok(lease);
                    }
                    // Foreground remains preferred, but an aged background
                    // waiter receives one bounded writer slice so recovery and
                    // indexing cannot be starved forever by a continuous stream
                    // of short foreground writers.
                    if Instant::now() >= next_aged_attempt {
                        if let Some(lease) = self.try_reserve_aged_background_lease()? {
                            return Ok(lease);
                        }
                        // Only one aged waiter may reserve the next handoff.
                        // Losing contenders cool down so the foreground waiter
                        // blocked on that reservation deterministically follows
                        // the single bounded background slice.
                        next_aged_attempt = Instant::now() + BACKGROUND_PROGRESS_AFTER;
                    }
                    thread::sleep(backoff);
                    backoff = backoff
                        .saturating_mul(2)
                        .min(BACKGROUND_ADMISSION_MAX_BACKOFF);
                }
            }
        }
    }

    pub(crate) fn try_lease(&self) -> Result<Option<IndexingWriterLease>> {
        match self.class {
            IndexingWorkClass::Foreground => self.try_foreground_lease(),
            IndexingWorkClass::Background => self.try_background_lease(),
        }
    }

    fn acquire_foreground_lease(&self) -> Result<IndexingWriterLease> {
        let foreground = open_lock_file(&self.foreground_lock_path())?;
        let background_reservation = open_lock_file(&self.background_reservation_lock_path())?;
        let writer = open_lock_file(&self.writer_lock_path())?;
        FileExt::lock_exclusive(&foreground)?;
        if let Err(error) = FileExt::lock_shared(&background_reservation) {
            let _ = FileExt::unlock(&foreground);
            return Err(error.into());
        }
        if let Err(error) = FileExt::lock_exclusive(&writer) {
            let _ = FileExt::unlock(&background_reservation);
            let _ = FileExt::unlock(&foreground);
            return Err(error.into());
        }
        FileExt::unlock(&background_reservation)?;
        FileExt::unlock(&foreground)?;
        Ok(IndexingWriterLease { writer })
    }

    fn try_foreground_lease(&self) -> Result<Option<IndexingWriterLease>> {
        let foreground = open_lock_file(&self.foreground_lock_path())?;
        let background_reservation = open_lock_file(&self.background_reservation_lock_path())?;
        let writer = open_lock_file(&self.writer_lock_path())?;
        if !try_lock_exclusive(&foreground)? {
            return Ok(None);
        }
        if !try_lock_shared(&background_reservation)? {
            FileExt::unlock(&foreground)?;
            return Ok(None);
        }
        let writer_locked = try_lock_exclusive(&writer)?;
        FileExt::unlock(&background_reservation)?;
        FileExt::unlock(&foreground)?;
        Ok(writer_locked.then_some(IndexingWriterLease { writer }))
    }

    fn try_background_lease(&self) -> Result<Option<IndexingWriterLease>> {
        let foreground = open_lock_file(&self.foreground_lock_path())?;
        let background_reservation = open_lock_file(&self.background_reservation_lock_path())?;
        let writer = open_lock_file(&self.writer_lock_path())?;
        if !try_lock_shared(&foreground)? {
            return Ok(None);
        }
        if !try_lock_shared(&background_reservation)? {
            FileExt::unlock(&foreground)?;
            return Ok(None);
        }
        let writer_locked = try_lock_exclusive(&writer)?;
        FileExt::unlock(&background_reservation)?;
        FileExt::unlock(&foreground)?;
        Ok(writer_locked.then_some(IndexingWriterLease { writer }))
    }

    fn try_reserve_aged_background_lease(&self) -> Result<Option<IndexingWriterLease>> {
        let background_reservation = open_lock_file(&self.background_reservation_lock_path())?;
        let writer = open_lock_file(&self.writer_lock_path())?;
        if !try_lock_exclusive(&background_reservation)? {
            return Ok(None);
        }
        if let Err(error) = FileExt::lock_exclusive(&writer) {
            let _ = FileExt::unlock(&background_reservation);
            return Err(error.into());
        }
        FileExt::unlock(&background_reservation)?;
        Ok(Some(IndexingWriterLease { writer }))
    }

    /// Admit a bounded heavy write to another SQLite file, such as the semantic
    /// vector sidecar, through the same cross-process writer lane as work.sqlite.
    #[doc(hidden)]
    pub fn acquire_external_writer(
        &self,
        sqlite_path: &Path,
        estimated_write_bytes: u64,
        operation: &'static str,
    ) -> Result<ExternalIndexingWriterLease> {
        let lease = self.lease()?;
        if let Err(error) = ensure_disk_headroom(sqlite_path, estimated_write_bytes, operation) {
            drop(lease);
            return Err(error);
        }
        let wal_path = sqlite_wal_path(sqlite_path);
        Ok(ExternalIndexingWriterLease {
            admission: self.clone(),
            lease: Some(lease),
            started: Instant::now(),
            wal_bytes_before: file_size_if_present(&wal_path),
            sqlite_footprint_before: sqlite_footprint_if_known(sqlite_path),
            sqlite_path: sqlite_path.to_path_buf(),
            wal_path,
            estimated_write_bytes,
        })
    }

    /// Admit one bounded copy slice before its destination file is created or
    /// extended. Headroom is deliberately checked after writer admission so a
    /// queued worker cannot rely on a stale disk observation.
    #[doc(hidden)]
    pub fn acquire_external_copy_slice(
        &self,
        destination: &Path,
        estimated_write_bytes: u64,
        operation: &'static str,
    ) -> Result<ExternalIndexingCopyLease> {
        let lease = self.lease()?;
        ensure_disk_headroom(destination, estimated_write_bytes, operation)?;
        Ok(ExternalIndexingCopyLease { _lease: lease })
    }

    fn writer_lock_path(&self) -> PathBuf {
        lock_path(&self.identity_path, WRITER_LOCK_SUFFIX)
    }

    fn foreground_lock_path(&self) -> PathBuf {
        lock_path(&self.identity_path, FOREGROUND_LOCK_SUFFIX)
    }

    fn background_reservation_lock_path(&self) -> PathBuf {
        lock_path(&self.identity_path, BACKGROUND_RESERVATION_LOCK_SUFFIX)
    }

    fn rate_reservation_lock_path(&self) -> PathBuf {
        lock_path(&self.identity_path, RATE_RESERVATION_LOCK_SUFFIX)
    }

    fn rest_background(
        &self,
        active: Duration,
        measured_bytes: Option<u64>,
        max_rest: Option<Duration>,
    ) {
        if self.class != IndexingWorkClass::Background {
            return;
        }
        let rest = background_rest_with_limit(active, measured_bytes, max_rest);
        IndexingIoPacer {
            quiet: true,
            rate_reservation_path: Some(self.rate_reservation_lock_path()),
        }
        .rest(rest);
    }
}

impl Store {
    pub fn ensure_disk_headroom(
        &self,
        estimated_write_bytes: u64,
        operation: &'static str,
    ) -> Result<()> {
        ensure_disk_headroom(&self.path, estimated_write_bytes, operation)
    }

    pub(crate) fn indexing_writer_lease_held(&self) -> bool {
        self.indexing_writer_lease.borrow().is_some()
    }

    pub(crate) fn acquire_indexing_writer_lease(&self, nonblocking: bool) -> Result<bool> {
        if self.indexing_writer_lease.borrow().is_some() {
            return Ok(true);
        }
        let Some(admission) = &self.indexing_admission else {
            return Ok(true);
        };
        let lease = if nonblocking {
            admission.try_lease()?
        } else {
            Some(admission.lease()?)
        };
        let Some(lease) = lease else {
            return Ok(false);
        };
        *self.indexing_writer_lease.borrow_mut() = Some(lease);
        Ok(true)
    }

    pub(crate) fn release_indexing_writer_lease(&self) {
        self.indexing_writer_lease.borrow_mut().take();
    }

    pub(crate) fn with_indexing_writer_lease<T>(
        &self,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let already_held = self.indexing_writer_lease.borrow().is_some();
        self.acquire_indexing_writer_lease(false)?;
        let result = operation();
        if !already_held {
            self.release_indexing_writer_lease();
        }
        result
    }

    pub(crate) fn try_with_indexing_writer_lease<T>(
        &self,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<Option<T>> {
        let already_held = self.indexing_writer_lease.borrow().is_some();
        if !self.acquire_indexing_writer_lease(true)? {
            return Ok(None);
        }
        let result = operation();
        if !already_held {
            self.release_indexing_writer_lease();
        }
        result.map(Some)
    }

    pub fn begin_indexing_slice(&self) -> Result<IndexingSlice> {
        Ok(IndexingSlice {
            started: Instant::now(),
            wal_bytes: self.wal_bytes()?,
            cache_writes: sqlite_cache_write_count(&self.conn),
            page_size_bytes: sqlite_page_size(&self.conn),
        })
    }

    pub fn indexing_slice_should_rotate(&self, slice: &IndexingSlice) -> Result<bool> {
        if slice.started.elapsed() >= INDEXING_TRANSACTION_MAX {
            return Ok(true);
        }
        let wal_delta = self
            .wal_bytes()?
            .unwrap_or(0)
            .saturating_sub(slice.wal_bytes.unwrap_or(0));
        if wal_delta >= INDEXING_WAL_DELTA_BYTES {
            return Ok(true);
        }
        let Some(admission) = &self.indexing_admission else {
            return Ok(false);
        };
        if admission.work_class() != IndexingWorkClass::Background {
            return Ok(false);
        }
        if admission.foreground_pending() {
            return Ok(true);
        }
        Ok(
            IndexingResourceSnapshot::current(&self.path, self.wal_bytes()?).pressure()
                != IndexingPressure::Normal,
        )
    }

    pub fn finish_indexing_slice(&self, slice: IndexingSlice) -> Result<()> {
        self.finish_indexing_slice_with_checkpoint_mode(slice, false)
    }

    pub(crate) fn finish_indexing_slice_with_checkpoint_mode(
        &self,
        slice: IndexingSlice,
        nonblocking_checkpoint: bool,
    ) -> Result<()> {
        let (wal_delta, cache_write_bytes) = self.indexing_slice_write_deltas(&slice)?;
        let checkpointed_frames = if nonblocking_checkpoint {
            self.try_checkpoint_wal_for_pressure()?
                .map(|(_, frames)| frames)
                .unwrap_or(0)
        } else {
            self.checkpoint_wal_for_pressure_work()?.1
        };
        let measured_bytes = physical_write_bytes(
            wal_delta,
            cache_write_bytes,
            slice.page_size_bytes,
            checkpointed_frames,
        );
        if let Some(admission) = &self.indexing_admission {
            admission.rest_background(slice.started.elapsed(), measured_bytes, None);
        }
        Ok(())
    }

    pub(crate) fn finish_indexing_checkpoint(
        &self,
        slice: IndexingSlice,
        checkpointed_frames: u64,
    ) {
        let measured_bytes = checkpoint_write_bytes(slice.page_size_bytes, checkpointed_frames);
        if let Some(admission) = &self.indexing_admission {
            admission.rest_background(slice.started.elapsed(), measured_bytes, None);
        }
    }

    #[cfg(test)]
    fn indexing_slice_write_bytes(&self, slice: &IndexingSlice) -> Result<Option<u64>> {
        let (wal_delta, cache_write_bytes) = self.indexing_slice_write_deltas(slice)?;
        Ok(physical_write_bytes(
            wal_delta,
            cache_write_bytes,
            slice.page_size_bytes,
            0,
        ))
    }

    fn indexing_slice_write_deltas(
        &self,
        slice: &IndexingSlice,
    ) -> Result<(Option<u64>, Option<u64>)> {
        let wal_delta = monotonic_delta(slice.wal_bytes, self.wal_bytes()?);
        let cache_write_bytes =
            monotonic_delta(slice.cache_writes, sqlite_cache_write_count(&self.conn))
                .zip(slice.page_size_bytes)
                .map(|(pages, page_size)| pages.saturating_mul(page_size));
        Ok((wal_delta, cache_write_bytes))
    }

    pub fn yield_indexing_admission(&self, active: Duration) -> Result<()> {
        if let Some(admission) = &self.indexing_admission {
            admission.rest_background(active, None, None);
        }
        Ok(())
    }

    pub fn yield_indexing_admission_with_budget(
        &self,
        active: Duration,
        remaining: Option<Duration>,
    ) -> Result<()> {
        if let Some(admission) = &self.indexing_admission {
            admission.rest_background(active, None, remaining);
        }
        Ok(())
    }

    pub fn indexing_work_class(&self) -> Option<IndexingWorkClass> {
        self.indexing_admission
            .as_ref()
            .map(IndexingAdmission::work_class)
    }

    /// Admission descriptor for bounded source-side copy work.
    #[doc(hidden)]
    pub fn indexing_admission(&self) -> Option<IndexingAdmission> {
        self.indexing_admission.clone()
    }

    #[doc(hidden)]
    pub fn indexing_io_pacer(&self) -> IndexingIoPacer {
        let quiet = self.indexing_work_class() != Some(IndexingWorkClass::Foreground);
        IndexingIoPacer {
            quiet,
            rate_reservation_path: quiet
                .then_some(self.indexing_admission.as_ref())
                .flatten()
                .map(IndexingAdmission::rate_reservation_lock_path),
        }
    }
}

fn sleep_for_rate_reservation(path: &Path, rest: Duration) -> io::Result<()> {
    let now = unix_time_nanos()?;
    let deadline = reserve_rate_duration_at(path, rest, now)?;
    let wait_nanos = deadline.saturating_sub(now).min(u128::from(u64::MAX));
    if wait_nanos > 0 {
        thread::sleep(Duration::from_nanos(wait_nanos as u64));
    }
    Ok(())
}

fn reserve_rate_duration_at(path: &Path, rest: Duration, now_nanos: u128) -> io::Result<u128> {
    let mut state = open_rate_reservation_file(path)?;
    FileExt::lock_exclusive(&state)?;
    let result = (|| {
        let next_nanos = read_rate_reservation(&mut state)
            .filter(|next| *next >= now_nanos)
            .unwrap_or(now_nanos);
        let deadline = next_nanos.saturating_add(rest.as_nanos());
        write_rate_reservation(&mut state, deadline)?;
        Ok(deadline)
    })();
    let unlock_result = FileExt::unlock(&state);
    match (result, unlock_result) {
        (Ok(deadline), Ok(())) => Ok(deadline),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

fn read_rate_reservation(file: &mut File) -> Option<u128> {
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut state = String::new();
    file.read_to_string(&mut state).ok()?;
    let mut fields = state.split_whitespace();
    (fields.next()? == RATE_RESERVATION_VERSION)
        .then(|| fields.next()?.parse::<u128>().ok())
        .flatten()
        .filter(|_| fields.next().is_none())
}

fn write_rate_reservation(file: &mut File, deadline_nanos: u128) -> io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    writeln!(file, "{RATE_RESERVATION_VERSION} {deadline_nanos}")?;
    file.flush()
}

fn unix_time_nanos() -> io::Result<u128> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(io::Error::other)
}

fn open_rate_reservation_file(path: &Path) -> io::Result<File> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn filesystem_rate_reservation_path(destination: &Path) -> io::Result<PathBuf> {
    let root = nearest_existing_ancestor(destination)?;
    let coordinator_parent = std::env::temp_dir().join(filesystem_rate_directory_name(&root));
    fs::create_dir_all(&coordinator_parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&coordinator_parent, fs::Permissions::from_mode(0o700))?;
    }
    Ok(coordinator_parent.join("rate-reservation.lock"))
}

fn nearest_existing_ancestor(path: &Path) -> io::Result<PathBuf> {
    let mut candidate = path;
    loop {
        match fs::canonicalize(candidate) {
            Ok(path) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                candidate = candidate.parent().ok_or(error)?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn filesystem_rate_directory_name(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(metadata) = fs::metadata(path) {
            return format!(
                "ctx-indexing-rate-{}-{}",
                unsafe { libc::geteuid() },
                metadata.dev()
            );
        }
    }
    // Windows temp directories are user-specific. Hash the canonical volume
    // path without exposing it in the coordinator filename.
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    format!("ctx-indexing-rate-{:016x}", hasher.finish())
}

fn duty_cycle_rest(active: Duration) -> Duration {
    Duration::from_nanos(
        active
            .as_nanos()
            .saturating_mul(3)
            .min(u128::from(u64::MAX)) as u64,
    )
}

fn bandwidth_rest(active: Duration, bytes: u64) -> Duration {
    let numerator = u128::from(bytes).saturating_mul(1_000_000_000);
    let denominator = u128::from(QUIET_INDEXING_BANDWIDTH_BYTES_PER_SEC);
    let required_nanos = numerator.saturating_add(denominator.saturating_sub(1)) / denominator;
    let required = Duration::from_nanos(required_nanos.min(u128::from(u64::MAX)) as u64);
    required.saturating_sub(active)
}

fn quiet_rest(active: Duration, bytes: Option<u64>) -> Duration {
    bytes
        .map(|bytes| duty_cycle_rest(active).max(bandwidth_rest(active, bytes)))
        .unwrap_or_else(|| duty_cycle_rest(active))
}

fn source_io_rest(active: Duration, bytes: u64, quiet: bool) -> Duration {
    if quiet {
        quiet_rest(active, Some(bytes))
    } else {
        bandwidth_rest(active, bytes)
    }
}

fn background_rest_with_limit(
    active: Duration,
    measured_bytes: Option<u64>,
    max_rest: Option<Duration>,
) -> Duration {
    max_rest
        .map(|limit| quiet_rest(active, measured_bytes).min(limit))
        .unwrap_or_else(|| quiet_rest(active, measured_bytes))
}

fn monotonic_delta(before: Option<u64>, after: Option<u64>) -> Option<u64> {
    let (before, after) = before.zip(after)?;
    after.checked_sub(before)
}

fn max_optional(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn physical_write_bytes(
    wal_delta: Option<u64>,
    cache_write_bytes: Option<u64>,
    page_size_bytes: Option<u64>,
    checkpointed_frames: u64,
) -> Option<u64> {
    let direct_write_bytes = max_optional(wal_delta, cache_write_bytes);
    if checkpointed_frames == 0 {
        return direct_write_bytes;
    }
    let checkpoint_write_bytes = checkpoint_write_bytes(page_size_bytes, checkpointed_frames);
    match (direct_write_bytes, checkpoint_write_bytes) {
        (Some(direct), Some(checkpoint)) => Some(direct.saturating_add(checkpoint)),
        (Some(bytes), None) | (None, Some(bytes)) => Some(bytes),
        (None, None) => None,
    }
}

fn checkpoint_write_bytes(page_size_bytes: Option<u64>, checkpointed_frames: u64) -> Option<u64> {
    (checkpointed_frames > 0)
        .then(|| page_size_bytes.map(|page_size| checkpointed_frames.saturating_mul(page_size)))
        .flatten()
}

fn sqlite_cache_write_count(conn: &Connection) -> Option<u64> {
    let mut current = 0_i32;
    let mut highwater = 0_i32;
    // SAFETY: the handle remains owned by `conn`, this synchronous status read
    // does not retain it, and reset=0 leaves SQLite's counter untouched.
    let result = unsafe {
        ffi::sqlite3_db_status(
            conn.handle(),
            ffi::SQLITE_DBSTATUS_CACHE_WRITE,
            &mut current,
            &mut highwater,
            0,
        )
    };
    (result == ffi::SQLITE_OK && current >= 0).then_some(current as u64)
}

pub(crate) fn sqlite_page_size(conn: &Connection) -> Option<u64> {
    conn.query_row("PRAGMA page_size", [], |row| row.get::<_, u64>(0))
        .ok()
        .filter(|size| *size > 0)
}

fn lock_path(store_path: &Path, suffix: &str) -> PathBuf {
    let mut path = store_path.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn sqlite_wal_path(sqlite_path: &Path) -> PathBuf {
    let mut path = sqlite_path.as_os_str().to_os_string();
    path.push("-wal");
    PathBuf::from(path)
}

fn sqlite_journal_path(sqlite_path: &Path) -> PathBuf {
    let mut path = sqlite_path.as_os_str().to_os_string();
    path.push("-journal");
    PathBuf::from(path)
}

fn sqlite_shm_path(sqlite_path: &Path) -> PathBuf {
    let mut path = sqlite_path.as_os_str().to_os_string();
    path.push("-shm");
    PathBuf::from(path)
}

fn file_size_if_present(path: &Path) -> Option<u64> {
    match fs::metadata(path) {
        Ok(metadata) => Some(metadata.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some(0),
        Err(_) => None,
    }
}

fn sqlite_footprint_if_known(sqlite_path: &Path) -> Option<u64> {
    [
        sqlite_path.to_path_buf(),
        sqlite_wal_path(sqlite_path),
        sqlite_journal_path(sqlite_path),
        sqlite_shm_path(sqlite_path),
    ]
    .into_iter()
    .try_fold(0_u64, |total, path| {
        file_size_if_present(&path).map(|bytes| total.saturating_add(bytes))
    })
}

/// Conservatively sizes a SQLite operation from the current database and
/// sidecar footprint. Metadata failures are errors because treating an
/// unreadable large file as empty would defeat low-disk admission.
#[doc(hidden)]
pub fn sqlite_amplifying_write_estimate(
    sqlite_path: &Path,
    amplification: u64,
    minimum_bytes: u64,
) -> Result<u64> {
    let footprint = sqlite_footprint(sqlite_path)?;
    Ok(footprint.saturating_mul(amplification).max(minimum_bytes))
}

fn sqlite_footprint(sqlite_path: &Path) -> Result<u64> {
    [
        sqlite_path.to_path_buf(),
        sqlite_wal_path(sqlite_path),
        sqlite_journal_path(sqlite_path),
        sqlite_shm_path(sqlite_path),
    ]
    .into_iter()
    .try_fold(0_u64, |total, path| match fs::metadata(&path) {
        Ok(metadata) => Ok(total.saturating_add(metadata.len())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(total),
        Err(error) => Err(error.into()),
    })
}

fn ensure_disk_headroom(
    sqlite_path: &Path,
    estimated_write_bytes: u64,
    operation: &'static str,
) -> Result<()> {
    if estimated_write_bytes == 0 {
        return Ok(());
    }
    #[cfg(test)]
    if let Some(result) = TEST_DISK_SPACE_PROBE.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .map(|probe| probe(sqlite_path, operation))
    }) {
        return ensure_disk_headroom_with_probe(
            sqlite_path,
            estimated_write_bytes,
            operation,
            || result,
        );
    }
    let parent = sqlite_path.parent().unwrap_or_else(|| Path::new("."));
    ensure_disk_headroom_with_probe(sqlite_path, estimated_write_bytes, operation, || {
        fs2::available_space(parent)
    })
}

/// Fail-closed disk preflight for bounded external indexing copies.
#[doc(hidden)]
pub fn ensure_indexing_disk_headroom(
    destination: &Path,
    estimated_write_bytes: u64,
    operation: &'static str,
) -> Result<()> {
    ensure_disk_headroom(destination, estimated_write_bytes, operation)
}

fn ensure_disk_headroom_with_probe(
    sqlite_path: &Path,
    estimated_write_bytes: u64,
    operation: &'static str,
    probe: impl FnOnce() -> io::Result<u64>,
) -> Result<()> {
    if estimated_write_bytes == 0 {
        return Ok(());
    }
    let available = probe().map_err(|source| StoreError::DiskSpaceProbeFailed {
        operation,
        path: sqlite_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
        source,
    })?;
    let required = estimated_write_bytes.saturating_add(INDEXING_DISK_RESERVE_BYTES);
    if !disk_headroom_is_sufficient(available, estimated_write_bytes) {
        return Err(StoreError::InsufficientDiskSpace {
            operation,
            required_bytes: required,
            available_bytes: available,
        });
    }
    Ok(())
}

fn disk_headroom_is_sufficient(available: u64, estimated_write_bytes: u64) -> bool {
    available >= estimated_write_bytes.saturating_add(INDEXING_DISK_RESERVE_BYTES)
}

fn store_has_multiple_hard_links(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(metadata) = fs::metadata(path) else {
            return false;
        };
        metadata.nlink() > 1
    }
    #[cfg(windows)]
    {
        File::open(path)
            .and_then(|file| windows_file_identity(&file))
            .is_ok_and(|identity| identity.links > 1)
    }
    #[cfg(not(any(unix, windows)))]
    false
}

fn store_identity_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn store_paths_share_identity(left: &Path, right: &Path) -> bool {
    left == right
}

fn files_share_identity(left_path: &Path, right_path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Some((left, right)) = fs::metadata(left_path)
            .ok()
            .zip(fs::metadata(right_path).ok())
        else {
            return false;
        };
        left.dev() == right.dev() && left.ino() == right.ino()
    }
    #[cfg(windows)]
    {
        let Ok(left) = File::open(left_path).and_then(|file| windows_file_identity(&file)) else {
            return false;
        };
        let Ok(right) = File::open(right_path).and_then(|file| windows_file_identity(&file)) else {
            return false;
        };
        left.volume_serial == right.volume_serial && left.file_index == right.file_index
    }
    #[cfg(not(any(unix, windows)))]
    false
}

pub(crate) fn prepare_store_path(path: &Path) -> Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "ctx index path has no file name",
        )
    })?;
    let parent = nonempty_parent(path);
    create_private_dir_all(parent)?;
    let canonical_parent = fs::canonicalize(parent)?;
    let candidate = canonical_parent.join(file_name);
    let canonical = match fs::canonicalize(&candidate) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if fs::symlink_metadata(&candidate).is_ok_and(|metadata| metadata.is_symlink()) {
                return Err(error.into());
            }
            candidate
        }
        Err(error) => return Err(error.into()),
    };
    if store_has_multiple_hard_links(&canonical) {
        return Err(StoreError::UnsafeStoreAlias(canonical));
    }
    if let Some(parent) = canonical.parent() {
        create_private_dir_all(parent)?;
    }
    Ok(canonical)
}

pub(crate) fn canonical_existing_store_path(path: &Path) -> Result<PathBuf> {
    Ok(fs::canonicalize(path)?)
}

fn canonical_store_path_for_status(path: &Path) -> Result<Option<PathBuf>> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let Some(file_name) = path.file_name() else {
                return Ok(None);
            };
            match fs::canonicalize(nonempty_parent(path)) {
                Ok(parent) => Ok(Some(parent.join(file_name))),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn nonempty_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn create_private_dir_all(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700).create(path)?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path)?;

    // Creation modes do not change an existing directory, so keep repairing
    // installations created by older releases or a different umask.
    restrict_private_dir(path)?;
    Ok(())
}

fn open_lock_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    let file = options.open(path)?;
    // The create mode is ignored for existing files.
    restrict_private_file(path)?;
    Ok(file)
}

fn open_existing_lock_file(path: &Path) -> Result<Option<File>> {
    match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StoreError::Io(error)),
    }
}

fn lock_is_held(file: &File) -> Result<bool> {
    match FileExt::try_lock_exclusive(file) {
        Ok(()) => {
            FileExt::unlock(file)?;
            Ok(false)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(StoreError::Io(error)),
    }
}

fn try_lock_exclusive(file: &File) -> Result<bool> {
    match FileExt::try_lock_exclusive(file) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
        Err(error) => Err(StoreError::Io(error)),
    }
}

fn try_lock_shared(file: &File) -> Result<bool> {
    match FileExt::try_lock_shared(file) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
        Err(error) => Err(StoreError::Io(error)),
    }
}

#[cfg(target_os = "linux")]
pub fn system_memory() -> (Option<u64>, Option<u64>) {
    let Ok(text) = fs::read_to_string("/proc/meminfo") else {
        return (None, None);
    };
    let mut total = None;
    let mut available = None;
    for line in text.lines() {
        if let Some(value) = meminfo_kib(line, "MemTotal:") {
            total = Some(value);
        } else if let Some(value) = meminfo_kib(line, "MemAvailable:") {
            available = Some(value);
        }
    }
    (total, available)
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn sysctl_number(name: &'static [u8]) -> Option<u64> {
    let mut value = 0_u64;
    let mut size = std::mem::size_of::<u64>();
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr().cast(),
            (&mut value as *mut u64).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return None;
    }
    match size {
        4 => Some(value & u64::from(u32::MAX)),
        8 => Some(value),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
pub fn system_memory() -> (Option<u64>, Option<u64>) {
    let total = sysctl_number(b"hw.memsize\0");
    let page_size = sysctl_number(b"hw.pagesize\0");
    let available_pages = [
        b"vm.page_free_count\0".as_slice(),
        b"vm.page_inactive_count\0".as_slice(),
        b"vm.page_speculative_count\0".as_slice(),
        b"vm.page_purgeable_count\0".as_slice(),
    ]
    .into_iter()
    .map(sysctl_number)
    .try_fold(0_u64, |total, value| total.checked_add(value?));
    (
        total,
        page_size.and_then(|size| available_pages?.checked_mul(size)),
    )
}

#[cfg(target_os = "freebsd")]
pub fn system_memory() -> (Option<u64>, Option<u64>) {
    let total = sysctl_number(b"hw.physmem\0");
    let page_size = sysctl_number(b"vm.stats.vm.v_page_size\0");
    let available_pages = [
        b"vm.stats.vm.v_free_count\0".as_slice(),
        b"vm.stats.vm.v_inactive_count\0".as_slice(),
        b"vm.stats.vm.v_cache_count\0".as_slice(),
    ]
    .into_iter()
    .map(sysctl_number)
    .try_fold(0_u64, |total, value| total.checked_add(value?));
    (
        total,
        page_size.and_then(|size| available_pages?.checked_mul(size)),
    )
}

#[cfg(target_os = "windows")]
pub fn system_memory() -> (Option<u64>, Option<u64>) {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }
    let mut status = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        memory_load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page_file: 0,
        avail_page_file: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended_virtual: 0,
    };
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return (None, None);
    }
    (Some(status.total_phys), Some(status.avail_phys))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "windows"
)))]
pub fn system_memory() -> (Option<u64>, Option<u64>) {
    (None, None)
}

#[cfg(target_os = "linux")]
fn meminfo_kib(line: &str, key: &str) -> Option<u64> {
    let mut fields = line.strip_prefix(key)?.split_whitespace();
    let kib = fields.next()?.parse::<u64>().ok()?;
    if fields.next()? != "kB" || fields.next().is_some() {
        return None;
    }
    kib.checked_mul(1024)
}

#[cfg(test)]
mod tests {
    use std::{env, io::Write, process::Command, sync::mpsc};

    use super::*;

    const PRIORITY_DB_ENV: &str = "CTX_TEST_PRIORITY_DB";
    const PRIORITY_ROLE_ENV: &str = "CTX_TEST_PRIORITY_ROLE";
    const PRIORITY_READY_ENV: &str = "CTX_TEST_PRIORITY_READY";
    const PRIORITY_RELEASE_ENV: &str = "CTX_TEST_PRIORITY_RELEASE";
    const PRIORITY_ORDER_ENV: &str = "CTX_TEST_PRIORITY_ORDER";
    const RATE_DB_ENV: &str = "CTX_TEST_RATE_DB";
    const RATE_RESULT_ENV: &str = "CTX_TEST_RATE_RESULT";
    #[cfg(unix)]
    const PRIVATE_LOCK_DB_ENV: &str = "CTX_TEST_PRIVATE_LOCK_DB";

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn missing_leaf_case_aliases_have_one_admission_identity() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path();
        let upper = parent.join("Ctx.DB");
        let lower = parent.join("ctx.db");
        let admission = IndexingAdmission::acquire(&upper, IndexingWorkClass::Background).unwrap();
        let upper_lock = lock_path(&upper, WRITER_LOCK_SUFFIX);
        let lower_lock = lock_path(&lower, WRITER_LOCK_SUFFIX);
        let filesystem_aliases = files_share_identity(&upper_lock, &lower_lock);
        assert_eq!(
            admission.ensure_store_path(&lower).is_ok(),
            filesystem_aliases
        );
    }

    #[test]
    fn admission_priority_child_helper() {
        let Some(db_path) = env::var_os(PRIORITY_DB_ENV).map(PathBuf::from) else {
            return;
        };
        let role = env::var(PRIORITY_ROLE_ENV).unwrap();
        let class = if role == "foreground" {
            IndexingWorkClass::Foreground
        } else {
            IndexingWorkClass::Background
        };
        let admission = IndexingAdmission::acquire(&db_path, class).unwrap();
        let _lease = admission.lease().unwrap();
        if role == "active" {
            let ready = PathBuf::from(env::var_os(PRIORITY_READY_ENV).unwrap());
            let release = PathBuf::from(env::var_os(PRIORITY_RELEASE_ENV).unwrap());
            fs::write(ready, b"ready").unwrap();
            while !release.exists() {
                thread::sleep(Duration::from_millis(2));
            }
            return;
        }
        let order = PathBuf::from(env::var_os(PRIORITY_ORDER_ENV).unwrap());
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(order)
            .unwrap();
        writeln!(file, "{role}").unwrap();
    }

    #[test]
    fn aggregate_rate_reservation_child_helper() {
        let Some(db_path) = env::var_os(RATE_DB_ENV).map(PathBuf::from) else {
            return;
        };
        let result_path = PathBuf::from(env::var_os(RATE_RESULT_ENV).unwrap());
        let admission =
            IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
        let deadline = reserve_rate_duration_at(
            &admission.rate_reservation_lock_path(),
            Duration::from_secs(1),
            1_000_000_000_000,
        )
        .unwrap();
        fs::write(result_path, deadline.to_string()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_lock_creation_with_permissive_umask_child() {
        let Some(db_path) = env::var_os(PRIVATE_LOCK_DB_ENV).map(PathBuf::from) else {
            return;
        };
        // SAFETY: this helper runs alone in a short-lived subprocess.
        unsafe {
            libc::umask(0);
        }
        IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn admission_creates_private_lock_paths_and_repairs_existing_modes() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let db_path = temp
            .path()
            .join("fresh")
            .join("private")
            .join("work.sqlite");
        let status = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "work_control::tests::private_lock_creation_with_permissive_umask_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PRIVATE_LOCK_DB_ENV, &db_path)
            .status()
            .unwrap();
        assert!(status.success());

        let parent = db_path.parent().unwrap();
        let lock_paths = [
            lock_path(&db_path, WRITER_LOCK_SUFFIX),
            lock_path(&db_path, FOREGROUND_LOCK_SUFFIX),
            lock_path(&db_path, BACKGROUND_RESERVATION_LOCK_SUFFIX),
        ];
        assert_eq!(
            fs::metadata(parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for path in &lock_paths {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        fs::set_permissions(parent, fs::Permissions::from_mode(0o777)).unwrap();
        for path in &lock_paths {
            fs::set_permissions(path, fs::Permissions::from_mode(0o666)).unwrap();
        }
        IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
        assert_eq!(
            fs::metadata(parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for path in lock_paths {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn hard_linked_database_aliases_are_rejected_before_sqlite_open() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        drop(Store::open(&db_path).unwrap());
        let alias = temp.path().join("work-alias.sqlite");
        fs::hard_link(&db_path, &alias).unwrap();

        assert!(matches!(
            IndexingAdmission::acquire(&db_path, IndexingWorkClass::Foreground),
            Err(StoreError::UnsafeStoreAlias(path)) if path == db_path
        ));
        assert!(matches!(
            IndexingAdmission::acquire(&alias, IndexingWorkClass::Background),
            Err(StoreError::UnsafeStoreAlias(path)) if path == alias
        ));
    }

    #[test]
    fn foreground_wins_multiprocess_handoff_over_queued_background() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let ready = temp.path().join("ready");
        let release = temp.path().join("release");
        let order = temp.path().join("order");
        let test_exe = env::current_exe().unwrap();
        let child_args = [
            "--exact",
            "work_control::tests::admission_priority_child_helper",
            "--nocapture",
            "--test-threads=1",
        ];
        let mut active = Command::new(&test_exe)
            .args(child_args)
            .env(PRIORITY_DB_ENV, &db_path)
            .env(PRIORITY_ROLE_ENV, "active")
            .env(PRIORITY_READY_ENV, &ready)
            .env(PRIORITY_RELEASE_ENV, &release)
            .spawn()
            .unwrap();
        wait_for_path(&ready);

        let mut queued = Command::new(&test_exe)
            .args(child_args)
            .env(PRIORITY_DB_ENV, &db_path)
            .env(PRIORITY_ROLE_ENV, "background")
            .env(PRIORITY_ORDER_ENV, &order)
            .spawn()
            .unwrap();
        thread::sleep(Duration::from_millis(25));
        let mut foreground = Command::new(&test_exe)
            .args(child_args)
            .env(PRIORITY_DB_ENV, &db_path)
            .env(PRIORITY_ROLE_ENV, "foreground")
            .env(PRIORITY_ORDER_ENV, &order)
            .spawn()
            .unwrap();

        let observer = IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !observer.foreground_pending() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(observer.foreground_pending());
        fs::write(&release, b"release").unwrap();

        assert!(active.wait().unwrap().success());
        assert!(foreground.wait().unwrap().success());
        assert!(queued.wait().unwrap().success());
        let order = fs::read_to_string(order).unwrap();
        assert_eq!(order.lines().next(), Some("foreground"), "{order:?}");
    }

    #[test]
    fn background_rate_is_aggregate_across_real_processes() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let test_exe = env::current_exe().unwrap();
        let child_args = [
            "--exact",
            "work_control::tests::aggregate_rate_reservation_child_helper",
            "--nocapture",
            "--test-threads=1",
        ];
        let mut children = Vec::new();
        let mut result_paths = Vec::new();
        for index in 0..3 {
            let result = temp.path().join(format!("rate-{index}"));
            children.push(
                Command::new(&test_exe)
                    .args(child_args)
                    .env(RATE_DB_ENV, &db_path)
                    .env(RATE_RESULT_ENV, &result)
                    .spawn()
                    .unwrap(),
            );
            result_paths.push(result);
        }
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        let mut deadlines = result_paths
            .iter()
            .map(|path| fs::read_to_string(path).unwrap().parse::<u128>().unwrap())
            .collect::<Vec<_>>();
        deadlines.sort_unstable();
        assert_eq!(
            deadlines,
            vec![1_001_000_000_000, 1_002_000_000_000, 1_003_000_000_000,]
        );
    }

    #[test]
    fn aged_background_waiter_receives_one_progress_slice() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let admission =
            IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
        let foreground_gate = open_lock_file(&admission.foreground_lock_path()).unwrap();
        FileExt::lock_exclusive(&foreground_gate).unwrap();

        let waiting = admission.clone();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _lease = waiting.lease().unwrap();
            acquired_tx.send(()).unwrap();
        });
        acquired_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("aged background work should receive a bounded progress slice");
        FileExt::unlock(&foreground_gate).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn aged_background_reservation_hands_off_before_new_foreground() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let active = IndexingAdmission::acquire(&db_path, IndexingWorkClass::Foreground).unwrap();
        let active_lease = active.lease().unwrap();

        let background =
            IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
        let (background_acquired_tx, background_acquired_rx) = mpsc::channel();
        let (release_background_tx, release_background_rx) = mpsc::channel();
        let background_worker = thread::spawn(move || {
            let _lease = background.lease().unwrap();
            background_acquired_tx.send(()).unwrap();
            release_background_rx.recv().unwrap();
        });

        let reservation = open_lock_file(&active.background_reservation_lock_path()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match FileExt::try_lock_shared(&reservation) {
                Ok(()) => {
                    FileExt::unlock(&reservation).unwrap();
                    assert!(
                        Instant::now() < deadline,
                        "background never reserved its handoff"
                    );
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("reservation probe failed: {error}"),
            }
        }

        let foreground = active.clone();
        let (foreground_acquired_tx, foreground_acquired_rx) = mpsc::channel();
        let foreground_worker = thread::spawn(move || {
            let _lease = foreground.lease().unwrap();
            foreground_acquired_tx.send(()).unwrap();
        });
        drop(active_lease);
        background_acquired_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("reserved background handoff should acquire next");
        assert!(foreground_acquired_rx.try_recv().is_err());
        release_background_tx.send(()).unwrap();
        foreground_acquired_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("foreground should continue after the bounded background slice");
        background_worker.join().unwrap();
        foreground_worker.join().unwrap();
    }

    #[test]
    fn one_aged_background_slice_is_followed_by_foreground_before_other_waiters() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let foreground =
            IndexingAdmission::acquire(&db_path, IndexingWorkClass::Foreground).unwrap();
        let active_lease = foreground.lease().unwrap();
        let background =
            IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
        let (order_tx, order_rx) = mpsc::channel();
        let mut releases = Vec::new();
        let mut workers = Vec::new();
        for index in 0..2 {
            let waiter = background.clone();
            let acquired = order_tx.clone();
            let (release_tx, release_rx) = mpsc::channel();
            releases.push(Some(release_tx));
            workers.push(thread::spawn(move || {
                let _lease = waiter.lease().unwrap();
                acquired.send(index).unwrap();
                release_rx.recv().unwrap();
            }));
        }

        let reservation = open_lock_file(&foreground.background_reservation_lock_path()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match FileExt::try_lock_shared(&reservation) {
                Ok(()) => {
                    FileExt::unlock(&reservation).unwrap();
                    assert!(
                        Instant::now() < deadline,
                        "no aged waiter reserved a handoff"
                    );
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("reservation probe failed: {error}"),
            }
        }

        let foreground_waiter = foreground.clone();
        let foreground_order = order_tx.clone();
        let foreground_worker = thread::spawn(move || {
            let _lease = foreground_waiter.lease().unwrap();
            foreground_order.send(usize::MAX).unwrap();
        });
        drop(active_lease);

        let first = order_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_ne!(first, usize::MAX);
        releases[first].take().unwrap().send(()).unwrap();
        assert_eq!(
            order_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            usize::MAX,
            "a second aged waiter formed a convoy ahead of foreground"
        );
        let second = order_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_ne!(second, first);
        assert_ne!(second, usize::MAX);
        releases[second].take().unwrap().send(()).unwrap();

        for worker in workers {
            worker.join().unwrap();
        }
        foreground_worker.join().unwrap();
    }

    fn wait_for_path(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(path.exists(), "timed out waiting for {}", path.display());
    }

    #[test]
    fn resource_policy_is_conservative_for_missing_and_pressure_signals() {
        assert_eq!(
            IndexingResourceSnapshot::default().pressure(),
            IndexingPressure::Unknown
        );
        assert_eq!(
            IndexingResourceSnapshot {
                available_memory_bytes: Some(99),
                available_disk_bytes: Some(WAL_TRUNCATE_MIN_BYTES * 4),
                wal_bytes: Some(0),
            }
            .pressure(),
            IndexingPressure::Constrained
        );
        assert_eq!(
            IndexingResourceSnapshot {
                available_memory_bytes: Some(WAL_TRUNCATE_MIN_BYTES * 4),
                available_disk_bytes: Some(WAL_TRUNCATE_MIN_BYTES * 4),
                wal_bytes: Some(WAL_PASSIVE_MIN_BYTES),
            }
            .pressure(),
            IndexingPressure::Normal
        );
    }

    #[test]
    fn disk_headroom_requires_estimated_amplification_plus_reserve() {
        assert!(!disk_headroom_is_sufficient(INDEXING_DISK_RESERVE_BYTES, 1));
        assert!(disk_headroom_is_sufficient(
            INDEXING_DISK_RESERVE_BYTES + INDEXING_WAL_DELTA_BYTES,
            INDEXING_WAL_DELTA_BYTES
        ));
        assert!(!disk_headroom_is_sufficient(u64::MAX - 1, u64::MAX));
        assert!(ensure_disk_headroom(Path::new("."), 0, "reclaiming cleanup").is_ok());
    }

    #[test]
    fn disk_probe_errors_fail_closed_for_growth_but_not_cleanup() {
        let failure = || Err(io::Error::other("probe unavailable"));
        assert!(matches!(
            ensure_disk_headroom_with_probe(
                Path::new("work.sqlite"),
                INDEXING_WAL_DELTA_BYTES,
                "amplifying work",
                failure,
            ),
            Err(StoreError::DiskSpaceProbeFailed {
                operation: "amplifying work",
                ..
            })
        ));
        assert!(ensure_disk_headroom_with_probe(
            Path::new("work.sqlite"),
            0,
            "reclaiming cleanup",
            || Err(io::Error::other("probe unavailable")),
        )
        .is_ok());
    }

    #[test]
    fn sqlite_amplification_uses_all_current_physical_files_and_saturates() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("work.sqlite");
        fs::write(&path, vec![0_u8; 11]).unwrap();
        fs::write(sqlite_wal_path(&path), vec![0_u8; 13]).unwrap();
        fs::write(sqlite_journal_path(&path), vec![0_u8; 17]).unwrap();
        fs::write(sqlite_shm_path(&path), vec![0_u8; 19]).unwrap();

        assert_eq!(sqlite_amplifying_write_estimate(&path, 3, 0).unwrap(), 180);
        assert_eq!(
            sqlite_amplifying_write_estimate(&path, u64::MAX, 0).unwrap(),
            u64::MAX
        );
    }

    #[test]
    fn source_io_pacing_uses_existing_slice_bounds_and_missing_class_is_quiet() {
        let quiet = IndexingIoPacer {
            quiet: true,
            rate_reservation_path: None,
        };
        let foreground = IndexingIoPacer {
            quiet: false,
            rate_reservation_path: None,
        };
        let started = Instant::now();

        assert!(!quiet
            .source_io_slice_should_rotate(started, INDEXING_WAL_DELTA_BYTES.saturating_sub(1)));
        assert!(quiet.source_io_slice_should_rotate(started, INDEXING_WAL_DELTA_BYTES));
        assert!(foreground.source_io_slice_should_rotate(started, INDEXING_WAL_DELTA_BYTES));

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("work.sqlite");
        let store = Store::open(&path).unwrap();
        assert!(!store.indexing_io_pacer().quiet);
        drop(store);
        let read_only = Store::open_read_only(path).unwrap();
        assert!(read_only.indexing_io_pacer().quiet);
    }

    #[test]
    fn quiet_rest_uses_the_stronger_duty_or_bandwidth_limit() {
        assert_eq!(
            quiet_rest(Duration::from_millis(100), Some(1024 * 1024)),
            Duration::from_millis(300)
        );
        assert_eq!(
            quiet_rest(Duration::from_millis(10), Some(INDEXING_WAL_DELTA_BYTES)),
            Duration::from_millis(990)
        );
        assert_eq!(
            source_io_rest(Duration::from_millis(10), INDEXING_WAL_DELTA_BYTES, false),
            Duration::from_millis(990)
        );
    }

    #[test]
    fn slice_finish_paces_aggregate_physical_writes_without_alias_double_counting() {
        let page_size = 4_096;
        let frames = INDEXING_WAL_DELTA_BYTES / page_size;
        let measured = physical_write_bytes(
            Some(INDEXING_WAL_DELTA_BYTES),
            Some(INDEXING_WAL_DELTA_BYTES),
            Some(page_size),
            frames,
        );
        assert_eq!(measured, Some(INDEXING_WAL_DELTA_BYTES.saturating_mul(2)));
        assert_eq!(
            quiet_rest(Duration::from_millis(10), measured),
            Duration::from_millis(1_990)
        );
        assert_eq!(
            physical_write_bytes(Some(u64::MAX), Some(1), Some(u64::MAX), u64::MAX),
            Some(u64::MAX)
        );
        assert_eq!(
            checkpoint_write_bytes(Some(page_size), frames),
            Some(INDEXING_WAL_DELTA_BYTES)
        );
        assert_eq!(checkpoint_write_bytes(Some(page_size), 0), None);
    }

    #[test]
    fn cache_write_counter_paces_reused_same_sized_wal() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        store
            .conn
            .execute_batch(
                "PRAGMA wal_autocheckpoint = 0;
                 CREATE TABLE wal_reuse (id INTEGER PRIMARY KEY, payload BLOB NOT NULL);
                 BEGIN IMMEDIATE",
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO wal_reuse (payload) VALUES (?1)",
                rusqlite::params![vec![1_u8; 1024 * 1024]],
            )
            .unwrap();
        store.conn.execute_batch("COMMIT").unwrap();
        store
            .conn
            .execute_batch("PRAGMA wal_checkpoint(RESTART)")
            .unwrap();

        let slice = store.begin_indexing_slice().unwrap();
        store.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        store
            .conn
            .execute(
                "UPDATE wal_reuse SET payload = ?1 WHERE id = 1",
                rusqlite::params![vec![2_u8; 1024 * 1024]],
            )
            .unwrap();
        store.conn.execute_batch("COMMIT").unwrap();

        assert_eq!(store.wal_bytes().unwrap(), slice.wal_bytes);
        let measured_bytes = store
            .indexing_slice_write_bytes(&slice)
            .unwrap()
            .expect("cache writes should remain measurable across WAL reuse");
        assert!(measured_bytes >= 1024 * 1024, "{measured_bytes}");
        assert!(quiet_rest(Duration::ZERO, Some(measured_bytes)) > Duration::ZERO);
    }

    #[test]
    fn background_rest_can_be_limited_by_a_deadline_budget() {
        assert_eq!(
            background_rest_with_limit(
                Duration::from_millis(100),
                None,
                Some(Duration::from_millis(25)),
            ),
            Duration::from_millis(25)
        );
    }

    #[test]
    fn background_handoff_admits_waiting_foreground_before_reacquiring() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let background =
            IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
        let active_background = background.lease().unwrap();

        let queued_background = background.clone();
        let (background_acquired_tx, background_acquired_rx) = mpsc::channel();
        let (release_background_tx, release_background_rx) = mpsc::channel();
        let background_thread = thread::spawn(move || {
            let _lease = queued_background.lease().unwrap();
            background_acquired_tx.send(()).unwrap();
            release_background_rx.recv().unwrap();
        });

        let (foreground_acquired_tx, foreground_acquired_rx) = mpsc::channel();
        let (release_foreground_tx, release_foreground_rx) = mpsc::channel();
        let foreground_path = db_path.clone();
        let foreground = thread::spawn(move || {
            let admission =
                IndexingAdmission::acquire(&foreground_path, IndexingWorkClass::Foreground)
                    .unwrap();
            let _lease = admission.lease().unwrap();
            foreground_acquired_tx.send(()).unwrap();
            release_foreground_rx.recv().unwrap();
        });

        let pending_deadline = Instant::now() + Duration::from_secs(2);
        while !background.foreground_pending() && Instant::now() < pending_deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(background.foreground_pending());
        drop(active_background);

        foreground_acquired_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("foreground should acquire at the background slice boundary");
        assert!(background_acquired_rx.try_recv().is_err());
        release_foreground_tx.send(()).unwrap();
        foreground.join().unwrap();
        background_acquired_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("queued background should resume after foreground");
        release_background_tx.send(()).unwrap();
        background_thread.join().unwrap();
    }

    #[test]
    fn slow_inventory_preparation_without_a_lease_does_not_delay_foreground() {
        assert_slow_preparation_does_not_delay_foreground();
    }

    #[test]
    fn slow_vector_preparation_without_a_lease_does_not_delay_foreground() {
        assert_slow_preparation_does_not_delay_foreground();
    }

    fn assert_slow_preparation_does_not_delay_foreground() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let background =
            IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
        let preparation = thread::spawn(move || {
            let _descriptor = background;
            thread::sleep(Duration::from_millis(250));
        });

        let foreground =
            IndexingAdmission::acquire(&db_path, IndexingWorkClass::Foreground).unwrap();
        let started = Instant::now();
        let lease = foreground.lease().unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
        drop(lease);
        preparation.join().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn meminfo_parser_is_strict() {
        assert_eq!(
            meminfo_kib("MemTotal: 1024 kB", "MemTotal:"),
            Some(1024 * 1024)
        );
        assert_eq!(meminfo_kib("MemTotal: 1024 MB", "MemTotal:"), None);
    }
}
