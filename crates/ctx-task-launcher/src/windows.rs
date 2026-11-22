use std::{
    env, io,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    ptr,
};
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE, WAIT_OBJECT_0},
    System::{
        JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
        },
        Threading::{
            CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
            InitializeProcThreadAttributeList, UpdateProcThreadAttribute, WaitForSingleObject,
            CREATE_NO_WINDOW, EXTENDED_STARTUPINFO_PRESENT, INFINITE, PROCESS_INFORMATION,
            PROC_THREAD_ATTRIBUTE_JOB_LIST, STARTUPINFOEXW,
        },
    },
};

pub(super) fn run() -> io::Result<u32> {
    let mut args = env::args_os().skip(1);
    let invalid = || io::Error::from(io::ErrorKind::InvalidInput);
    let program = args.next().ok_or_else(invalid)?;
    let script = args.next().ok_or_else(invalid)?;
    let script = script.to_str().ok_or_else(invalid)?;
    if args.next().is_some()
        || script.is_empty()
        || !script
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c))
    {
        return Err(invalid());
    }
    let program: Vec<_> = program.encode_wide().chain(Some(0)).collect();
    let mut command: Vec<_> =
        format!("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {script}")
            .encode_utf16()
            .chain(Some(0))
            .collect();
    let job = supervisor_job()?;
    let child = start_supervisor(&program, &mut command, &job)?;
    let mut code = 0;
    // SAFETY: the process and job handles remain owned until the wait completes.
    unsafe {
        if WaitForSingleObject(child.as_raw_handle(), INFINITE) != WAIT_OBJECT_0
            || GetExitCodeProcess(child.as_raw_handle(), &mut code) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(code)
}

fn supervisor_job() -> io::Result<OwnedHandle> {
    // Ending the task closes the launcher's job handle and terminates PowerShell.
    // Its daemon child retains the existing ownership handoff.
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
    // SAFETY: create an unnamed job with default security and take ownership on success.
    unsafe {
        let raw = CreateJobObjectW(ptr::null(), ptr::null());
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = OwnedHandle::from_raw_handle(raw);
        if SetInformationJobObject(
            raw,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of_val(&limits) as u32,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
}

fn start_supervisor(
    program: &[u16],
    command: &mut [u16],
    job: &OwnedHandle,
) -> io::Result<OwnedHandle> {
    // Assign the job during CreateProcessW. An immediate task stop must also
    // stop PowerShell, even before its first instruction.
    let mut size = 0;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    let mut child = PROCESS_INFORMATION::default();
    let handle: HANDLE = job.as_raw_handle();
    // SAFETY: the size query precedes allocation. The aligned storage and job
    // handle outlive the initialized attribute list, including its deletion.
    unsafe {
        InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size);
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) || size == 0 {
            return Err(error);
        }
        let mut storage = vec![0usize; size.div_ceil(size_of::<usize>())];
        let list = storage.as_mut_ptr().cast();
        if InitializeProcThreadAttributeList(list, 1, 0, &mut size) == 0 {
            return Err(io::Error::last_os_error());
        }
        startup.lpAttributeList = list;
        let started = UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
            (&raw const handle).cast(),
            size_of::<HANDLE>(),
            ptr::null_mut(),
            ptr::null(),
        ) != 0
            && CreateProcessW(
                program.as_ptr(),
                command.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
                ptr::null(),
                ptr::null(),
                &startup.StartupInfo,
                &mut child,
            ) != 0;
        let result = if started {
            drop(OwnedHandle::from_raw_handle(child.hThread));
            Ok(OwnedHandle::from_raw_handle(child.hProcess))
        } else {
            Err(io::Error::last_os_error())
        };
        DeleteProcThreadAttributeList(list);
        result
    }
}
