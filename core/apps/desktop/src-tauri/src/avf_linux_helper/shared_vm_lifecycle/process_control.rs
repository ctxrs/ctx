use super::*;

pub(crate) fn request_shared_vm_shutdown(data_root: &Path) -> Result<()> {
    let path = shared_vm_shutdown_request_path(data_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(&path, now_timestamp_string()).with_context(|| format!("writing {}", path.display()))
}

pub(crate) fn clear_shared_vm_shutdown_request(data_root: &Path) {
    let path = shared_vm_shutdown_request_path(data_root);
    if path.exists() {
        let _ = fs::remove_file(path);
    }
}

pub(crate) fn request_shared_vm_memory_pressure_stop(data_root: &Path, note: &str) -> Result<()> {
    let path = shared_vm_memory_pressure_request_path(data_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(&path, note).with_context(|| format!("writing {}", path.display()))
}

pub(crate) fn shared_vm_memory_pressure_stop_requested_note(
    data_root: &Path,
) -> Result<Option<String>> {
    let path = shared_vm_memory_pressure_request_path(data_root);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(Some(raw.trim().to_string()))
}

pub(crate) fn clear_shared_vm_memory_pressure_stop_request(data_root: &Path) {
    let path = shared_vm_memory_pressure_request_path(data_root);
    if path.exists() {
        let _ = fs::remove_file(path);
    }
}

pub(crate) fn shared_vm_shutdown_requested(data_root: &Path) -> bool {
    shared_vm_shutdown_request_path(data_root).exists()
}

pub(crate) fn wait_for_process_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if process_has_exited(pid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    process_has_exited(pid)
}

#[cfg(unix)]
fn process_has_exited(pid: u32) -> bool {
    if let Some(exited) = try_reap_owned_process(pid) {
        return exited;
    }
    !shared_vm_server_process_alive(pid)
}

#[cfg(not(unix))]
fn process_has_exited(pid: u32) -> bool {
    !shared_vm_server_process_alive(pid)
}

#[cfg(unix)]
fn try_reap_owned_process(pid: u32) -> Option<bool> {
    loop {
        let mut status = 0_i32;
        let result = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
        if result == pid as i32 {
            return Some(true);
        }
        if result == 0 {
            return Some(false);
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(code) if code == libc::EINTR => continue,
            Some(code) if code == libc::ECHILD => return None,
            _ => return None,
        }
    }
}

#[cfg(unix)]
pub(crate) fn shared_vm_server_process_alive(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        return true;
    }
    let err = std::io::Error::last_os_error();
    err.raw_os_error() != Some(libc::ESRCH)
}

#[cfg(not(unix))]
pub(crate) fn shared_vm_server_process_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
pub(crate) fn stop_shared_vm_server(pid: u32) {
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
pub(crate) fn stop_shared_vm_server(pid: u32) {
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status();
}
