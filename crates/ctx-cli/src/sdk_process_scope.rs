use std::{env, ffi::OsString, io, process::Command};

use anyhow::{bail, Context, Result};

pub(crate) const LAUNCHER_ARG: &str = "__ctx_sdk_process_scope_v1";
pub(crate) const ACTIVE_ENV: &str = "CTX_SDK_PROCESS_SCOPE_ACTIVE";
#[cfg(windows)]
pub(crate) const WINDOWS_DRAIN_FAILURE_EXIT: i32 = 252;

pub(crate) fn run_if_requested() -> Result<Option<i32>> {
    let Some((target, target_args)) = parse_request(env::args_os())? else {
        return Ok(None);
    };
    run_scoped(target, target_args).map(Some)
}

pub(crate) fn active() -> bool {
    marker_present(env::var_os(ACTIVE_ENV).as_deref())
}

fn marker_present(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some()
}

fn parse_request(
    args: impl IntoIterator<Item = OsString>,
) -> Result<Option<(OsString, Vec<OsString>)>> {
    let mut args = args.into_iter();
    let _executable = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new(LAUNCHER_ARG)) {
        return Ok(None);
    }
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
        bail!("invalid SDK process-scope launcher request");
    }
    let target = args
        .next()
        .context("SDK process-scope launcher target is missing")?;
    let target_args = args.collect::<Vec<_>>();
    Ok(Some((target, target_args)))
}

#[cfg(unix)]
fn run_scoped(target: OsString, target_args: Vec<OsString>) -> Result<i32> {
    use std::os::unix::process::CommandExt;

    // No target code or target descendants exist before this boundary. exec preserves the
    // launcher PID as the process-group ID, so the SDK can still signal the full group after the
    // direct child exits while a descendant retains stdout or stderr.
    let pid = unsafe { libc::getpid() };
    let group = unsafe { libc::getpgrp() };
    if group != pid && unsafe { libc::setpgid(0, 0) } != 0 {
        return Err(io::Error::last_os_error()).context("establish SDK process group");
    }
    let error = Command::new(target)
        .args(target_args)
        .env(ACTIVE_ENV, "1")
        .exec();
    Err(error).context("execute SDK process-scope target")
}

#[cfg(windows)]
fn run_scoped(target: OsString, target_args: Vec<OsString>) -> Result<i32> {
    use std::{
        io::Read,
        os::windows::{io::AsRawHandle, process::CommandExt},
        process::Stdio,
        sync::mpsc,
        thread,
        time::Duration,
    };
    use windows_sys::Win32::{
        Foundation::HANDLE,
        System::JobObjects::{AssignProcessToJobObject, TerminateJobObject},
    };

    const CREATE_SUSPENDED: u32 = 0x0000_0004;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const ACK_BYTE: u8 = 0x06;
    const DRAIN_ACK_TIMEOUT: Duration = Duration::from_millis(750);

    let job = WindowsJob::create()?;
    job.configure()?;
    let mut child = Command::new(target)
        .args(target_args)
        .env(ACTIVE_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .creation_flags(CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .context("start suspended SDK process-scope target")?;
    let process_handle = child.as_raw_handle() as HANDLE;
    if unsafe { AssignProcessToJobObject(job.0, process_handle) } == 0 {
        let error = io::Error::last_os_error();
        let _ = child.kill();
        reap_child_bounded(&mut child, DRAIN_ACK_TIMEOUT);
        return Err(error).context("assign SDK process-scope target to job");
    }
    if let Err(error) = resume_process(process_handle) {
        let _ = unsafe { TerminateJobObject(job.0, 1) };
        reap_child_bounded(&mut child, DRAIN_ACK_TIMEOUT);
        return Err(error);
    }

    let (ack_sender, ack_receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut byte = [0_u8; 1];
        let acknowledged = io::stdin().read_exact(&mut byte).is_ok() && byte[0] == ACK_BYTE;
        let _ = ack_sender.send(acknowledged);
    });
    close_parent_output_handles();

    let status = child.wait().context("wait for SDK process-scope target")?;
    let exit_code = status.code().unwrap_or(1);
    if !status.success() {
        let _ = unsafe { TerminateJobObject(job.0, exit_code as u32) };
        return Ok(exit_code);
    }
    if ack_receiver.recv_timeout(DRAIN_ACK_TIMEOUT) != Ok(true) {
        let _ = unsafe { TerminateJobObject(job.0, WINDOWS_DRAIN_FAILURE_EXIT as u32) };
        return Ok(WINDOWS_DRAIN_FAILURE_EXIT);
    }
    Ok(0)
}

#[cfg(windows)]
fn reap_child_bounded(child: &mut std::process::Child, timeout: std::time::Duration) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(None) | Err(_) => break,
        }
    }
    let _ = child.kill();
    let hard_deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < hard_deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(windows)]
struct WindowsJob(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl WindowsJob {
    fn create() -> Result<Self> {
        let handle = unsafe {
            windows_sys::Win32::System::JobObjects::CreateJobObjectW(
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if handle.is_null() {
            return Err(io::Error::last_os_error()).context("create SDK process-scope job");
        }
        Ok(Self(handle))
    }

    fn configure(&self) -> Result<()> {
        use windows_sys::Win32::System::JobObjects::{
            JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let result = unsafe {
            SetInformationJobObject(
                self.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if result == 0 {
            return Err(io::Error::last_os_error()).context("configure SDK process-scope job");
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        let _ = unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(windows)]
fn resume_process(process: windows_sys::Win32::Foundation::HANDLE) -> Result<()> {
    type NtResumeProcess = unsafe extern "system" fn(windows_sys::Win32::Foundation::HANDLE) -> i32;

    let library = unsafe { libloading::Library::new("ntdll.dll") }
        .context("load ntdll for SDK process-scope resume")?;
    let resume = unsafe { library.get::<NtResumeProcess>(b"NtResumeProcess\0") }
        .context("resolve NtResumeProcess for SDK process-scope resume")?;
    let status = unsafe { resume(process) };
    if status != 0 {
        bail!("resume SDK process-scope target failed with NTSTATUS 0x{status:08x}");
    }
    Ok(())
}

#[cfg(windows)]
fn close_parent_output_handles() {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::Console::{GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE},
    };

    for stream in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        let handle = unsafe { GetStdHandle(stream) };
        let _ = unsafe { SetStdHandle(stream, std::ptr::null_mut()) };
        if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
            let _ = unsafe { CloseHandle(handle) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{marker_present, parse_request, LAUNCHER_ARG};
    use std::ffi::{OsStr, OsString};

    #[test]
    fn launcher_protocol_removes_its_sentinel_before_target_execution() {
        let request = parse_request([
            OsString::from("ctx"),
            OsString::from(LAUNCHER_ARG),
            OsString::from("--"),
            OsString::from("/opt/ctx/ctx"),
            OsString::from("status"),
            OsString::from("--json"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(request.0, OsString::from("/opt/ctx/ctx"));
        assert_eq!(
            request.1,
            [OsString::from("status"), OsString::from("--json")]
        );
    }

    #[test]
    fn ordinary_cli_arguments_do_not_enter_launcher_mode() {
        assert!(parse_request([
            OsString::from("ctx"),
            OsString::from("status"),
            OsString::from("--json"),
        ])
        .unwrap()
        .is_none());
    }

    #[test]
    fn scoped_marker_suppresses_nested_background_ownership() {
        assert!(!marker_present(None));
        assert!(marker_present(Some(OsStr::new("1"))));
    }
}
