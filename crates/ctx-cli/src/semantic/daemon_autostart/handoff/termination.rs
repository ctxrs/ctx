use super::*;

#[cfg(unix)]
pub(super) fn terminate_identity_verified_residual_daemon(
    data_root: &Path,
    expected_executable: &Path,
) -> Result<()> {
    let lock_path = daemon_lock_path(data_root);
    let value = read_pid_lock_json(&lock_path)
        .ok_or_else(|| anyhow!("active ctx daemon lock has no readable identity"))?;
    let pid = pid_from_lock_json(&value)
        .ok_or_else(|| anyhow!("active ctx daemon lock has no process identity"))?;
    let signal_target = UnixSignalTarget::open(pid)?;
    verify_residual_daemon_identity(data_root, expected_executable, pid, &value)?;
    signal_target.signal(data_root, expected_executable, libc::SIGTERM)?;
    let term_deadline = Instant::now() + DAEMON_UPGRADE_RESTART_TIMEOUT;
    while daemon_lock_is_active(data_root) && Instant::now() < term_deadline {
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    }
    if !daemon_lock_is_active(data_root) {
        return Ok(());
    }

    signal_target.signal(data_root, expected_executable, libc::SIGKILL)?;
    Ok(())
}

#[cfg(unix)]
enum UnixSignalTarget {
    #[cfg(target_os = "linux")]
    PidFd(LinuxPidFd),
    ReverifiedPid(u32),
}

#[cfg(unix)]
impl UnixSignalTarget {
    fn open(pid: u32) -> Result<Self> {
        #[cfg(target_os = "linux")]
        if let Some(pidfd) = LinuxPidFd::open(pid)? {
            return Ok(Self::PidFd(pidfd));
        }
        Ok(Self::ReverifiedPid(pid))
    }

    fn signal(
        &self,
        data_root: &Path,
        expected_executable: &Path,
        signal: libc::c_int,
    ) -> Result<()> {
        let pid = match self {
            #[cfg(target_os = "linux")]
            Self::PidFd(pidfd) => pidfd.pid,
            Self::ReverifiedPid(pid) => *pid,
        };
        reverify_residual_daemon_identity(data_root, expected_executable, pid)?;
        match self {
            #[cfg(target_os = "linux")]
            Self::PidFd(pidfd) => pidfd.signal(signal),
            Self::ReverifiedPid(pid) => signal_verified_process(*pid, signal),
        }
    }
}

#[cfg(target_os = "linux")]
struct LinuxPidFd {
    fd: std::os::fd::OwnedFd,
    pid: u32,
}

#[cfg(target_os = "linux")]
impl LinuxPidFd {
    fn open(pid: u32) -> Result<Option<Self>> {
        use std::os::fd::FromRawFd as _;

        let raw_fd = unsafe {
            libc::syscall(
                libc::SYS_pidfd_open,
                libc::pid_t::try_from(pid)
                    .map_err(|_| anyhow!("invalid daemon process identity"))?,
                0_u32,
            )
        };
        if raw_fd >= 0 {
            let raw_fd = i32::try_from(raw_fd).context("convert Linux pidfd")?;
            return Ok(Some(Self {
                fd: unsafe { std::os::fd::OwnedFd::from_raw_fd(raw_fd) },
                pid,
            }));
        }
        let error = std::io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EPERM)
        ) {
            return Ok(None);
        }
        Err(error).context("open stable Linux pidfd for residual ctx daemon")
    }

    fn signal(&self, signal: libc::c_int) -> Result<()> {
        use std::os::fd::AsRawFd as _;

        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.fd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0_u32,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(error).context("signal residual ctx daemon through Linux pidfd")
    }
}

#[cfg(unix)]
fn reverify_residual_daemon_identity(
    data_root: &Path,
    expected_executable: &Path,
    expected_pid: u32,
) -> Result<()> {
    let current = read_pid_lock_json(&daemon_lock_path(data_root))
        .ok_or_else(|| anyhow!("ctx daemon identity disappeared before termination signal"))?;
    if pid_from_lock_json(&current) != Some(expected_pid) {
        return Err(anyhow!(
            "ctx daemon ownership changed before termination signal; refusing to signal"
        ));
    }
    verify_residual_daemon_identity(data_root, expected_executable, expected_pid, &current)
}

