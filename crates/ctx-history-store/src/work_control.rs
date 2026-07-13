use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;

use crate::object_store::restrict_private_file;
use crate::{Result, Store, StoreError};

pub const INDEXING_WAL_DELTA_BYTES: u64 = 8 * 1024 * 1024;
pub const WAL_PASSIVE_MIN_BYTES: u64 = 8 * 1024 * 1024;
pub const WAL_RESTART_MIN_BYTES: u64 = 32 * 1024 * 1024;
pub const WAL_TRUNCATE_MIN_BYTES: u64 = 64 * 1024 * 1024;

// A background writer checks for foreground demand after every unit and never
// intentionally keeps one transaction open beyond this admission handoff SLO.
pub const INDEXING_TRANSACTION_MAX: Duration = Duration::from_millis(250);

const WRITER_LOCK_SUFFIX: &str = ".indexing-writer.lock";
const FOREGROUND_LOCK_SUFFIX: &str = ".indexing-foreground.lock";

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
    pub process_rss_bytes: Option<u64>,
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
            process_rss_bytes: process_rss_bytes(),
            available_disk_bytes,
            wal_bytes,
        }
    }

    pub fn pressure(self) -> IndexingPressure {
        let memory = match (self.available_memory_bytes, self.process_rss_bytes) {
            (Some(available), Some(rss)) if available < rss => IndexingPressure::Constrained,
            (Some(_), Some(_)) => IndexingPressure::Normal,
            _ => IndexingPressure::Unknown,
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
}

#[derive(Clone)]
pub struct IndexingAdmission {
    inner: Arc<Mutex<IndexingAdmissionState>>,
}

impl std::fmt::Debug for IndexingAdmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IndexingAdmission")
            .field("class", &self.work_class())
            .finish_non_exhaustive()
    }
}

struct IndexingAdmissionState {
    class: IndexingWorkClass,
    writer: File,
    foreground: File,
    writer_locked: bool,
    foreground_locked: bool,
}

