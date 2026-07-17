use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use fs2::FileExt;

use crate::{CaptureError, Result};

const SCRATCH_ROOT_NAME: &str = "ctx-history-capture-scratch-v1";
const MANAGER_LOCK_NAME: &str = ".manager.lock";
const NEXT_RUN_ID_NAME: &str = ".next-run-id";
const SWEEP_STATE_NAME: &str = ".sweep-state";
const LEASE_NAME: &str = "lease";
const OWNER_NAME: &str = "owner";
const MAX_SCAVENGE_RUNS: usize = 4;
const SCRATCH_DELETE_FILES_PER_PAGE: usize = 64;
const SCRATCH_DELETE_ROW_OVERHEAD_BYTES: u64 = 4 * 1024;
const SCRATCH_CLEANUP_MAX_PAGES: usize = 4;
const SCRATCH_CLEANUP_MAX_OPERATIONS: u64 = 8 * 1024;
const SCRATCH_CLEANUP_MAX_BYTES: u64 = 64 * 1024 * 1024;
const SCRATCH_CLEANUP_MAX_ELAPSED: Duration = Duration::from_millis(50);
const SCRATCH_CLEANUP_PAGE_OPERATIONS: u64 = 32;
const SCRATCH_CLEANUP_ENTRY_OPERATIONS: u64 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScratchCleanupOutcome {
    Complete,
    Pending,
    Busy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScratchCleanupPageOutcome {
    Complete,
    Progress,
    Pending,
}

struct ScratchCleanupBudget {
    started: Instant,
    pages: usize,
    operations: u64,
    bytes: u64,
    max_bytes: u64,
}

impl ScratchCleanupBudget {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            pages: 0,
            operations: 0,
            bytes: 0,
            max_bytes: SCRATCH_CLEANUP_MAX_BYTES,
        }
    }

    #[cfg(test)]
    fn with_max_bytes_for_test(max_bytes: u64) -> Self {
        Self {
            started: Instant::now(),
            pages: 0,
            operations: 0,
            bytes: 0,
            max_bytes,
        }
    }

    fn begin_page(&mut self) -> bool {
        if self.pages >= SCRATCH_CLEANUP_MAX_PAGES
            || self
                .operations
                .saturating_add(SCRATCH_CLEANUP_PAGE_OPERATIONS)
                > SCRATCH_CLEANUP_MAX_OPERATIONS
            || self.started.elapsed() >= SCRATCH_CLEANUP_MAX_ELAPSED
        {
            return false;
        }
        self.pages += 1;
        self.operations = self
            .operations
            .saturating_add(SCRATCH_CLEANUP_PAGE_OPERATIONS);
        true
    }

    fn remaining_bytes(&self) -> u64 {
        self.max_bytes.saturating_sub(self.bytes)
    }

    fn reserve_entry(&mut self, bytes: u64) -> bool {
        if self
            .operations
            .saturating_add(SCRATCH_CLEANUP_ENTRY_OPERATIONS)
            > SCRATCH_CLEANUP_MAX_OPERATIONS
            || self.bytes.saturating_add(bytes) > self.max_bytes
            || self.started.elapsed() >= SCRATCH_CLEANUP_MAX_ELAPSED
        {
            return false;
        }
        self.operations = self
            .operations
            .saturating_add(SCRATCH_CLEANUP_ENTRY_OPERATIONS);
        self.bytes = self.bytes.saturating_add(bytes);
        true
    }
}

pub(crate) struct CaptureScratchSpace {
    root: PathBuf,
    path: PathBuf,
    lease: Option<File>,
}

impl CaptureScratchSpace {
    pub(crate) fn create(kind: &'static str) -> Result<Self> {
        Self::create_at_root(default_scratch_root(), kind)
    }

    #[cfg(test)]
    pub(crate) fn create_in(root: PathBuf, kind: &'static str) -> Result<Self> {
        Self::create_at_root(root, kind)
    }

    fn create_at_root(root: PathBuf, kind: &'static str) -> Result<Self> {
        validate_kind(kind)?;
        ensure_private_directory(&root)?;
        let _manager_lock = acquire_manager_lock(&root)?;
        let run_id = allocate_run_id(&root)?;
        scavenge_abandoned_runs(&root, run_id)?;

        let path = run_path(&root, run_id);
        create_private_directory(&path)?;
        let lease = create_private_file(&path.join(LEASE_NAME))?;
        pace_filesystem_path(&path.join(LEASE_NAME));
        FileExt::lock_exclusive(&lease)?;
        let mut owner = create_private_file(&path.join(OWNER_NAME))?;
        crate::pace_current_disk_io(128);
        writeln!(owner, "pid={}", std::process::id())?;
        writeln!(owner, "run_id={run_id}")?;
        writeln!(owner, "kind={kind}")?;
        owner.sync_all()?;

        Ok(Self {
            root,
            path,
            lease: Some(lease),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn create_file(&self, name: &str) -> Result<File> {
        validate_file_name(name)?;
        let path = self.path.join(name);
        Ok(create_private_file(&path)?)
    }

    fn cleanup(&mut self) {
        let Ok(_manager_lock) = acquire_manager_lock(&self.root) else {
            return;
        };
        if let Some(lease) = self.lease.take() {
            let mut budget = ScratchCleanupBudget::new();
            let _ = cleanup_owned_scratch_run(&self.path, lease, &mut budget);
        }
    }
}

impl Drop for CaptureScratchSpace {
    fn drop(&mut self) {
        self.cleanup();
    }
}

struct ManagerLock {
    file: File,
}

impl Drop for ManagerLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn default_scratch_root() -> PathBuf {
    #[cfg(unix)]
    {
        let uid = unsafe { libc::geteuid() };
        std::env::temp_dir().join(format!("{SCRATCH_ROOT_NAME}-{uid}"))
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir().join(SCRATCH_ROOT_NAME)
    }
}

fn validate_kind(kind: &str) -> Result<()> {
    if kind.is_empty()
        || !kind
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(CaptureError::SystemInvariant(
            "capture scratch kind must be lowercase ASCII",
        ));
    }
    Ok(())
}

fn validate_file_name(name: &str) -> Result<()> {
    let path = Path::new(name);
    if name.is_empty()
        || path.file_name() != Some(OsStr::new(name))
        || path.components().count() != 1
    {
        return Err(CaptureError::InvalidPayload(
            "capture scratch file name must be one path component".to_owned(),
        ));
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> io::Result<()> {
    match create_private_directory(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            validate_private_directory(path)
        }
        Err(error) => Err(error),
    }
}

fn acquire_manager_lock(root: &Path) -> io::Result<ManagerLock> {
    let path = root.join(MANAGER_LOCK_NAME);
    let file = match create_private_file(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            open_private_regular_file(&path)?
        }
        Err(error) => return Err(error),
    };
    FileExt::lock_exclusive(&file)?;
    Ok(ManagerLock { file })
}

fn run_path(root: &Path, run_id: u64) -> PathBuf {
    root.join(format!("run-{run_id:020}"))
}

fn open_or_create_private_control_file(root: &Path, name: &str) -> io::Result<File> {
    let path = root.join(name);
    match create_private_file(&path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            open_private_regular_file(&path)
        }
        Err(error) => Err(error),
    }
}

fn read_control_file(file: &mut File) -> io::Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut contents = String::new();
    file.take(256).read_to_string(&mut contents)?;
    Ok(contents)
}

