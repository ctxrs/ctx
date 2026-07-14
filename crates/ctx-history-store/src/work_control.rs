use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;
use rusqlite::{ffi, Connection};

use crate::object_store::restrict_private_file;
use crate::{Result, Store, StoreError};

pub const INDEXING_WAL_DELTA_BYTES: u64 = 8 * 1024 * 1024;
pub const WAL_PASSIVE_MIN_BYTES: u64 = 8 * 1024 * 1024;
pub const WAL_RESTART_MIN_BYTES: u64 = 32 * 1024 * 1024;
pub const WAL_TRUNCATE_MIN_BYTES: u64 = 64 * 1024 * 1024;

// A background writer checks for foreground demand after every unit and never
// intentionally keeps one transaction open beyond this admission handoff SLO.
pub const INDEXING_TRANSACTION_MAX: Duration = Duration::from_millis(250);

// One full 8 MiB slice followed by the 3x quiet rest occupies one second. Use
// that existing invariant as the portable bandwidth ceiling instead of adding
// another tuning value.
const QUIET_INDEXING_BANDWIDTH_BYTES_PER_SEC: u64 = INDEXING_WAL_DELTA_BYTES;

const WRITER_LOCK_SUFFIX: &str = ".indexing-writer.lock";
const FOREGROUND_LOCK_SUFFIX: &str = ".indexing-foreground.lock";
// Keep retry wakeups well inside the 250 ms transaction handoff boundary.
const BACKGROUND_ADMISSION_MAX_BACKOFF: Duration = Duration::from_millis(25);

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexingIoPacer {
    quiet: bool,
}

impl Default for IndexingIoPacer {
    fn default() -> Self {
        Self { quiet: true }
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
        if !rest.is_zero() {
            thread::sleep(rest);
        }
    }
}

#[derive(Clone)]
pub struct IndexingAdmission {
    store_path: PathBuf,
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

impl Drop for IndexingWriterLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.writer);
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
        if let Some(parent) = store_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let admission = Self {
            store_path: store_path.to_path_buf(),
            class,
        };
        open_lock_file(&admission.writer_lock_path())?;
        open_lock_file(&admission.foreground_lock_path())?;
        Ok(admission)
    }

    pub fn status(store_path: &Path) -> Result<IndexingAdmissionStatus> {
        let Some(writer) = open_existing_lock_file(&lock_path(store_path, WRITER_LOCK_SUFFIX))?
        else {
            return Ok(IndexingAdmissionStatus::default());
        };
        let foreground = open_existing_lock_file(&lock_path(store_path, FOREGROUND_LOCK_SUFFIX))?;
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
                loop {
                    if let Some(lease) = self.try_background_lease()? {
                        return Ok(lease);
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
        let writer = open_lock_file(&self.writer_lock_path())?;
        FileExt::lock_exclusive(&foreground)?;
        if let Err(error) = FileExt::lock_exclusive(&writer) {
            let _ = FileExt::unlock(&foreground);
            return Err(error.into());
        }
        FileExt::unlock(&foreground)?;
        Ok(IndexingWriterLease { writer })
    }

    fn try_foreground_lease(&self) -> Result<Option<IndexingWriterLease>> {
        let foreground = open_lock_file(&self.foreground_lock_path())?;
        let writer = open_lock_file(&self.writer_lock_path())?;
        if !try_lock_exclusive(&foreground)? {
            return Ok(None);
        }
        let writer_locked = try_lock_exclusive(&writer)?;
        FileExt::unlock(&foreground)?;
        Ok(writer_locked.then_some(IndexingWriterLease { writer }))
    }

    fn try_background_lease(&self) -> Result<Option<IndexingWriterLease>> {
        let foreground = open_lock_file(&self.foreground_lock_path())?;
        let writer = open_lock_file(&self.writer_lock_path())?;
        if !try_lock_shared(&foreground)? {
            return Ok(None);
        }
        let writer_locked = try_lock_exclusive(&writer)?;
        FileExt::unlock(&foreground)?;
        Ok(writer_locked.then_some(IndexingWriterLease { writer }))
    }

    fn writer_lock_path(&self) -> PathBuf {
        lock_path(&self.store_path, WRITER_LOCK_SUFFIX)
    }

    fn foreground_lock_path(&self) -> PathBuf {
        lock_path(&self.store_path, FOREGROUND_LOCK_SUFFIX)
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
        if !rest.is_zero() {
            thread::sleep(rest);
        }
    }
}

impl Store {
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
        let measured_bytes = self.indexing_slice_write_bytes(&slice)?;
        if nonblocking_checkpoint {
            let _ = self.try_checkpoint_wal_for_pressure()?;
        } else {
            self.checkpoint_wal_for_pressure()?;
        }
        if let Some(admission) = &self.indexing_admission {
            admission.rest_background(slice.started.elapsed(), measured_bytes, None);
        }
        Ok(())
    }

    fn indexing_slice_write_bytes(&self, slice: &IndexingSlice) -> Result<Option<u64>> {
        let wal_delta = monotonic_delta(slice.wal_bytes, self.wal_bytes()?);
        let cache_write_bytes =
            monotonic_delta(slice.cache_writes, sqlite_cache_write_count(&self.conn))
                .zip(slice.page_size_bytes)
                .map(|(pages, page_size)| pages.saturating_mul(page_size));
        Ok(max_optional(wal_delta, cache_write_bytes))
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

    #[doc(hidden)]
    pub fn indexing_io_pacer(&self) -> IndexingIoPacer {
        IndexingIoPacer {
            quiet: self.indexing_work_class() != Some(IndexingWorkClass::Foreground),
        }
    }
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

fn sqlite_page_size(conn: &Connection) -> Option<u64> {
    conn.query_row("PRAGMA page_size", [], |row| row.get::<_, u64>(0))
        .ok()
        .filter(|size| *size > 0)
}

fn lock_path(store_path: &Path, suffix: &str) -> PathBuf {
    let mut path = store_path.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn open_lock_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
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
    fn source_io_pacing_uses_existing_slice_bounds_and_missing_class_is_quiet() {
        let quiet = IndexingIoPacer { quiet: true };
        let foreground = IndexingIoPacer { quiet: false };
        let started = Instant::now();

        assert!(!quiet
            .source_io_slice_should_rotate(started, INDEXING_WAL_DELTA_BYTES.saturating_sub(1)));
        assert!(quiet.source_io_slice_should_rotate(started, INDEXING_WAL_DELTA_BYTES));
        assert!(foreground.source_io_slice_should_rotate(started, INDEXING_WAL_DELTA_BYTES));

        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        assert!(store.indexing_io_pacer().quiet);
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
