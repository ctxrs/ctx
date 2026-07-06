use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Result};

use super::util::now_unix_s;

const LOCK_FILE: &str = "upgrade.lock";
const STALE_UPGRADE_LOCK_AFTER: Duration = Duration::from_secs(30 * 60);

pub(super) struct UpgradeLock {
    path: PathBuf,
}

impl UpgradeLock {
    pub(super) fn acquire(data_root: &Path) -> Result<Self> {
        fs::create_dir_all(data_root)?;
        let path = data_root.join(LOCK_FILE);
        for _ in 0..2 {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    writeln!(file, "{} {}", std::process::id(), now_unix_s())?;
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if stale_upgrade_lock_reason(&path).is_some() {
                        match fs::remove_file(&path) {
                            Ok(()) => continue,
                            Err(remove_error)
                                if remove_error.kind() == std::io::ErrorKind::NotFound =>
                            {
                                continue;
                            }
                            Err(remove_error) => {
                                return Err(anyhow!(
                                    "ctx upgrade lock is stale but could not be removed at {}: {remove_error}",
                                    path.display()
                                ));
                            }
                        }
                    }
                    return Err(anyhow!(
                        "ctx upgrade lock is held at {}: {error}",
                        path.display()
                    ));
                }
                Err(error) => {
                    return Err(anyhow!(
                        "ctx upgrade lock is held at {}: {error}",
                        path.display()
                    ));
                }
            }
        }
        Err(anyhow!(
            "ctx upgrade lock could not be acquired at {}",
            path.display()
        ))
    }
}

fn stale_upgrade_lock_reason(path: &Path) -> Option<String> {
    let contents = fs::read_to_string(path).ok();
    let (pid, created_at) = contents
        .as_deref()
        .map(parse_upgrade_lock)
        .unwrap_or((None, None));
    if let Some(pid) = pid {
        match process_state(pid) {
            ProcessState::Running => return None,
            ProcessState::NotRunning => {
                return Some(format!(
                    "recorded upgrade process {pid} is no longer running"
                ));
            }
            ProcessState::Unknown => {}
        }
    }
    if lock_age_seconds(path, created_at)
        .is_some_and(|age| age >= STALE_UPGRADE_LOCK_AFTER.as_secs())
    {
        return Some(format!(
            "upgrade lock is older than {} seconds",
            STALE_UPGRADE_LOCK_AFTER.as_secs()
        ));
    }
    None
}

fn parse_upgrade_lock(contents: &str) -> (Option<u32>, Option<u64>) {
    let mut fields = contents.split_whitespace();
    let pid = fields.next().and_then(|value| value.parse::<u32>().ok());
    let created_at = fields.next().and_then(|value| value.parse::<u64>().ok());
    (pid, created_at)
}

fn lock_age_seconds(path: &Path, created_at: Option<u64>) -> Option<u64> {
    if let Some(created_at) = created_at {
        return Some(now_unix_s().saturating_sub(created_at));
    }
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.elapsed().ok())
        .map(|age| age.as_secs())
}

enum ProcessState {
    Running,
    NotRunning,
    Unknown,
}

#[cfg(unix)]
fn process_state(pid: u32) -> ProcessState {
    if pid == 0 {
        return ProcessState::NotRunning;
    }
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return ProcessState::Running;
    }
    match last_errno() {
        Some(libc::ESRCH) => ProcessState::NotRunning,
        Some(libc::EPERM) => ProcessState::Running,
        _ => ProcessState::Unknown,
    }
}

#[cfg(not(unix))]
fn process_state(_pid: u32) -> ProcessState {
    ProcessState::Unknown
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn last_errno() -> Option<i32> {
    Some(unsafe { *libc::__errno_location() })
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn last_errno() -> Option<i32> {
    Some(unsafe { *libc::__error() })
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))
))]
fn last_errno() -> Option<i32> {
    None
}

impl Drop for UpgradeLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
