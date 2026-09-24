// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
use super::*;
use std::ffi::OsStr;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

pub(super) fn resolve_program(program: &OsStr) -> OsString {
    use std::path::Path;

    // Command finds native executables, but not extensionless batch shims.
    // Resolve only directly runnable Windows formats, in PATH/PATHEXT order;
    // script associations (e.g. .ps1) still require an explicit interpreter.
    // Hand the path and untouched argv to std, including its batch escaping.
    let Ok(cwd) = std::env::current_dir() else {
        return program.to_owned();
    };
    let path = Path::new(program);
    if path.file_name().is_none() {
        return program.to_owned();
    }
    let executable_extension = path.extension().is_some_and(|extension| {
        ["com", "exe", "bat", "cmd"].iter().any(|supported| {
            extension
                .as_encoded_bytes()
                .eq_ignore_ascii_case(supported.as_bytes())
        })
    });
    let pathext =
        std::env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
    let pathext = pathext.to_string_lossy();
    let extensions: Vec<_> = pathext
        .split(';')
        .filter(|ext| {
            [".com", ".exe", ".bat", ".cmd"]
                .iter()
                .any(|supported| ext.eq_ignore_ascii_case(supported))
        })
        .collect();
    let mut directories = vec![cwd.clone()];
    if path.components().count() == 1
        && let Some(search) = std::env::var_os("PATH")
    {
        directories.extend(std::env::split_paths(&search).map(|dir| cwd.join(dir)));
    }
    for directory in directories {
        let base = directory.join(path);
        if path.extension().is_some() && base.is_file() {
            return base.into_os_string();
        }
        if executable_extension {
            continue;
        }
        for extension in &extensions {
            let mut candidate = base.clone().into_os_string();
            candidate.push(extension);
            if Path::new(&candidate).is_file() {
                return candidate;
            }
        }
    }
    // Preserve std's native executable lookup and spawn error classification.
    program.to_owned()
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static INTERRUPTED: AtomicU32 = AtomicU32::new(0);

unsafe extern "system" fn console_event(event: u32) -> i32 {
    if event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT {
        INTERRUPTED.store(2, Ordering::Relaxed);
        1
    } else {
        // Keep Windows' default close/logoff/shutdown action. The kernel
        // closes our noninherited job handle even on forced termination.
        0
    }
}

pub(super) struct State {
    job: OwnedHandle,
}
impl State {
    pub(super) fn new() -> io::Result<Self> {
        if ACTIVE
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(io::Error::other("a command is already running"));
        }
        INTERRUPTED.store(0, Ordering::Relaxed);
        let result = (|| {
            // Null security attributes make the job handle noninheritable.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: CreateJobObjectW returned an owned, valid handle.
            let job = unsafe { OwnedHandle::from_raw_handle(handle) };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: limits is correctly sized and initialized; handle is live.
            if unsafe {
                SetInformationJobObject(
                    job.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if unsafe { SetConsoleCtrlHandler(Some(console_event), 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { job })
        })();
        if result.is_err() {
            ACTIVE.store(false, Ordering::Release);
        }
        result
    }

    pub(super) fn preserve_descendants(&self) -> io::Result<()> {
        // This private job has only KILL_ON_JOB_CLOSE set. Clear it only
        // after an uncancelled zero exit and successful output delivery.
        let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: the live job handle and initialized native structure remain
        // valid for the call. Failure leaves the caller's cleanup guard armed.
        if unsafe {
            SetInformationJobObject(
                self.job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn terminate(&self) {
        // SAFETY: the state owns this live job handle.
        unsafe {
            TerminateJobObject(self.job.as_raw_handle(), 130);
        }
    }

    pub(super) fn assign_and_resume(&self, child: &Child) -> io::Result<()> {
        // std::Command retains argv quoting, cwd/env and stream inheritance.
        // CREATE_SUSPENDED prevents child code (and grandchildren) running
        // before assignment. Errors leave Process's guard to kill/reap it.
        // SAFETY: both handles are owned and remain live through the call.
        if unsafe { AssignProcessToJobObject(self.job.as_raw_handle(), child.as_raw_handle()) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // std drops the initial thread handle. Its documented ToolHelp ID
        // lets us reopen it; no custom native command-line builder is needed.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the checked snapshot handle is newly owned.
        let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        // SAFETY: entry is initialized to the API's native structure size.
        let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
        while found != 0 {
            if entry.th32OwnerProcessID == child.id() {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: OpenThread returned a new live handle; the initial
                // child thread remains suspended until ResumeThread succeeds.
                let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
                if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                return Ok(());
            }
            // SAFETY: the snapshot and writable entry remain live.
            found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
        }
        Err(io::Error::other("cannot find suspended command thread"))
    }

    pub(super) fn check_cancelled(&self, cancelled: &mut Option<(i32, Instant)>) {
        if INTERRUPTED.load(Ordering::Relaxed) != 0 {
            // Shared-console children already receive Ctrl+C/Break from the
            // OS. Rebroadcasting it would also interrupt Sift's caller.
            cancelled.get_or_insert((2, Instant::now()));
        }
        if cancelled.is_some_and(|(_, time)| time.elapsed() >= Duration::from_secs(1)) {
            self.terminate();
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        unsafe {
            SetConsoleCtrlHandler(Some(console_event), 0);
        }
        ACTIVE.store(false, Ordering::Release);
        // OwnedHandle closes the job. Cleanup remains armed unless a
        // successful invocation explicitly preserved background descendants.
    }
}

impl Process {
    pub(super) fn write_windows(&mut self, stderr: bool, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write;
        use windows_sys::Win32::Storage::FileSystem::WriteFile;
        use windows_sys::Win32::System::Console::{
            GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        };
        let bytes = bytes.to_vec();
        let emitted_bytes = self.emitted_bytes[usize::from(stderr)].clone();
        self.windows_io(move || {
            let mut remaining = bytes.as_slice();
            while !remaining.is_empty() {
                let n = if (stderr && io::stderr().is_terminal())
                    || (!stderr && io::stdout().is_terminal())
                {
                    // Capture can explicitly target a terminal. Retain Rust's
                    // Unicode console conversion on either output stream.
                    if stderr {
                        io::stderr().write(remaining)?
                    } else {
                        let n = io::stdout().write(remaining)?;
                        io::stdout().flush()?;
                        n
                    }
                } else {
                    let handle = unsafe {
                        GetStdHandle(if stderr {
                            STD_ERROR_HANDLE
                        } else {
                            STD_OUTPUT_HANDLE
                        })
                    };
                    let mut written = 0;
                    // SAFETY: a borrowed standard handle and a live byte slice;
                    // synchronous WriteFile initializes the count before returning.
                    // Avoid Stdout's line buffering/retries so cancellation of a
                    // blocked pipe write cannot be swallowed as an interrupted I/O.
                    if unsafe {
                        WriteFile(
                            handle,
                            remaining.as_ptr(),
                            remaining.len().min(64 * 1024) as u32,
                            &mut written,
                            std::ptr::null_mut(),
                        )
                    } == 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                    written as usize
                };
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "output write returned zero",
                    ));
                }
                emitted_bytes.fetch_add(n as u64, Ordering::Relaxed);
                remaining = &remaining[n..];
            }
            Ok(())
        })
    }

    fn windows_io(
        &mut self,
        operation: impl FnOnce() -> io::Result<()> + Send + 'static,
    ) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::IO::CancelSynchronousIo;

        // Synchronous pipe writes can block. Windows reports a closed consumer
        // on the next write; quiet children are not polled for pipe closure.
        // Supervise them off-thread; repeat cancellation to cover the race just
        // before the worker enters the system call.
        let (tx, rx) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let _ = tx.send(operation());
        });
        let mut cancelled = false;
        let result = loop {
            self.windows.check_cancelled(&mut self.cancelled);
            if self
                .cancelled
                .is_some_and(|(_, time)| time.elapsed() >= Duration::from_secs(1))
            {
                cancelled = true;
                // SAFETY: the join handle owns this live worker thread's handle.
                unsafe {
                    CancelSynchronousIo(worker.as_raw_handle());
                }
            }
            match rx.recv_timeout(TICK) {
                Ok(result) => break result,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    break Err(io::Error::other("output worker stopped"));
                }
            }
        };
        let _ = worker.join();
        if cancelled {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "command cancelled",
            ))
        } else {
            result
        }
    }
}