fn write_control_file(file: &mut File, contents: &str) -> io::Result<()> {
    crate::pace_current_disk_io(contents.len() as u64);
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()
}

fn allocate_run_id(root: &Path) -> io::Result<u64> {
    let mut file = open_or_create_private_control_file(root, NEXT_RUN_ID_NAME)?;
    let contents = read_control_file(&mut file)?;
    let next = if contents.is_empty() {
        0
    } else {
        contents.trim().parse::<u64>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "capture scratch next-run ID is corrupt",
            )
        })?
    };
    let following = next
        .checked_add(1)
        .ok_or_else(|| io::Error::other("capture scratch run ID space is exhausted"))?;
    write_control_file(&mut file, &format!("{following}\n"))?;
    Ok(next)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SweepState {
    cursor: u64,
    highwater: u64,
}

fn read_sweep_state(file: &mut File, next_run_id: u64) -> io::Result<SweepState> {
    let contents = read_control_file(file)?;
    if contents.is_empty() {
        return Ok(SweepState {
            cursor: 0,
            highwater: next_run_id,
        });
    }
    let mut lines = contents.lines();
    let cursor = lines
        .next()
        .and_then(|line| line.strip_prefix("cursor="))
        .and_then(|value| value.parse::<u64>().ok());
    let highwater = lines
        .next()
        .and_then(|line| line.strip_prefix("highwater="))
        .and_then(|value| value.parse::<u64>().ok());
    if lines.next().is_some() || cursor.is_none() || highwater.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "capture scratch sweep state is corrupt",
        ));
    }
    let state = SweepState {
        cursor: cursor.unwrap(),
        highwater: highwater.unwrap(),
    };
    if state.cursor > state.highwater || state.highwater > next_run_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "capture scratch sweep state is outside the allocated run range",
        ));
    }
    Ok(state)
}

fn write_sweep_state(file: &mut File, state: SweepState) -> io::Result<()> {
    write_control_file(
        file,
        &format!("cursor={}\nhighwater={}\n", state.cursor, state.highwater),
    )
}

fn scavenge_abandoned_runs(root: &Path, current_run_id: u64) -> io::Result<()> {
    scavenge_abandoned_runs_with_budget(root, current_run_id, ScratchCleanupBudget::new())
}

fn scavenge_abandoned_runs_with_budget(
    root: &Path,
    current_run_id: u64,
    mut cleanup_budget: ScratchCleanupBudget,
) -> io::Result<()> {
    let next_run_id = current_run_id
        .checked_add(1)
        .ok_or_else(|| io::Error::other("capture scratch run ID space is exhausted"))?;
    let mut state_file = open_or_create_private_control_file(root, SWEEP_STATE_NAME)?;
    let mut state = read_sweep_state(&mut state_file, next_run_id)?;
    if state.cursor == state.highwater {
        state = SweepState {
            cursor: 0,
            highwater: next_run_id,
        };
    }

    let mut inspected = 0usize;
    while inspected < MAX_SCAVENGE_RUNS && state.cursor < state.highwater {
        let run_id = state.cursor;
        if run_id == current_run_id {
            state.cursor += 1;
            inspected += 1;
            continue;
        }
        let path = run_path(root, run_id);
        match cleanup_abandoned_scratch_run(&path, &mut cleanup_budget) {
            Ok(ScratchCleanupOutcome::Pending) => break,
            Ok(ScratchCleanupOutcome::Complete | ScratchCleanupOutcome::Busy) => {
                state.cursor += 1;
                inspected += 1;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                state.cursor += 1;
                inspected += 1;
            }
            Err(error) => return Err(error),
        }
    }
    write_sweep_state(&mut state_file, state)
}

#[cfg(unix)]
fn cleanup_owned_scratch_run(
    path: &Path,
    lease: File,
    budget: &mut ScratchCleanupBudget,
) -> io::Result<ScratchCleanupOutcome> {
    let run = UnixScratchRun::open(path)?;
    run.validate_held_lease(&lease)?;
    let removal =
        remove_anchored_scratch_run(&run, path, UnixScratchRunLocation::Canonical, budget);
    let unlock = FileExt::unlock(&lease);
    let outcome = removal?;
    unlock?;
    Ok(outcome)
}

#[cfg(windows)]
fn cleanup_owned_scratch_run(
    path: &Path,
    lease: File,
    budget: &mut ScratchCleanupBudget,
) -> io::Result<ScratchCleanupOutcome> {
    let run = WindowsScratchRun::open(path)?;
    run.validate_held_lease(path, &lease)?;
    remove_anchored_scratch_run(&run, path, Some(lease), budget)
}