#[cfg(unix)]
fn verify_residual_daemon_identity(
    data_root: &Path,
    expected_executable: &Path,
    pid: u32,
    value: &Value,
) -> Result<()> {
    if pid == process::id() {
        return Err(anyhow!("refusing to terminate the current ctx process"));
    }
    if observe_pid_advisory_lock(&daemon_lock_path(data_root))
        != Some(PidAdvisoryLockObservation {
            held: true,
            released: false,
        })
    {
        return Err(anyhow!(
            "ctx daemon owner lock is not held; refusing residual termination"
        ));
    }
    let recorded_root = value
        .get("data_root")
        .and_then(Value::as_str)
        .map(Path::new)
        .ok_or_else(|| anyhow!("ctx daemon lock has no data-root identity"))?;
    if fs::canonicalize(recorded_root).ok() != fs::canonicalize(data_root).ok() {
        return Err(anyhow!(
            "ctx daemon lock data-root identity does not match uninstall target"
        ));
    }
    let recorded_binary = value
        .get("binary")
        .and_then(Value::as_str)
        .map(Path::new)
        .ok_or_else(|| anyhow!("ctx daemon lock has no executable identity"))?;
    if !same_unix_file(recorded_binary, expected_executable)? {
        return Err(anyhow!(
            "ctx daemon lock executable is not the installed ctx executable"
        ));
    }
    let process_executable = unix_process_executable(pid).ok_or_else(|| {
        anyhow!(
            "cannot verify executable identity for residual ctx process {pid}; refusing to signal"
        )
    })?;
    if !same_unix_file(&process_executable, expected_executable)? {
        return Err(anyhow!(
            "residual lock owner is not the installed ctx executable; refusing to signal"
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn same_unix_file(left: &Path, right: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let left = fs::metadata(left)
        .with_context(|| format!("inspect executable identity {}", left.display()))?;
    let right = fs::metadata(right)
        .with_context(|| format!("inspect executable identity {}", right.display()))?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

#[cfg(target_os = "linux")]
fn unix_process_executable(pid: u32) -> Option<PathBuf> {
    fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(target_os = "macos")]
fn unix_process_executable(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;

    const MAX_PATH_BYTES: usize = 4096;
    unsafe extern "C" {
        fn proc_pidpath(pid: libc::c_int, buffer: *mut libc::c_void, size: u32) -> libc::c_int;
    }
    let mut buffer = vec![0_u8; MAX_PATH_BYTES];
    let length = unsafe {
        proc_pidpath(
            libc::pid_t::try_from(pid).ok()?,
            buffer.as_mut_ptr().cast(),
            u32::try_from(buffer.len()).ok()?,
        )
    };
    if length <= 0 {
        return None;
    }
    CStr::from_bytes_until_nul(&buffer)
        .ok()
        .map(|path| PathBuf::from(path.to_string_lossy().into_owned()))
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn unix_process_executable(pid: u32) -> Option<PathBuf> {
    [format!("/proc/{pid}/file"), format!("/proc/{pid}/exe")]
        .into_iter()
        .find_map(|path| fs::read_link(path).ok())
}

#[cfg(unix)]
fn signal_verified_process(pid: u32, signal: libc::c_int) -> Result<()> {
    let pid = libc::pid_t::try_from(pid).map_err(|_| anyhow!("invalid daemon process identity"))?;
    if unsafe { libc::kill(pid, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(error).context("signal identity-verified residual ctx daemon")
}

#[cfg(windows)]
pub(super) fn terminate_identity_verified_residual_daemon(
    data_root: &Path,
    expected_executable: &Path,
) -> Result<()> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        },
    };

    let lock_path = daemon_lock_path(data_root);
    let value = read_pid_lock_json(&lock_path)
        .ok_or_else(|| anyhow!("active ctx daemon lock has no readable identity"))?;
    let pid = pid_from_lock_json(&value)
        .ok_or_else(|| anyhow!("active ctx daemon lock has no process identity"))?;
    verify_residual_daemon_identity(data_root, expected_executable, pid, &value)?;
    let handle = unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        )
    };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error())
            .context("open identity-verified residual ctx daemon");
    }
    let terminated = unsafe { TerminateProcess(handle, 0) };
    unsafe {
        CloseHandle(handle);
    }
    if terminated == 0 {
        return Err(std::io::Error::last_os_error())
            .context("terminate identity-verified residual ctx daemon");
    }
    Ok(())
}

#[cfg(windows)]
fn verify_residual_daemon_identity(
    data_root: &Path,
    expected_executable: &Path,
    pid: u32,
    value: &Value,
) -> Result<()> {
    if pid == process::id() {
        return Err(anyhow!("refusing to terminate the current ctx process"));
    }
    if observe_pid_advisory_lock(&daemon_lock_path(data_root))
        != Some(PidAdvisoryLockObservation {
            held: true,
            released: false,
        })
    {
        return Err(anyhow!(
            "ctx daemon owner lock is not held; refusing residual termination"
        ));
    }
    let recorded_root = value
        .get("data_root")
        .and_then(Value::as_str)
        .map(Path::new)
        .ok_or_else(|| anyhow!("ctx daemon lock has no data-root identity"))?;
    if !same_windows_path(recorded_root, data_root) {
        return Err(anyhow!(
            "ctx daemon lock data-root identity does not match uninstall target"
        ));
    }
    let recorded_binary = value
        .get("binary")
        .and_then(Value::as_str)
        .map(Path::new)
        .ok_or_else(|| anyhow!("ctx daemon lock has no executable identity"))?;
    if !same_windows_path(recorded_binary, expected_executable) {
        return Err(anyhow!(
            "ctx daemon lock executable is not the installed ctx executable"
        ));
    }
    let process_executable = windows_process_executable(pid).ok_or_else(|| {
        anyhow!(
            "cannot verify executable identity for residual ctx process {pid}; refusing to terminate"
        )
    })?;
    if !same_windows_path(&process_executable, expected_executable) {
        return Err(anyhow!(
            "residual lock owner is not the installed ctx executable; refusing to terminate"
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn same_windows_path(left: &Path, right: &Path) -> bool {
    let normalize = |path: &Path| {
        fs::canonicalize(path)
            .ok()
            .map(|path| path.to_string_lossy().to_lowercase())
    };
    normalize(left) == normalize(right)
}

#[cfg(windows)]
fn windows_process_executable(pid: u32) -> Option<PathBuf> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut buffer = vec![0_u16; 32_768];
    let mut length = u32::try_from(buffer.len()).ok()?;
    let succeeded =
        unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &raw mut length) };
    unsafe {
        CloseHandle(handle);
    }
    (succeeded != 0).then(|| {
        PathBuf::from(String::from_utf16_lossy(
            &buffer[..usize::try_from(length).unwrap_or(0)],
        ))
    })
}

#[cfg(not(any(unix, windows)))]
pub(super) fn terminate_identity_verified_residual_daemon(
    _data_root: &Path,
    _expected_executable: &Path,
) -> Result<()> {
    Err(anyhow!(
        "this platform cannot identity-verify residual daemon termination"
    ))
}

#[cfg(all(test, target_os = "linux"))]
mod pidfd_tests {
    use super::*;

    #[test]
    fn linux_pidfd_signals_the_opened_process_handle() -> Result<()> {
        let mut child = Command::new("sleep").arg("30").spawn()?;
        let Some(pidfd) = LinuxPidFd::open(child.id())? else {
            child.kill()?;
            child.wait()?;
            eprintln!("Linux pidfd unavailable; exercised the explicit fallback branch");
            return Ok(());
        };
        eprintln!("Linux pidfd available; exercising stable-handle signaling");
        if let Err(error) = pidfd.signal(libc::SIGTERM) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let status = child.wait()?;
        assert!(!status.success());
        Ok(())
    }
}