impl Drop for IndexingAdmissionState {
    fn drop(&mut self) {
        if self.writer_locked {
            let _ = FileExt::unlock(&self.writer);
        }
        if self.foreground_locked {
            let _ = FileExt::unlock(&self.foreground);
        }
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
        let writer = open_lock_file(&lock_path(store_path, WRITER_LOCK_SUFFIX))?;
        let foreground = open_lock_file(&lock_path(store_path, FOREGROUND_LOCK_SUFFIX))?;
        let mut state = IndexingAdmissionState {
            class,
            writer,
            foreground,
            writer_locked: false,
            foreground_locked: false,
        };
        match class {
            IndexingWorkClass::Foreground => {
                FileExt::lock_exclusive(&state.foreground)?;
                state.foreground_locked = true;
                FileExt::lock_exclusive(&state.writer)?;
                state.writer_locked = true;
            }
            IndexingWorkClass::Background => acquire_background_lane(&mut state)?,
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(state)),
        })
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
        self.state().class
    }

    pub fn foreground_pending(&self) -> bool {
        let state = self.state();
        if state.class == IndexingWorkClass::Foreground {
            return false;
        }
        match FileExt::try_lock_shared(&state.foreground) {
            Ok(()) => {
                let _ = FileExt::unlock(&state.foreground);
                false
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => true,
            Err(_) => true,
        }
    }

    fn yield_background(&self, active: Duration, max_rest: Option<Duration>) -> Result<()> {
        let mut state = self.state();
        if state.class != IndexingWorkClass::Background {
            return Ok(());
        }
        if state.writer_locked {
            FileExt::unlock(&state.writer)?;
            state.writer_locked = false;
        }
        drop(state);

        let rest = background_rest_with_limit(active, max_rest);
        if !rest.is_zero() {
            thread::sleep(rest);
        }

        let mut state = self.state();
        acquire_background_lane(&mut state)
    }

    fn state(&self) -> MutexGuard<'_, IndexingAdmissionState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Store {
    pub fn begin_indexing_slice(&self) -> Result<IndexingSlice> {
        Ok(IndexingSlice {
            started: Instant::now(),
            wal_bytes: self.wal_bytes()?,
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
        self.checkpoint_wal_for_pressure()?;
        if let Some(admission) = &self.indexing_admission {
            admission.yield_background(slice.started.elapsed(), None)?;
        }
        Ok(())
    }

    pub fn yield_indexing_admission(&self, active: Duration) -> Result<()> {
        if let Some(admission) = &self.indexing_admission {
            admission.yield_background(active, None)?;
        }
        Ok(())
    }

    pub fn yield_indexing_admission_with_budget(
        &self,
        active: Duration,
        remaining: Option<Duration>,
    ) -> Result<()> {
        if let Some(admission) = &self.indexing_admission {
            admission.yield_background(active, remaining)?;
        }
        Ok(())
    }

    pub fn indexing_work_class(&self) -> Option<IndexingWorkClass> {
        self.indexing_admission
            .as_ref()
            .map(IndexingAdmission::work_class)
    }
}

fn acquire_background_lane(state: &mut IndexingAdmissionState) -> Result<()> {
    FileExt::lock_shared(&state.foreground)?;
    if let Err(error) = FileExt::lock_exclusive(&state.writer) {
        let _ = FileExt::unlock(&state.foreground);
        return Err(error.into());
    }
    state.writer_locked = true;
    if let Err(error) = FileExt::unlock(&state.foreground) {
        let _ = FileExt::unlock(&state.writer);
        state.writer_locked = false;
        return Err(error.into());
    }
    Ok(())
}

fn background_rest(active: Duration) -> Duration {
    Duration::from_nanos(
        active
            .as_nanos()
            .saturating_mul(3)
            .min(u128::from(u64::MAX)) as u64,
    )
}

fn background_rest_with_limit(active: Duration, max_rest: Option<Duration>) -> Duration {
    max_rest
        .map(|limit| background_rest(active).min(limit))
        .unwrap_or_else(|| background_rest(active))
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

#[cfg(target_os = "linux")]
fn process_rss_bytes() -> Option<u64> {
    let resident_pages = fs::read_to_string("/proc/self/statm")
        .ok()?
        .split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()?;
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    (page_size > 0).then(|| resident_pages.saturating_mul(page_size as u64))
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn process_rss_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    let rss = unsafe { usage.assume_init() }.ru_maxrss;
    let rss = u64::try_from(rss).ok()?;
    #[cfg(target_os = "macos")]
    return Some(rss);
    #[cfg(target_os = "freebsd")]
    return Some(rss.saturating_mul(1024));
}

#[cfg(target_os = "windows")]
fn process_rss_bytes() -> Option<u64> {
    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
    }
    #[link(name = "psapi")]
    extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut std::ffi::c_void,
            counters: *mut ProcessMemoryCounters,
            size: u32,
        ) -> i32;
    }
    let mut counters = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
    };
    let result = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            std::mem::size_of::<ProcessMemoryCounters>() as u32,
        )
    };
    (result != 0).then(|| counters.working_set_size as u64)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "windows"
)))]
fn process_rss_bytes() -> Option<u64> {
    None
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
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn resource_policy_is_conservative_for_missing_and_pressure_signals() {
        assert_eq!(
            IndexingResourceSnapshot::default().pressure(),
            IndexingPressure::Unknown
        );
        assert_eq!(
            IndexingResourceSnapshot {
                available_memory_bytes: Some(99),
                process_rss_bytes: Some(100),
                available_disk_bytes: Some(WAL_TRUNCATE_MIN_BYTES * 4),
                wal_bytes: Some(0),
            }
            .pressure(),
            IndexingPressure::Constrained
        );
        assert_eq!(
            IndexingResourceSnapshot {
                available_memory_bytes: Some(200),
                process_rss_bytes: Some(100),
                available_disk_bytes: Some(WAL_TRUNCATE_MIN_BYTES * 4),
                wal_bytes: Some(WAL_PASSIVE_MIN_BYTES),
            }
            .pressure(),
            IndexingPressure::Normal
        );
    }

    #[test]
    fn background_rest_preserves_quiet_twenty_five_percent_duty_cycle() {
        assert_eq!(
            background_rest(Duration::from_millis(100)),
            Duration::from_millis(300)
        );
    }

    #[test]
    fn background_rest_can_be_limited_by_a_deadline_budget() {
        assert_eq!(
            background_rest_with_limit(Duration::from_millis(100), Some(Duration::from_millis(25)),),
            Duration::from_millis(25)
        );
    }

    #[test]
    fn background_handoff_admits_waiting_foreground_before_reacquiring() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let background =
            IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
        let (foreground_acquired_tx, foreground_acquired_rx) = mpsc::channel();
        let (release_foreground_tx, release_foreground_rx) = mpsc::channel();
        let foreground_path = db_path.clone();
        let foreground = thread::spawn(move || {
            let _admission =
                IndexingAdmission::acquire(&foreground_path, IndexingWorkClass::Foreground)
                    .unwrap();
            foreground_acquired_tx.send(()).unwrap();
            release_foreground_rx.recv().unwrap();
        });

        let pending_deadline = Instant::now() + Duration::from_secs(2);
        while !background.foreground_pending() && Instant::now() < pending_deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(background.foreground_pending());

        let yielding_background = background.clone();
        let (background_reacquired_tx, background_reacquired_rx) = mpsc::channel();
        let background_handoff = thread::spawn(move || {
            yielding_background
                .yield_background(Duration::from_millis(1), None)
                .unwrap();
            background_reacquired_tx.send(()).unwrap();
        });

        foreground_acquired_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("foreground should acquire at the background slice boundary");
        assert!(background_reacquired_rx.try_recv().is_err());
        release_foreground_tx.send(()).unwrap();
        foreground.join().unwrap();
        background_handoff.join().unwrap();
        background_reacquired_rx.recv().unwrap();
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