#[cfg(not(any(unix, windows)))]
fn cleanup_owned_scratch_run(
    _path: &Path,
    _lease: File,
    _budget: &mut ScratchCleanupBudget,
) -> io::Result<ScratchCleanupOutcome> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private capture scratch is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn cleanup_abandoned_scratch_run(
    path: &Path,
    budget: &mut ScratchCleanupBudget,
) -> io::Result<ScratchCleanupOutcome> {
    let quarantine_path = scratch_quarantine_path(path)?;
    let recovered_quarantine = match cleanup_abandoned_unix_scratch_run(
        &quarantine_path,
        UnixScratchRunLocation::Quarantined,
        budget,
    ) {
        Ok(ScratchCleanupOutcome::Complete) => true,
        Ok(ScratchCleanupOutcome::Pending) => return Ok(ScratchCleanupOutcome::Pending),
        Ok(ScratchCleanupOutcome::Busy) => return Ok(ScratchCleanupOutcome::Busy),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    match cleanup_abandoned_unix_scratch_run(path, UnixScratchRunLocation::Canonical, budget) {
        Err(error) if recovered_quarantine && error.kind() == io::ErrorKind::NotFound => {
            Ok(ScratchCleanupOutcome::Complete)
        }
        outcome => outcome,
    }
}

#[cfg(unix)]
fn cleanup_abandoned_unix_scratch_run(
    path: &Path,
    location: UnixScratchRunLocation,
    budget: &mut ScratchCleanupBudget,
) -> io::Result<ScratchCleanupOutcome> {
    let run = UnixScratchRun::open(path)?;
    let Some(lease) = run.open_lease()? else {
        return remove_anchored_scratch_run(&run, path, location, budget);
    };
    match FileExt::try_lock_exclusive(&lease) {
        Ok(()) => {
            let removal = remove_anchored_scratch_run(&run, path, location, budget);
            let unlock = FileExt::unlock(&lease);
            let outcome = removal?;
            unlock?;
            Ok(outcome)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(ScratchCleanupOutcome::Busy),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn cleanup_abandoned_scratch_run(
    path: &Path,
    budget: &mut ScratchCleanupBudget,
) -> io::Result<ScratchCleanupOutcome> {
    let run = WindowsScratchRun::open(path)?;
    let Some(lease) = run.open_lease(path)? else {
        return remove_anchored_scratch_run(&run, path, None, budget);
    };
    match FileExt::try_lock_exclusive(&lease) {
        Ok(()) => remove_anchored_scratch_run(&run, path, Some(lease), budget),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(ScratchCleanupOutcome::Busy),
        Err(error) => Err(error),
    }
}

#[cfg(not(any(unix, windows)))]
fn cleanup_abandoned_scratch_run(
    _path: &Path,
    _budget: &mut ScratchCleanupBudget,
) -> io::Result<ScratchCleanupOutcome> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private capture scratch is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn remove_anchored_scratch_run(
    run: &UnixScratchRun,
    path: &Path,
    location: UnixScratchRunLocation,
    budget: &mut ScratchCleanupBudget,
) -> io::Result<ScratchCleanupOutcome> {
    loop {
        if !budget.begin_page() {
            return Ok(ScratchCleanupOutcome::Pending);
        }
        match run.remove_page(path, location, budget)? {
            ScratchCleanupPageOutcome::Complete => return Ok(ScratchCleanupOutcome::Complete),
            ScratchCleanupPageOutcome::Pending => return Ok(ScratchCleanupOutcome::Pending),
            ScratchCleanupPageOutcome::Progress => {}
        }
        std::thread::yield_now();
    }
}

#[cfg(windows)]
fn remove_anchored_scratch_run(
    run: &WindowsScratchRun,
    path: &Path,
    lease: Option<File>,
    budget: &mut ScratchCleanupBudget,
) -> io::Result<ScratchCleanupOutcome> {
    loop {
        if !budget.begin_page() {
            return Ok(ScratchCleanupOutcome::Pending);
        }
        match run.remove_non_lease_page(path, budget)? {
            ScratchCleanupPageOutcome::Complete => break,
            ScratchCleanupPageOutcome::Pending => return Ok(ScratchCleanupOutcome::Pending),
            ScratchCleanupPageOutcome::Progress => {}
        }
        std::thread::yield_now();
    }
    run.finalize(path, lease)?;
    Ok(ScratchCleanupOutcome::Complete)
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnixScratchRunLocation {
    Canonical,
    Quarantined,
}

#[cfg(any(unix, windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScratchDirectoryIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    volume_serial: u64,
    #[cfg(windows)]
    file_id: [u8; 16],
}

#[cfg(unix)]
struct UnixScratchRun {
    parent: File,
    directory: File,
    name: std::ffi::CString,
    identity: ScratchDirectoryIdentity,
}

#[cfg(unix)]
impl UnixScratchRun {
    fn open(path: &Path) -> io::Result<Self> {
        use std::os::unix::{ffi::OsStrExt, io::AsRawFd, io::FromRawFd};

        let parent_path = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch run has no parent directory",
            )
        })?;
        let name = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch run has no directory name",
            )
        })?;
        let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch run name contains an invalid byte",
            )
        })?;
        let parent = open_existing_directory_no_follow(parent_path)?;
        validate_private_directory_handle(&parent)?;
        pace_filesystem_path(path);
        let descriptor = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let directory = unsafe { File::from_raw_fd(descriptor) };
        validate_private_directory_handle(&directory)?;
        let identity = scratch_handle_identity(&directory)?;
        Ok(Self {
            parent,
            directory,
            name,
            identity,
        })
    }

    fn open_lease(&self) -> io::Result<Option<File>> {
        match self.open_relative_file(LEASE_NAME) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn validate_held_lease(&self, lease: &File) -> io::Result<()> {
        let anchored = self.open_lease()?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch lease disappeared before cleanup",
            )
        })?;
        if scratch_handle_identity(&anchored)? != scratch_handle_identity(lease)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch lease changed identity before cleanup",
            ));
        }
        Ok(())
    }

    fn open_relative_file(&self, name: &str) -> io::Result<File> {
        use std::os::unix::{io::AsRawFd, io::FromRawFd};

        let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch file name contains an invalid byte",
            )
        })?;
        crate::pace_current_filesystem_operation(name.as_bytes().len() as u64);
        let descriptor = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        validate_private_file_handle(&file)?;
        Ok(file)
    }

    fn remove_page(
        &self,
        path: &Path,
        location: UnixScratchRunLocation,
        budget: &mut ScratchCleanupBudget,
    ) -> io::Result<ScratchCleanupPageOutcome> {
        use std::os::unix::{ffi::OsStrExt, io::FromRawFd, io::IntoRawFd};

        self.revalidate(path)?;
        let page_directory = self.open_current_directory(path)?;
        let page_descriptor = page_directory.into_raw_fd();
        let stream = unsafe { libc::fdopendir(page_descriptor) };
        if stream.is_null() {
            let error = io::Error::last_os_error();
            unsafe {
                libc::close(page_descriptor);
            }
            return Err(error);
        }
        let stream = UnixDirectoryStream(stream);
        let mut removed = 0usize;
        let mut exhausted = false;
        while removed < SCRATCH_DELETE_FILES_PER_PAGE {
            pace_filesystem_path(path);
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                exhausted = true;
                break;
            }
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            let entry_path = path.join(std::ffi::OsStr::from_bytes(name.to_bytes()));
            pace_filesystem_path(&entry_path);
            let mut metadata = std::mem::MaybeUninit::<libc::stat>::zeroed();
            let status = unsafe {
                libc::fstatat(
                    page_descriptor,
                    name.as_ptr(),
                    metadata.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if status != 0 {
                return Err(io::Error::last_os_error());
            }
            let metadata = unsafe { metadata.assume_init() };
            if metadata.st_mode & libc::S_IFMT != libc::S_IFREG
                || metadata.st_uid != unsafe { libc::geteuid() }
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "capture scratch run contains a link or non-file entry",
                ));
            }
            let file_bytes = u64::try_from(metadata.st_size).unwrap_or(0);
            let required_bytes = file_bytes.saturating_add(SCRATCH_DELETE_ROW_OVERHEAD_BYTES);
            if required_bytes > budget.remaining_bytes() {
                let truncate_bytes = file_bytes.min(
                    budget
                        .remaining_bytes()
                        .saturating_sub(SCRATCH_DELETE_ROW_OVERHEAD_BYTES),
                );
                if truncate_bytes == 0
                    || !budget.reserve_entry(
                        truncate_bytes.saturating_add(SCRATCH_DELETE_ROW_OVERHEAD_BYTES),
                    )
                {
                    return Ok(ScratchCleanupPageOutcome::Pending);
                }
                crate::pace_current_disk_io(
                    truncate_bytes.saturating_add(SCRATCH_DELETE_ROW_OVERHEAD_BYTES),
                );
                let descriptor = unsafe {
                    libc::openat(
                        page_descriptor,
                        name.as_ptr(),
                        libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    )
                };
                if descriptor < 0 {
                    return Err(io::Error::last_os_error());
                }
                let file = unsafe { File::from_raw_fd(descriptor) };
                validate_private_file_handle(&file)?;
                let expected = ScratchDirectoryIdentity {
                    device: u64::try_from(metadata.st_dev).unwrap_or(u64::MAX),
                    inode: u64::try_from(metadata.st_ino).unwrap_or(u64::MAX),
                };
                if scratch_handle_identity(&file)? != expected {
                    return Err(scratch_directory_changed_error());
                }
                file.set_len(file_bytes.saturating_sub(truncate_bytes))?;
                return Ok(ScratchCleanupPageOutcome::Pending);
            }
            if !budget.reserve_entry(required_bytes) {
                return Ok(ScratchCleanupPageOutcome::Pending);
            }
            crate::pace_current_disk_io(required_bytes);
            pace_filesystem_path(&entry_path);
            if unsafe { libc::unlinkat(page_descriptor, name.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            removed += 1;
        }
        drop(stream);
        if removed > 0 && !exhausted {
            self.revalidate(path)?;
            return Ok(ScratchCleanupPageOutcome::Progress);
        }
        let complete = match location {
            UnixScratchRunLocation::Canonical => self.finalize_canonical(path),
            UnixScratchRunLocation::Quarantined => self.finalize_quarantined(path, &self.name),
        }?;
        Ok(if complete {
            ScratchCleanupPageOutcome::Complete
        } else {
            ScratchCleanupPageOutcome::Pending
        })
    }

    fn revalidate(&self, path: &Path) -> io::Result<()> {
        use std::os::unix::io::AsRawFd;

        pace_filesystem_path(path);
        if scratch_handle_identity(&self.directory)? != self.identity {
            return Err(scratch_directory_changed_error());
        }
        pace_filesystem_path(path);
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::zeroed();
        let status = unsafe {
            libc::fstatat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if status != 0 {
            return Err(io::Error::last_os_error());
        }
        let metadata = unsafe { metadata.assume_init() };
        let current = ScratchDirectoryIdentity {
            device: u64::try_from(metadata.st_dev).unwrap_or(u64::MAX),
            inode: u64::try_from(metadata.st_ino).unwrap_or(u64::MAX),
        };
        if metadata.st_mode & libc::S_IFMT != libc::S_IFDIR
            || metadata.st_uid != unsafe { libc::geteuid() }
            || current != self.identity
        {
            return Err(scratch_directory_changed_error());
        }
        Ok(())
    }

    fn open_current_directory(&self, path: &Path) -> io::Result<File> {
        use std::os::unix::{io::AsRawFd, io::FromRawFd};

        pace_filesystem_path(path);
        let current = b".\0";
        let descriptor = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                current.as_ptr().cast(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let directory = unsafe { File::from_raw_fd(descriptor) };
        validate_private_directory_handle(&directory)?;
        if scratch_handle_identity(&directory)? != self.identity {
            return Err(scratch_directory_changed_error());
        }
        Ok(directory)
    }

    fn finalize_canonical(&self, path: &Path) -> io::Result<bool> {
        #[cfg(target_os = "freebsd")]
        {
            return self.finalize_quarantined(path, &self.name);
        }

        #[cfg(not(target_os = "freebsd"))]
        {
            use std::os::unix::io::AsRawFd;

            self.revalidate(path)?;
            // A deterministic quarantine keeps interrupted cleanup reachable by the run-ID sweep.
            let quarantine_path = scratch_quarantine_path(path)?;
            let quarantine = path_component_cstring(&quarantine_path)?;
            pace_filesystem_path(&quarantine_path);
            rename_scratch_entry_no_replace(self.parent.as_raw_fd(), &self.name, &quarantine)?;
            let finalization = (|| {
                maybe_fail_unix_scratch_finalization(
                    UnixScratchFinalizationFailurePoint::AfterRename,
                    path,
                )?;
                self.finalize_quarantined(&quarantine_path, &quarantine)
            })();
            let Err(finalization_error) = finalization else {
                return Ok(true);
            };
            match self.directory_link_count() {
                Ok(0) => return Err(finalization_error),
                Ok(_) => {}
                Err(identity_error) => {
                    return Err(io::Error::other(format!(
                    "capture scratch finalization failed ({finalization_error}); quarantine retained at {} because anchored identity could not be revalidated ({identity_error})",
                    quarantine_path.display()
                )));
                }
            }
            if let Err(restore_error) = self.restore_quarantine(path, &quarantine) {
                return Err(io::Error::other(format!(
                "capture scratch finalization failed ({finalization_error}); quarantine retained at {} for bounded cleanup ({restore_error})",
                quarantine_path.display()
            )));
            }
            Err(finalization_error)
        }
    }

    fn finalize_quarantined(
        &self,
        canonical_path: &Path,
        quarantine: &std::ffi::CStr,
    ) -> io::Result<bool> {
        if self.identity_at(quarantine)? != self.identity {
            return Err(scratch_directory_changed_error());
        }
        maybe_fail_unix_scratch_finalization(
            UnixScratchFinalizationFailurePoint::AfterFirstIdentityCheck,
            canonical_path,
        )?;
        if self.identity_at(quarantine)? != self.identity {
            return Err(scratch_directory_changed_error());
        }
        maybe_fail_unix_scratch_finalization(
            UnixScratchFinalizationFailurePoint::AfterSecondIdentityCheck,
            canonical_path,
        )?;
        maybe_fail_unix_scratch_finalization(
            UnixScratchFinalizationFailurePoint::BeforeUnlink,
            canonical_path,
        )?;

        #[cfg(target_os = "freebsd")]
        {
            self.unlink_anchored_directory(canonical_path, quarantine)?;
            maybe_fail_unix_scratch_finalization(
                UnixScratchFinalizationFailurePoint::AfterUnlink,
                canonical_path,
            )?;
            if self.directory_link_count()? != 0 {
                return Err(io::Error::other(
                    "capture scratch finalization did not unlink the anchored run",
                ));
            }
        }
        #[cfg(not(target_os = "freebsd"))]
        {
            use std::os::unix::io::AsRawFd;

            // The scratch root is same-user 0700 state. We reject links and revalidate the
            // anchored empty directory immediately before this checked name-based rmdir. A
            // malicious same-UID process racing names inside that private root is outside the
            // isolation boundary; the link-count postcondition still detects a lost identity.
            pace_filesystem_path(canonical_path);
            if self.identity_at(quarantine)? != self.identity {
                return Err(scratch_directory_changed_error());
            }
            if unsafe {
                libc::unlinkat(
                    self.parent.as_raw_fd(),
                    quarantine.as_ptr(),
                    libc::AT_REMOVEDIR,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            if self.directory_link_count()? != 0 {
                return Err(io::Error::other(
                    "capture scratch finalization removed an unexpected directory identity",
                ));
            }
        }
        Ok(true)
    }

    #[cfg(target_os = "freebsd")]
    fn unlink_anchored_directory(&self, path: &Path, name: &std::ffi::CStr) -> io::Result<()> {
        use std::os::unix::io::AsRawFd;

        pace_filesystem_path(path);
        let status = freebsd_unlink_anchored_directory(
            self.parent.as_raw_fd(),
            name,
            self.directory.as_raw_fd(),
        )?;
        if status != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(not(target_os = "freebsd"))]
    fn restore_quarantine(
        &self,
        canonical_path: &Path,
        quarantine: &std::ffi::CStr,
    ) -> io::Result<()> {
        use std::os::unix::io::AsRawFd;

        if self.identity_at(quarantine)? != self.identity {
            return Err(scratch_directory_changed_error());
        }
        rename_scratch_entry_no_replace(self.parent.as_raw_fd(), quarantine, &self.name)?;
        self.revalidate(canonical_path)
    }

    fn directory_link_count(&self) -> io::Result<u64> {
        use std::os::unix::fs::MetadataExt;

        crate::pace_current_filesystem_operation(0);
        Ok(self.directory.metadata()?.nlink())
    }

    fn identity_at(&self, name: &std::ffi::CStr) -> io::Result<ScratchDirectoryIdentity> {
        use std::os::unix::io::AsRawFd;

        crate::pace_current_filesystem_operation(name.to_bytes().len() as u64);
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::zeroed();
        if unsafe {
            libc::fstatat(
                self.parent.as_raw_fd(),
                name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let metadata = unsafe { metadata.assume_init() };
        if metadata.st_mode & libc::S_IFMT != libc::S_IFDIR
            || metadata.st_uid != unsafe { libc::geteuid() }
        {
            return Err(scratch_directory_changed_error());
        }
        Ok(ScratchDirectoryIdentity {
            device: u64::try_from(metadata.st_dev).unwrap_or(u64::MAX),
            inode: u64::try_from(metadata.st_ino).unwrap_or(u64::MAX),
        })
    }
}

#[cfg(unix)]
struct UnixDirectoryStream(*mut libc::DIR);

#[cfg(unix)]
impl Drop for UnixDirectoryStream {
    fn drop(&mut self) {
        unsafe {
            libc::closedir(self.0);
        }
    }
}

#[cfg(all(unix, not(target_os = "freebsd")))]
fn rename_scratch_entry_no_replace(
    parent: std::os::unix::io::RawFd,
    source: &std::ffi::CStr,
    destination: &std::ffi::CStr,
) -> io::Result<()> {
    crate::pace_current_filesystem_operations(
        2,
        source
            .to_bytes()
            .len()
            .saturating_add(destination.to_bytes().len()) as u64,
    );

    #[cfg(target_os = "linux")]
    let status = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            parent,
            source.as_ptr(),
            parent,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        ) as libc::c_int
    };
    #[cfg(target_os = "android")]
    let status = unsafe {
        libc::renameat2(
            parent,
            source.as_ptr(),
            parent,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let status = unsafe {
        libc::renameatx_np(
            parent,
            source.as_ptr(),
            parent,
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    {
        if status != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace scratch cleanup is unsupported on this Unix platform",
        ))
    }
}

#[cfg(target_os = "freebsd")]
fn freebsd_unlink_anchored_directory(
    parent: std::os::unix::io::RawFd,
    name: &std::ffi::CStr,
    directory: std::os::unix::io::RawFd,
) -> io::Result<libc::c_int> {
    // funlinkat appeared in FreeBSD 13. Dynamic lookup keeps older binaries loadable and fail-closed.
    type FunlinkAt = unsafe extern "C" fn(
        libc::c_int,
        *const libc::c_char,
        libc::c_int,
        libc::c_int,
    ) -> libc::c_int;

    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"funlinkat".as_ptr()) };
    if symbol.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "identity-conditional scratch cleanup requires FreeBSD 13 or newer",
        ));
    }
    let funlinkat = unsafe { std::mem::transmute::<*mut libc::c_void, FunlinkAt>(symbol) };
    Ok(unsafe { funlinkat(parent, name.as_ptr(), directory, libc::AT_REMOVEDIR) })
}

