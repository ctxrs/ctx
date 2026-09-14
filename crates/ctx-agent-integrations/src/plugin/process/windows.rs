use std::{
    ffi::c_void,
    io,
    mem::size_of,
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
        process::CommandExt as _,
    },
    process::{Child, Command},
    ptr,
};

const CREATE_SUSPENDED: u32 = 0x0000_0004;
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
const TH32CS_SNAPTHREAD: u32 = 0x0000_0004;
const THREAD_SUSPEND_RESUME: u32 = 0x0000_0002;
const RESUME_FAILED: u32 = u32::MAX;

pub(super) fn configure(command: &mut Command) {
    command.creation_flags(CREATE_SUSPENDED);
}

pub(super) struct ProcessTree {
    job: OwnedHandle,
}

impl ProcessTree {
    pub(super) fn start(child: &Child) -> io::Result<Self> {
        // SAFETY: Null attributes/name request a private job with default security.
        let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a new owned HANDLE.
        let job = unsafe { OwnedHandle::from_raw_handle(job) };
        let mut limits = JobObjectExtendedLimitInformation::default();
        limits.basic_limit_information.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: The buffer has the exact layout and lifetime required by the API.
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                (&raw const limits).cast(),
                u32::try_from(size_of::<JobObjectExtendedLimitInformation>())
                    .expect("job information size fits in u32"),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: Both handles are valid and the child is still suspended.
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        resume_process_threads(child.id())?;
        Ok(Self { job })
    }

    pub(super) fn terminate(&self) -> io::Result<()> {
        // SAFETY: The owned handle remains valid for this call.
        if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

fn resume_process_threads(process_id: u32) -> io::Result<()> {
    // SAFETY: The flags and process ID follow the Toolhelp contract.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == invalid_handle_value() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateToolhelp32Snapshot returned a new owned HANDLE.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = ThreadEntry32 {
        size: u32::try_from(size_of::<ThreadEntry32>()).expect("thread entry size fits in u32"),
        ..ThreadEntry32::default()
    };
    let mut resumed = false;
    // SAFETY: entry has the documented size and remains writable during enumeration.
    let mut has_entry = unsafe { Thread32First(snapshot.as_raw_handle(), &raw mut entry) } != 0;
    while has_entry {
        if entry.owner_process_id == process_id {
            // SAFETY: The thread ID came from the active snapshot entry.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.thread_id) };
            if !thread.is_null() {
                // SAFETY: OpenThread returned a new owned HANDLE.
                let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
                // SAFETY: The handle grants THREAD_SUSPEND_RESUME.
                if unsafe { ResumeThread(thread.as_raw_handle()) } != RESUME_FAILED {
                    resumed = true;
                }
            }
        }
        // SAFETY: entry remains initialized with its documented size.
        has_entry = unsafe { Thread32Next(snapshot.as_raw_handle(), &raw mut entry) } != 0;
    }
    if resumed {
        Ok(())
    } else {
        Err(io::Error::other(
            "could not resume the contained manager process",
        ))
    }
}

const fn invalid_handle_value() -> RawHandle {
    (-1_isize) as RawHandle
}

#[repr(C)]
#[derive(Default)]
struct IoCounters {
    read_operation_count: u64,
    write_operation_count: u64,
    other_operation_count: u64,
    read_transfer_count: u64,
    write_transfer_count: u64,
    other_transfer_count: u64,
}

#[repr(C)]
#[derive(Default)]
struct JobObjectBasicLimitInformation {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: u32,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}

#[repr(C)]
#[derive(Default)]
struct JobObjectExtendedLimitInformation {
    basic_limit_information: JobObjectBasicLimitInformation,
    io_info: IoCounters,
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

#[repr(C)]
#[derive(Default)]
struct ThreadEntry32 {
    size: u32,
    usage_count: u32,
    thread_id: u32,
    owner_process_id: u32,
    base_priority: i32,
    priority_delta: i32,
    flags: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> RawHandle;
    fn SetInformationJobObject(
        job: RawHandle,
        information_class: i32,
        information: *const c_void,
        information_length: u32,
    ) -> i32;
    fn AssignProcessToJobObject(job: RawHandle, process: RawHandle) -> i32;
    fn TerminateJobObject(job: RawHandle, exit_code: u32) -> i32;
    fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> RawHandle;
    fn Thread32First(snapshot: RawHandle, entry: *mut ThreadEntry32) -> i32;
    fn Thread32Next(snapshot: RawHandle, entry: *mut ThreadEntry32) -> i32;
    fn OpenThread(desired_access: u32, inherit_handle: i32, thread_id: u32) -> RawHandle;
    fn ResumeThread(thread: RawHandle) -> u32;
}
