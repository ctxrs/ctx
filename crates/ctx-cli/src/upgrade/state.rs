use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::utc_now;
use serde_json::{json, Value};

use super::UpgradePlan;

pub(super) const STATE_FILE: &str = "upgrade-state.json";
const LOCK_FILE: &str = "upgrade.lock";
const LOG_FILE: &str = "logs/upgrade.log";
const STALE_UPGRADE_LOCK_AFTER: Duration = Duration::from_secs(30 * 60);

pub(super) fn write_state_checked(
    data_root: &Path,
    plan: &UpgradePlan,
    status: &str,
) -> Result<()> {
    let body = json!({
        "schema_version": 1,
        "status": status,
        "checked_at": utc_now(),
        "last_checked_unix_s": now_unix_s(),
        "current_version": plan.current_version,
        "latest_version": plan.latest_version,
        "update_available": plan.update_available,
        "channel": plan.channel,
        "platform": plan.platform,
        "metadata_url": plan.metadata_url,
        "artifact_url": plan.artifact_url,
        "install_path": plan.install_path,
        "managed": plan.managed,
    });
    atomic_write_json(&data_root.join(STATE_FILE), &body)
}

pub(super) fn write_state_error(data_root: &Path, error: &str) -> Result<()> {
    let body = json!({
        "schema_version": 1,
        "status": "error",
        "checked_at": utc_now(),
        "last_checked_unix_s": now_unix_s(),
        "error": error,
    });
    atomic_write_json(&data_root.join(STATE_FILE), &body)
}

pub(super) fn should_check_now(data_root: &Path, interval: Duration) -> bool {
    if interval.is_zero() {
        return true;
    }
    let Some(value) = read_json_file(&data_root.join(STATE_FILE)) else {
        return true;
    };
    let Some(last) = value.get("last_checked_unix_s").and_then(Value::as_u64) else {
        return true;
    };
    now_unix_s().saturating_sub(last) >= interval.as_secs()
}

pub(super) fn read_json_file(path: &Path) -> Option<Value> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

pub(super) fn atomic_write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let body = serde_json::to_vec_pretty(value)?;
    fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} to {}", tmp.display(), path.display()))
}

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

pub(super) fn append_upgrade_log(data_root: &Path, message: &str) {
    let path = data_root.join(LOG_FILE);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{} {}", utc_now().to_rfc3339(), message);
    }
}

pub(super) fn now_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(super) fn set_auto_mode(data_root: &Path, mode: &str) -> Result<()> {
    fs::create_dir_all(data_root)?;
    let config_path = data_root.join(crate::config::CONFIG_FILE);
    let existing = fs::read_to_string(&config_path).unwrap_or_default();
    let next = set_toml_section_value(&existing, "upgrade", "auto", &format!("\"{mode}\""));
    fs::write(&config_path, next).with_context(|| format!("write {}", config_path.display()))?;
    println!("ctx background auto-upgrade {mode}");
    Ok(())
}

fn set_toml_section_value(input: &str, section: &str, key: &str, value: &str) -> String {
    let mut lines = Vec::new();
    let mut in_section = false;
    let mut saw_section = false;
    let mut wrote_key = false;
    for raw in input.lines() {
        let trimmed = raw.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_section && !wrote_key {
                lines.push(format!("{key} = {value}"));
                wrote_key = true;
            }
            in_section = trimmed == format!("[{section}]");
            saw_section |= in_section;
            lines.push(raw.to_owned());
            continue;
        }
        if in_section
            && (trimmed.starts_with(&format!("{key} ")) || trimmed.starts_with(&format!("{key}=")))
        {
            lines.push(format!("{key} = {value}"));
            wrote_key = true;
        } else {
            lines.push(raw.to_owned());
        }
    }
    if saw_section {
        if in_section && !wrote_key {
            lines.push(format!("{key} = {value}"));
        }
    } else {
        if !lines.is_empty() && lines.last().is_some_and(|line| !line.is_empty()) {
            lines.push(String::new());
        }
        lines.push(format!("[{section}]"));
        lines.push(format!("{key} = {value}"));
    }
    lines.join("\n") + "\n"
}