#[cfg(unix)]
fn scratch_quarantine_path(path: &Path) -> io::Result<PathBuf> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch run has no directory name",
        )
    })?;
    let mut quarantine = b".ctx-cleanup-".to_vec();
    quarantine.extend_from_slice(name.as_bytes());
    Ok(path.with_file_name(std::ffi::OsString::from_vec(quarantine)))
}

#[cfg(all(unix, not(target_os = "freebsd")))]
fn path_component_cstring(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;

    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch path has no directory name",
        )
    })?;
    std::ffi::CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch path contains an invalid byte",
        )
    })
}

#[cfg(windows)]
struct WindowsScratchRun {
    directory: File,
    identity: ScratchDirectoryIdentity,
}

#[cfg(windows)]
impl WindowsScratchRun {
    fn open(path: &Path) -> io::Result<Self> {
        let directory = open_existing_directory_for_cleanup(path)?;
        validate_private_directory_handle(&directory)?;
        let identity = scratch_handle_identity(&directory)?;
        Ok(Self {
            directory,
            identity,
        })
    }

    fn open_lease(&self, path: &Path) -> io::Result<Option<File>> {
        self.revalidate(path)?;
        let lease_path = path.join(LEASE_NAME);
        let lease = match open_private_regular_file(&lease_path) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        self.revalidate(path)?;
        Ok(lease)
    }

