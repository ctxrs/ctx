use std::{
    io,
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    process::Child,
    ptr,
};

use windows_sys::Win32::{
    Foundation::INVALID_HANDLE_VALUE,
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
        },
        JobObjects::{AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject},
        Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
    },
};

pub(super) struct ProcessTree {
    job: OwnedHandle,
}

impl ProcessTree {
    pub(super) fn start(child: &Child) -> io::Result<Self> {
        // SAFETY: null attributes and name request a private job with default security.
        let raw_job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if raw_job.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a new owned handle after the null check.
        let job = unsafe { OwnedHandle::from_raw_handle(raw_job) };
        // SAFETY: both handles remain valid for the duration of this call.
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }

        let process_tree = Self { job };
        if let Err(err) = resume_process(child.id()) {
            process_tree.terminate();
            return Err(err);
        }
        Ok(process_tree)
    }

    pub(super) fn terminate(&self) {
        // SAFETY: this is the live private job handle owned by this ProcessTree.
        unsafe {
            TerminateJobObject(self.job.as_raw_handle(), 1);
        }
    }
}

fn resume_process(process_id: u32) -> io::Result<()> {
    // SAFETY: the snapshot call has no borrowed pointer arguments.
    let raw_snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw_snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateToolhelp32Snapshot returned a new owned handle after the check.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw_snapshot) };
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..THREADENTRY32::default()
    };
    // SAFETY: entry advertises its exact size and is writable for the call.
    if unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } == 0 {
        return Err(io::Error::last_os_error());
    }

    loop {
        if entry.th32OwnerProcessID == process_id {
            // SAFETY: the discovered thread ID belongs to the suspended child.
            let raw_thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if raw_thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: OpenThread returned a new owned handle after the null check.
            let thread = unsafe { OwnedHandle::from_raw_handle(raw_thread) };
            // SAFETY: the handle grants THREAD_SUSPEND_RESUME for this child thread.
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        // SAFETY: entry remains correctly sized and writable for each iteration.
        if unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "suspended ctx CLI thread was not found",
            ));
        }
    }
}
