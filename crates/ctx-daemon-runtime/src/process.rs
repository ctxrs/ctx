use std::{env, fmt::Write as _, fs, io::Read, path::Path};

use anyhow::{Context, Result};
use ring::digest::{Context as DigestContext, SHA256};
use serde_json::{json, Value};

use crate::{daemon_lock_path, pid_lock_payload, read_pid_lock_json};

pub fn current_daemon_lock_identity(data_root: &Path) -> Result<Value> {
    let binary = env::current_exe().context("resolve ctx daemon executable identity")?;
    Ok(pid_lock_payload(json!({
        "binary": binary,
        "binary_sha256": executable_sha256(&binary)?,
        "data_root": data_root,
    })))
}

pub fn daemon_lock_matches_executable(data_root: &Path, executable: &Path) -> Result<bool> {
    let Some(value) = read_pid_lock_json(&daemon_lock_path(data_root)) else {
        return Ok(false);
    };
    daemon_lock_binary_identity_matches(&value, executable)
}

pub fn daemon_lock_binary_identity_matches(value: &Value, executable: &Path) -> Result<bool> {
    let Some(recorded_binary) = value.get("binary").and_then(Value::as_str).map(Path::new) else {
        return Ok(false);
    };
    if fs::canonicalize(recorded_binary).ok() != fs::canonicalize(executable).ok() {
        return Ok(false);
    }
    let Some(recorded_sha256) = value.get("binary_sha256").and_then(Value::as_str) else {
        return Ok(false);
    };
    Ok(recorded_sha256 == executable_sha256(executable)?)
}

pub fn daemon_owner_binary_identity_matches(value: &Value, executable: &Path) -> Result<bool> {
    if !daemon_lock_binary_identity_matches(value, executable)? {
        return Ok(false);
    }
    let Some(pid) = value
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
    else {
        return Ok(false);
    };
    let Some(recorded_sha256) = value.get("binary_sha256").and_then(Value::as_str) else {
        return Ok(false);
    };
    Ok(process_executable_sha256(pid).as_deref() == Some(recorded_sha256))
}

pub fn process_executable_sha256(pid: u32) -> Option<String> {
    process_executable_path(pid)
        .as_deref()
        .and_then(|path| executable_sha256(path).ok())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessState {
    Running,
    NotRunning,
    Unknown,
}

#[cfg(unix)]
pub fn process_session_id(pid: u32) -> std::io::Result<u32> {
    let pid = libc::pid_t::try_from(pid).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "process ID overflow")
    })?;
    let session = unsafe { libc::getsid(pid) };
    if session == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        u32::try_from(session).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "negative process session ID",
            )
        })
    }
}

#[cfg(unix)]
pub fn process_group_id(pid: u32) -> std::io::Result<u32> {
    let pid = libc::pid_t::try_from(pid).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "process ID overflow")
    })?;
    let group = unsafe { libc::getpgid(pid) };
    if group == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        u32::try_from(group).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "negative process group ID")
        })
    }
}

#[cfg(unix)]
pub fn process_state(pid: u32) -> ProcessState {
    if pid == 0 {
        return ProcessState::NotRunning;
    }
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return ProcessState::NotRunning;
    };
    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        return ProcessState::Running;
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => ProcessState::NotRunning,
        Some(libc::EPERM) => ProcessState::Running,
        _ => ProcessState::Unknown,
    }
}

#[cfg(windows)]
pub fn process_state(pid: u32) -> ProcessState {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    if pid == 0 {
        return ProcessState::NotRunning;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if !handle.is_null() {
        unsafe {
            CloseHandle(handle);
        }
        return ProcessState::Running;
    }
    match unsafe { GetLastError() } {
        windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER => ProcessState::NotRunning,
        ERROR_ACCESS_DENIED => ProcessState::Running,
        _ => ProcessState::Unknown,
    }
}

#[cfg(not(any(unix, windows)))]
pub fn process_state(_pid: u32) -> ProcessState {
    ProcessState::Unknown
}

#[cfg(target_os = "linux")]
pub fn process_executable_path(pid: u32) -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from(format!("/proc/{pid}/exe")))
}

#[cfg(target_os = "macos")]
pub fn process_executable_path(pid: u32) -> Option<std::path::PathBuf> {
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
        .map(|path| std::path::PathBuf::from(path.to_string_lossy().into_owned()))
}

#[cfg(target_os = "freebsd")]
pub fn process_executable_path(pid: u32) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;

    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PATHNAME,
        libc::c_int::try_from(pid).ok()?,
    ];
    let mut buffer = vec![0_u8; libc::PATH_MAX as usize];
    let mut length = buffer.len();
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buffer.as_mut_ptr().cast(),
            &raw mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || length == 0 {
        return None;
    }
    let end = buffer[..length]
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(length);
    Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(
        &buffer[..end],
    )))
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))
))]
pub fn process_executable_path(pid: u32) -> Option<std::path::PathBuf> {
    [format!("/proc/{pid}/file"), format!("/proc/{pid}/exe")]
        .into_iter()
        .map(std::path::PathBuf::from)
        .find(|path| fs::metadata(path).is_ok())
}

#[cfg(windows)]
pub fn process_executable_path(pid: u32) -> Option<std::path::PathBuf> {
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
        std::path::PathBuf::from(String::from_utf16_lossy(
            &buffer[..usize::try_from(length).unwrap_or(0)],
        ))
    })
}

#[cfg(not(any(unix, windows)))]
pub fn process_executable_path(_pid: u32) -> Option<std::path::PathBuf> {
    None
}

pub fn executable_sha256(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("open executable identity {}", path.display()))?;
    let mut hasher = DigestContext::new(&SHA256);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .with_context(|| format!("read executable identity {}", path.display()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let mut encoded = String::with_capacity(64);
    for byte in hasher.finish().as_ref() {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn binary_identity_detects_same_path_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("ctx");
        fs::write(&executable, b"old executable image").unwrap();
        let lock = json!({
            "binary": executable,
            "binary_sha256": executable_sha256(&executable).unwrap(),
        });

        assert!(daemon_lock_binary_identity_matches(&lock, &executable).unwrap());
        fs::write(&executable, b"new executable image").unwrap();
        assert!(!daemon_lock_binary_identity_matches(&lock, &executable).unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_owner_identity_reads_the_process_image_not_a_replaced_path() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("ctx");
        fs::copy("/bin/sleep", &executable).unwrap();
        let spawn_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut child = loop {
            match std::process::Command::new(&executable).arg("30").spawn() {
                Ok(child) => break child,
                Err(error)
                    if error.raw_os_error() == Some(libc::ETXTBSY)
                        && std::time::Instant::now() < spawn_deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("spawn copied test executable: {error}"),
            }
        };
        let lock = json!({
            "pid": child.id(),
            "binary": executable,
            "binary_sha256": executable_sha256(&executable).unwrap(),
        });

        assert!(daemon_owner_binary_identity_matches(&lock, &executable).unwrap());
        let replacement = temp.path().join("ctx.new");
        fs::copy("/bin/true", &replacement).unwrap();
        fs::rename(&replacement, &executable).unwrap();
        let forged_current_lock = json!({
            "pid": child.id(),
            "binary": executable,
            "binary_sha256": executable_sha256(&executable).unwrap(),
        });
        assert!(daemon_lock_binary_identity_matches(&forged_current_lock, &executable).unwrap());
        assert!(!daemon_owner_binary_identity_matches(&forged_current_lock, &executable).unwrap());
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