    fn validate_held_lease(&self, path: &Path, lease: &File) -> io::Result<()> {
        let anchored = self.open_lease(path)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch lease disappeared before cleanup",
            )
        })?;
        if scratch_handle_identity(&anchored)? != scratch_handle_identity(lease)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch lease changed identity before cleanup",
            ));
        }
        Ok(())
    }

    fn open_lease_for_cleanup(&self, path: &Path, lease: &File) -> io::Result<File> {
        self.revalidate(path)?;
        let cleanup = open_existing_file_for_cleanup(&path.join(LEASE_NAME))?;
        validate_private_file_handle(&cleanup)?;
        if scratch_handle_identity(&cleanup)? != scratch_handle_identity(lease)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch lease changed identity before finalization",
            ));
        }
        self.revalidate(path)?;
        Ok(cleanup)
    }

    fn remove_non_lease_page(
        &self,
        path: &Path,
        budget: &mut ScratchCleanupBudget,
    ) -> io::Result<ScratchCleanupPageOutcome> {
        self.revalidate(path)?;
        pace_filesystem_path(path);
        let mut entries = fs::read_dir(path)?;
        let mut removed = 0usize;
        let mut exhausted = false;
        while removed < SCRATCH_DELETE_FILES_PER_PAGE {
            pace_filesystem_path(path);
            let Some(entry) = entries.next() else {
                exhausted = true;
                break;
            };
            let entry = entry?;
            if entry.file_name() == OsStr::new(LEASE_NAME) {
                continue;
            }
            let entry_path = entry.path();
            self.revalidate(path)?;
            maybe_attempt_windows_scratch_child_aba(path)?;
            let file = open_existing_file_for_cleanup(&entry_path)?;
            validate_private_file_handle(&file)?;
            self.revalidate(path)?;
            pace_filesystem_path(&entry_path);
            let metadata = file.metadata()?;
            let required_bytes = metadata
                .len()
                .saturating_add(SCRATCH_DELETE_ROW_OVERHEAD_BYTES);
            if required_bytes > budget.remaining_bytes() {
                let truncate_bytes = metadata.len().min(
                    budget
                        .remaining_bytes()
                        .saturating_sub(SCRATCH_DELETE_ROW_OVERHEAD_BYTES),
                );
                if truncate_bytes == 0
                    || !budget.reserve_entry(
                        truncate_bytes.saturating_add(SCRATCH_DELETE_ROW_OVERHEAD_BYTES),
                    )
                {
                    return Ok(ScratchCleanupPageOutcome::Pending);
                }
                let truncate_file = open_existing_file_no_follow(&entry_path)?;
                validate_private_file_handle(&truncate_file)?;
                if scratch_handle_identity(&truncate_file)? != scratch_handle_identity(&file)? {
                    return Err(scratch_directory_changed_error());
                }
                self.revalidate(path)?;
                crate::pace_current_disk_io(
                    truncate_bytes.saturating_add(SCRATCH_DELETE_ROW_OVERHEAD_BYTES),
                );
                truncate_file.set_len(metadata.len().saturating_sub(truncate_bytes))?;
                return Ok(ScratchCleanupPageOutcome::Pending);
            }
            if !budget.reserve_entry(required_bytes) {
                return Ok(ScratchCleanupPageOutcome::Pending);
            }
            crate::pace_current_disk_io(required_bytes);
            pace_filesystem_path(&entry_path);
            delete_windows_handle(&file)?;
            drop(file);
            removed += 1;
        }
        drop(entries);
        if removed > 0 && !exhausted {
            self.revalidate(path)?;
            return Ok(ScratchCleanupPageOutcome::Progress);
        }
        Ok(ScratchCleanupPageOutcome::Complete)
    }

    fn finalize(&self, path: &Path, lease: Option<File>) -> io::Result<()> {
        self.revalidate(path)?;
        if let Some(lease) = lease {
            // Owner leases lack DELETE access, so validate a second handle before releasing it.
            let cleanup_lease = self.open_lease_for_cleanup(path, &lease)?;
            self.verify_children(path, true)?;

            FileExt::unlock(&lease)?;
            drop(lease);
            let lease_deletion = delete_windows_handle(&cleanup_lease);
            drop(cleanup_lease);
            lease_deletion?;
        }

        self.revalidate(path)?;
        self.verify_children(path, false)?;
        self.revalidate(path)?;
        pace_filesystem_path(path);
        delete_windows_handle(&self.directory)
    }

    fn verify_children(&self, path: &Path, expect_lease: bool) -> io::Result<()> {
        self.revalidate(path)?;
        pace_filesystem_path(path);
        let mut entries = fs::read_dir(path)?;
        let mut saw_lease = false;
        for _ in 0..2 {
            pace_filesystem_path(path);
            let Some(entry) = entries.next() else {
                self.revalidate(path)?;
                if expect_lease && !saw_lease {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "capture scratch lease disappeared during finalization",
                    ));
                }
                return Ok(());
            };
            let entry = entry?;
            if expect_lease && !saw_lease && entry.file_name() == OsStr::new(LEASE_NAME) {
                saw_lease = true;
                continue;
            }
            return Err(io::Error::new(
                io::ErrorKind::DirectoryNotEmpty,
                "capture scratch run changed while finalizing cleanup",
            ));
        }
        Err(io::Error::new(
            io::ErrorKind::DirectoryNotEmpty,
            "capture scratch run changed while finalizing cleanup",
        ))
    }

    fn revalidate(&self, path: &Path) -> io::Result<()> {
        let current = open_existing_directory_no_follow(path)?;
        validate_private_directory_handle(&current)?;
        if scratch_handle_identity(&self.directory)? != self.identity
            || scratch_handle_identity(&current)? != self.identity
        {
            return Err(scratch_directory_changed_error());
        }
        Ok(())
    }
}

#[cfg(any(unix, windows))]
fn scratch_directory_changed_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "capture scratch run changed identity during cleanup",
    )
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnixScratchFinalizationFailurePoint {
    #[cfg(not(target_os = "freebsd"))]
    AfterRename,
    AfterFirstIdentityCheck,
    AfterSecondIdentityCheck,
    BeforeUnlink,
    #[cfg(target_os = "freebsd")]
    AfterUnlink,
}

#[cfg(all(test, unix))]
struct UnixScratchFinalizationFailure {
    point: UnixScratchFinalizationFailurePoint,
    create_restore_collision: bool,
}

#[cfg(all(test, unix))]
thread_local! {
    static UNIX_SCRATCH_FINALIZATION_FAILURE_ONCE: std::cell::RefCell<Option<UnixScratchFinalizationFailure>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(all(test, unix))]
fn inject_unix_scratch_finalization_failure_once(
    point: UnixScratchFinalizationFailurePoint,
    create_restore_collision: bool,
) {
    UNIX_SCRATCH_FINALIZATION_FAILURE_ONCE.with(|slot| {
        *slot.borrow_mut() = Some(UnixScratchFinalizationFailure {
            point,
            create_restore_collision,
        });
    });
}

#[cfg(all(test, unix))]
fn maybe_fail_unix_scratch_finalization(
    point: UnixScratchFinalizationFailurePoint,
    canonical_path: &Path,
) -> io::Result<()> {
    let failure = UNIX_SCRATCH_FINALIZATION_FAILURE_ONCE.with(|slot| {
        if slot.borrow().as_ref().map(|failure| failure.point) == Some(point) {
            slot.borrow_mut().take()
        } else {
            None
        }
    });
    let Some(failure) = failure else {
        return Ok(());
    };
    if failure.create_restore_collision {
        create_private_directory(canonical_path)?;
    }
    Err(io::Error::other(format!(
        "injected capture scratch finalization failure at {point:?}"
    )))
}

#[cfg(all(unix, not(test)))]
fn maybe_fail_unix_scratch_finalization(
    _point: UnixScratchFinalizationFailurePoint,
    _canonical_path: &Path,
) -> io::Result<()> {
    Ok(())
}

#[cfg(all(test, windows))]
struct WindowsScratchChildAba {
    moved_path: PathBuf,
    replacement_path: PathBuf,
}

#[cfg(all(test, windows))]
thread_local! {
    static WINDOWS_SCRATCH_CHILD_ABA_ONCE: std::cell::RefCell<Option<WindowsScratchChildAba>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(all(test, windows))]
fn inject_windows_scratch_child_aba_once(moved_path: PathBuf, replacement_path: PathBuf) {
    WINDOWS_SCRATCH_CHILD_ABA_ONCE.with(|slot| {
        *slot.borrow_mut() = Some(WindowsScratchChildAba {
            moved_path,
            replacement_path,
        });
    });
}

#[cfg(all(test, windows))]
fn maybe_attempt_windows_scratch_child_aba(path: &Path) -> io::Result<()> {
    let aba = WINDOWS_SCRATCH_CHILD_ABA_ONCE.with(|slot| slot.borrow_mut().take());
    let Some(aba) = aba else {
        return Ok(());
    };
    fs::rename(path, &aba.moved_path)?;
    fs::rename(&aba.replacement_path, path)?;
    Err(io::Error::other(
        "Windows scratch directory ABA unexpectedly succeeded",
    ))
}

#[cfg(all(windows, not(test)))]
fn maybe_attempt_windows_scratch_child_aba(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn validate_private_directory(path: &Path) -> io::Result<()> {
    pace_filesystem_path(path);
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch path is not a private directory",
        ));
    }
    validate_private_owner(&metadata)?;
    validate_private_directory_permissions(&metadata)?;
    let directory = open_existing_directory_no_follow(path)?;
    validate_private_directory_handle(&directory)
}

fn open_private_regular_file(path: &Path) -> io::Result<File> {
    let file = open_existing_file_no_follow(path)?;
    validate_private_file_handle(&file)?;
    Ok(file)
}

fn validate_private_directory_handle(directory: &File) -> io::Result<()> {
    crate::pace_current_filesystem_operation(0);
    let metadata = directory.metadata()?;
    if !metadata.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch handle is not a directory",
        ));
    }
    validate_private_owner(&metadata)?;
    validate_private_directory_permissions(&metadata)?;
    #[cfg(windows)]
    validate_private_windows_handle(directory, true)?;
    Ok(())
}

fn validate_private_file_handle(file: &File) -> io::Result<()> {
    crate::pace_current_filesystem_operation(0);
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch lease is not a regular file",
        ));
    }
    validate_private_owner(&metadata)?;
    validate_private_file_permissions(&metadata)?;
    #[cfg(windows)]
    validate_private_windows_handle(file, false)?;
    Ok(())
}

#[cfg(unix)]
fn scratch_handle_identity(directory: &File) -> io::Result<ScratchDirectoryIdentity> {
    use std::os::unix::fs::MetadataExt;

    crate::pace_current_filesystem_operation(0);
    let metadata = directory.metadata()?;
    Ok(ScratchDirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn scratch_handle_identity(directory: &File) -> io::Result<ScratchDirectoryIdentity> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandleEx, FILE_ID_INFO,
    };

    crate::pace_current_filesystem_operation(0);
    let mut information = MaybeUninit::<FILE_ID_INFO>::zeroed();
    let ok = unsafe {
        GetFileInformationByHandleEx(
            directory.as_raw_handle() as _,
            FileIdInfo,
            information.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let information = unsafe { information.assume_init() };
    Ok(ScratchDirectoryIdentity {
        volume_serial: information.VolumeSerialNumber,
        file_id: information.FileId.Identifier,
    })
}

#[cfg(unix)]
fn open_existing_directory_no_follow(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    pace_filesystem_path(path);
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn open_existing_directory_no_follow(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
    };

    pace_filesystem_path(path);
    let mut options = fs::OpenOptions::new();
    options
        .access_mode(FILE_READ_ATTRIBUTES | READ_CONTROL)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    options.open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_existing_directory_no_follow(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private capture scratch is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn open_existing_directory_for_cleanup(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
        FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
    };

    pace_filesystem_path(path);
    let mut options = fs::OpenOptions::new();
    // Omitting FILE_SHARE_DELETE pins the directory name while child paths are opened.
    options
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    options.open(path)
}

#[cfg(windows)]
fn open_existing_file_for_cleanup(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
    };

    pace_filesystem_path(path);
    let mut options = fs::OpenOptions::new();
    options
        .access_mode(FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    options.open(path)
}

#[cfg(windows)]
fn delete_windows_handle(file: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
    };

    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as _,
            FileDispositionInfo,
            std::ptr::from_ref(&disposition).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    pace_filesystem_path(path);
    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(windows)]
fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::{mem, os::windows::ffi::OsStrExt};
    use windows_sys::Win32::{
        Foundation::LocalFree, Security::SECURITY_ATTRIBUTES, Storage::FileSystem::CreateDirectoryW,
    };

    pace_filesystem_path(path);
    let descriptor = private_windows_security_descriptor(true)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let created = unsafe { CreateDirectoryW(path.as_ptr(), &attributes) };
    let error = (created == 0).then(io::Error::last_os_error);
    unsafe {
        LocalFree(descriptor);
    }
    error.map_or(Ok(()), Err)
}

#[cfg(not(any(unix, windows)))]
fn create_private_directory(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private capture scratch is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    pace_filesystem_path(path);
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn create_private_file(path: &Path) -> io::Result<File> {
    use std::{mem, os::windows::ffi::OsStrExt, os::windows::io::FromRawHandle, ptr};
    use windows_sys::Win32::{
        Foundation::{LocalFree, INVALID_HANDLE_VALUE},
        Security::SECURITY_ATTRIBUTES,
        Storage::FileSystem::{
            CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE,
        },
    };

    pace_filesystem_path(path);
    let descriptor = private_windows_security_descriptor(false)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    let error = (handle == INVALID_HANDLE_VALUE).then(io::Error::last_os_error);
    unsafe {
        LocalFree(descriptor);
    }
    if let Some(error) = error {
        return Err(error);
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(not(any(unix, windows)))]
fn create_private_file(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private capture scratch is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn open_existing_file_no_follow(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    pace_filesystem_path(path);
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn open_existing_file_no_follow(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    pace_filesystem_path(path);
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

fn pace_filesystem_path(path: &Path) {
    crate::disk_io_pacing::pace_current_filesystem_operation(path.as_os_str().len() as u64);
}

#[cfg(not(any(unix, windows)))]
fn open_existing_file_no_follow(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private capture scratch is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn validate_private_owner(metadata: &fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch is not owned by the effective user",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_owner(_metadata: &fs::Metadata) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn validate_private_directory_permissions(metadata: &fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    if metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch directory permissions are not private",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_private_directory_permissions(metadata: &fs::Metadata) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch directory is a reparse point",
        ));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn validate_private_directory_permissions(_metadata: &fs::Metadata) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private capture scratch is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn validate_private_file_permissions(metadata: &fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    if metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch file permissions are not private",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_private_file_permissions(metadata: &fs::Metadata) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch file is a reparse point",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_private_windows_handle(file: &File, directory: bool) -> io::Result<()> {
    use std::{
        mem::MaybeUninit,
        os::windows::{fs::MetadataExt, io::AsRawHandle},
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, LocalFree, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HANDLE},
        Security::{
            AclSizeInformation,
            Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
            EqualSid, GetAclInformation, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
            GetTokenInformation, TokenUser, ACL, ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION,
            OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, TOKEN_QUERY,
            TOKEN_USER,
        },
        Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT,
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    let metadata = file.metadata()?;
    let expected_type = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    if !expected_type || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch handle has an unsafe type",
        ));
    }

    let mut owner: PSID = std::ptr::null_mut();
    let mut actual_dacl: *mut ACL = std::ptr::null_mut();
    let mut actual_descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut actual_dacl,
            std::ptr::null_mut(),
            &mut actual_descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }

    let expected_descriptor = match private_windows_security_descriptor(directory) {
        Ok(descriptor) => descriptor,
        Err(error) => {
            unsafe {
                LocalFree(actual_descriptor);
            }
            return Err(error);
        }
    };
    let comparison = (|| {
        if owner.is_null() || actual_dacl.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch has no owner or DACL",
            ));
        }

        let mut token: HANDLE = std::ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let owner_matches = (|| {
            let mut required = 0_u32;
            let first = unsafe {
                GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut required)
            };
            if first != 0
                || io::Error::last_os_error().raw_os_error()
                    != Some(ERROR_INSUFFICIENT_BUFFER as i32)
                || required == 0
            {
                return Err(io::Error::last_os_error());
            }
            let word_size = std::mem::size_of::<usize>();
            let word_count = (required as usize).div_ceil(word_size);
            let mut buffer = vec![0_usize; word_count];
            if unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.as_mut_ptr().cast(),
                    required,
                    &mut required,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
            Ok(unsafe { EqualSid(owner, token_user.User.Sid) } != 0)
        })();
        unsafe {
            CloseHandle(token);
        }
        if !owner_matches? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch is not owned by the current user",
            ));
        }

        let mut control = 0_u16;
        let mut revision = 0_u32;
        if unsafe { GetSecurityDescriptorControl(actual_descriptor, &mut control, &mut revision) }
            == 0
            || control & SE_DACL_PROTECTED == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch DACL is not protected",
            ));
        }

        let mut expected_present = 0;
        let mut expected_defaulted = 0;
        let mut expected_dacl: *mut ACL = std::ptr::null_mut();
        if unsafe {
            GetSecurityDescriptorDacl(
                expected_descriptor,
                &mut expected_present,
                &mut expected_dacl,
                &mut expected_defaulted,
            )
        } == 0
            || expected_present == 0
            || expected_dacl.is_null()
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch expected DACL is invalid",
            ));
        }

        fn acl_bytes(acl: *const ACL) -> io::Result<Vec<u8>> {
            let mut info = MaybeUninit::<ACL_SIZE_INFORMATION>::zeroed();
            if unsafe {
                GetAclInformation(
                    acl,
                    info.as_mut_ptr().cast(),
                    std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                    AclSizeInformation,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            let info = unsafe { info.assume_init() };
            Ok(unsafe {
                std::slice::from_raw_parts(acl.cast::<u8>(), info.AclBytesInUse as usize).to_vec()
            })
        }

        if acl_bytes(actual_dacl)? != acl_bytes(expected_dacl)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch DACL is not owner/System-only",
            ));
        }
        Ok(())
    })();
    unsafe {
        LocalFree(expected_descriptor);
        LocalFree(actual_descriptor);
    }
    comparison
}

#[cfg(not(any(unix, windows)))]
fn validate_private_file_permissions(_metadata: &fs::Metadata) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private capture scratch is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn private_windows_security_descriptor(
    directory: bool,
) -> io::Result<windows_sys::Win32::Security::PSECURITY_DESCRIPTOR> {
    use std::ptr;
    use windows_sys::Win32::Security::{
        Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, PSECURITY_DESCRIPTOR,
    };

    let sddl = if directory {
        "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)"
    } else {
        "D:P(A;;FA;;;OW)(A;;FA;;;SY)"
    };
    let sddl = sddl.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(descriptor)
}

#[cfg(test)]
include!("scratch/tests.rs");
