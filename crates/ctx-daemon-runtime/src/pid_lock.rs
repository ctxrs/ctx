use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result};
use ctx_history_core::utc_now;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    create_private_dir_all, current_daemon_lock_identity, daemon_lock_path, daemon_root_path,
    private_create_new_lock_file, private_open_existing_lock_file, process_state,
    secure_private_file_permissions, ProcessState,
};

pub const DAEMON_LOCK_STALE_AFTER_MS: i64 = 25 * 60 * 60 * 1_000;
pub const PID_LOCK_INCOMPLETE_GRACE: Duration = Duration::from_secs(30);
pub const PID_LOCK_PROTOCOL: &str = "advisory-v1";
pub const PID_LOCK_ACQUIRE_ATTEMPTS: usize = 20;
pub const PID_LOCK_ACQUIRE_RETRY: Duration = Duration::from_millis(2);

pub struct DaemonLock {
    _inner: PidFileLock,
}

impl DaemonLock {
    pub fn acquire(data_root: &Path) -> Result<Option<Self>> {
        ctx_history_platform::platform_security::establish_private_data_root(data_root)?;
        create_private_dir_all(&daemon_root_path(data_root))?;
        let payload = current_daemon_lock_identity(data_root)?;
        Ok(PidFileLock::acquire(&daemon_lock_path(data_root), payload)?
            .map(|inner| Self { _inner: inner }))
    }

    pub fn started_at_ms(&self) -> Option<i64> {
        self._inner
            .payload
            .get("started_at_ms")
            .and_then(Value::as_i64)
            .filter(|started_at_ms| *started_at_ms > 0)
    }
}

/// Excludes daemon ownership for a bounded cleanup transition without
/// publishing the cleanup process as a daemon owner.
pub struct DaemonQuiescenceGuard {
    _guard: fs::File,
}

impl DaemonQuiescenceGuard {
    pub fn acquire(data_root: &Path) -> Result<Option<Self>> {
        ctx_history_platform::platform_security::establish_private_data_root(data_root)?;
        create_private_dir_all(&daemon_root_path(data_root))?;
        let lock_path = daemon_lock_path(data_root);
        let guard_path = pid_lock_guard_path(&lock_path);
        let (guard, _) = open_or_create_pid_lock_file(&guard_path).with_context(|| {
            format!("open ctx daemon quiescence guard {}", guard_path.display())
        })?;
        secure_private_file_permissions(&guard_path)?;
        if !try_lock_pid_file(&guard)? {
            return Ok(None);
        }
        let metadata = read_pid_lock_json(&lock_path);
        let legacy_owner_is_live = lock_path.exists()
            && metadata
                .as_ref()
                .is_some_and(|value| !pid_lock_uses_advisory_protocol(value))
            && !legacy_pid_lock_value_is_stale(&lock_path, metadata.as_ref());
        if legacy_owner_is_live {
            let _ = fs2::FileExt::unlock(&guard);
            return Ok(None);
        }
        Ok(Some(Self { _guard: guard }))
    }
}

pub struct PidFileLock {
    guard: fs::File,
    path: PathBuf,
    payload: Value,
}

impl PidFileLock {
    pub fn acquire(path: &Path, payload: Value) -> Result<Option<Self>> {
        let guard_path = pid_lock_guard_path(path);
        let (guard, _) = open_or_create_pid_lock_file(&guard_path)
            .with_context(|| format!("open ctx process guard {}", guard_path.display()))?;
        secure_private_file_permissions(&guard_path)?;
        if !try_lock_pid_file(&guard)
            .with_context(|| format!("lock ctx process guard {}", guard_path.display()))?
        {
            return Ok(None);
        }

        let previous = read_pid_lock_json(path);
        if path.exists()
            && !previous
                .as_ref()
                .is_some_and(pid_lock_uses_advisory_protocol)
            && !legacy_pid_lock_value_is_stale(path, previous.as_ref())
        {
            let _ = fs2::FileExt::unlock(&guard);
            return Ok(None);
        }
        if !publish_pid_lock_metadata(path, &payload)? {
            let _ = fs2::FileExt::unlock(&guard);
            return Ok(None);
        }
        Ok(Some(Self {
            guard,
            path: path.to_path_buf(),
            payload,
        }))
    }
}

impl Drop for PidFileLock {
    fn drop(&mut self) {
        if pid_lock_path_has_owner(&self.path, &self.payload) {
            if let Some(object) = self.payload.as_object_mut() {
                object.insert("released".to_owned(), Value::Bool(true));
            }
            let _ = publish_pid_lock_metadata(&self.path, &self.payload);
        }
        let _ = fs2::FileExt::unlock(&self.guard);
    }
}

pub fn pid_lock_guard_path(path: &Path) -> PathBuf {
    path.with_extension("guard")
}

pub fn open_or_create_pid_lock_file(path: &Path) -> std::io::Result<(fs::File, bool)> {
    match private_create_new_lock_file(path) {
        Ok(file) => Ok((file, true)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            private_open_existing_lock_file(path).map(|file| (file, false))
        }
        Err(error) => Err(error),
    }
}

pub fn publish_pid_lock_metadata(path: &Path, payload: &Value) -> Result<bool> {
    for attempt in 0..3 {
        let (mut file, created) = open_or_create_pid_lock_file(path)
            .with_context(|| format!("open ctx process lock metadata {}", path.display()))?;
        secure_private_file_permissions(path)?;
        let previous = (!created).then(|| read_pid_lock_json(path)).flatten();
        if !created
            && !previous
                .as_ref()
                .is_some_and(pid_lock_uses_advisory_protocol)
            && !legacy_pid_lock_value_is_stale(path, previous.as_ref())
        {
            return Ok(false);
        }
        write_pid_lock_json(&mut file, payload)
            .with_context(|| format!("publish ctx process lock metadata {}", path.display()))?;
        if pid_lock_path_has_owner(path, payload) {
            return Ok(true);
        }
        if attempt < 2 {
            std::thread::sleep(PID_LOCK_ACQUIRE_RETRY);
        }
    }
    Ok(false)
}

pub fn pid_lock_payload(extra: Value) -> Value {
    let mut payload = json!({
        "lock_protocol": PID_LOCK_PROTOCOL,
        "owner_id": Uuid::now_v7().to_string(),
        "pid": process::id(),
        "released": false,
        "started_at_ms": utc_now().timestamp_millis(),
    });
    if let (Some(payload), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
        payload.extend(extra.clone());
    }
    payload
}

pub fn daemon_lock_is_stale(path: &Path) -> bool {
    pid_lock_file_is_stale(path)
}

pub fn daemon_lock_is_active(data_root: &Path) -> bool {
    let path = daemon_lock_path(data_root);
    path.exists() && !daemon_lock_is_stale(&path)
}

pub fn pid_lock_file_is_active(path: &Path) -> bool {
    path.exists() && !pid_lock_file_is_stale(path)
}

pub fn daemon_lock_is_owned_by(data_root: &Path, pid: u32) -> bool {
    let path = daemon_lock_path(data_root);
    read_pid_lock_file(&path) == Some(pid) && !daemon_lock_is_stale(&path)
}

/// Marks only an exact reaped daemon owner as released and removes only its
/// endpoint identities. A replacement which acquires ownership first makes
/// this a no-op.
pub fn cleanup_reaped_daemon_owner(
    data_root: &Path,
    owner_pid: u32,
    owner_id: &str,
    endpoint_paths: &[PathBuf],
) -> Result<bool> {
    let Some(_quiescence) = DaemonQuiescenceGuard::acquire(data_root)? else {
        return Ok(false);
    };
    let lock_path = daemon_lock_path(data_root);
    let Some(mut lock) = read_pid_lock_json(&lock_path) else {
        return Ok(false);
    };
    if !pid_lock_uses_advisory_protocol(&lock)
        || pid_from_lock_json(&lock) != Some(owner_pid)
        || lock.get("owner_id").and_then(Value::as_str) != Some(owner_id)
    {
        return Ok(false);
    }
    if let Some(object) = lock.as_object_mut() {
        object.insert("released".to_owned(), Value::Bool(true));
    }
    if !publish_pid_lock_metadata(&lock_path, &lock)? {
        return Ok(false);
    }
    for endpoint_path in endpoint_paths {
        let identity = crate::read_daemon_service_endpoint_identity_at(endpoint_path)?;
        let Some(identity) = identity.filter(|identity| identity.owner_pid == owner_pid) else {
            continue;
        };
        #[cfg(unix)]
        {
            let crate::DaemonQueryEndpoint::Unix { path, .. } = identity.endpoint;
            remove_reaped_artifact(&path)
                .with_context(|| format!("remove reaped daemon socket {}", path.display()))?;
        }
        remove_reaped_artifact(endpoint_path).with_context(|| {
            format!(
                "remove reaped daemon endpoint identity {}",
                endpoint_path.display()
            )
        })?;
    }
    Ok(true)
}

fn remove_reaped_artifact(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn pid_lock_file_is_stale(path: &Path) -> bool {
    if let Some(observation) = observe_pid_advisory_lock(path) {
        return !observation.held;
    }
    let value = read_pid_lock_json(path);
    legacy_pid_lock_value_is_stale(path, value.as_ref())
}

pub fn pid_lock_file_is_orphaned(path: &Path) -> bool {
    if let Some(observation) = observe_pid_advisory_lock(path) {
        return !observation.held && !observation.released;
    }
    let value = read_pid_lock_json(path);
    legacy_pid_lock_value_is_stale(path, value.as_ref())
}

pub fn legacy_pid_lock_value_is_stale(path: &Path, value: Option<&Value>) -> bool {
    let Some(value) = value else {
        return incomplete_pid_lock_is_stale(path);
    };
    let Some(pid) = pid_from_lock_json(value) else {
        return incomplete_pid_lock_is_stale(path);
    };
    match process_state(pid) {
        ProcessState::Running => false,
        ProcessState::NotRunning => true,
        ProcessState::Unknown => lock_started_at_is_stale(value),
    }
}

pub fn incomplete_pid_lock_is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > PID_LOCK_INCOMPLETE_GRACE)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PidAdvisoryLockObservation {
    pub held: bool,
    pub released: bool,
}

pub fn observe_pid_advisory_lock(path: &Path) -> Option<PidAdvisoryLockObservation> {
    let guard = private_open_existing_lock_file(&pid_lock_guard_path(path)).ok()?;
    match fs2::FileExt::try_lock_shared(&guard) {
        Ok(()) => {
            let observation = read_pid_lock_json(path)
                .filter(pid_lock_uses_advisory_protocol)
                .map(|value| PidAdvisoryLockObservation {
                    held: false,
                    released: value
                        .get("released")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                });
            let _ = fs2::FileExt::unlock(&guard);
            observation
        }
        Err(error) if pid_lock_error_is_contended(&error) => {
            let released = read_pid_lock_json(path)
                .filter(pid_lock_uses_advisory_protocol)
                .and_then(|value| value.get("released").and_then(Value::as_bool))
                .unwrap_or(false);
            Some(PidAdvisoryLockObservation {
                held: true,
                released,
            })
        }
        Err(_) => None,
    }
}

fn pid_lock_error_is_contended(error: &std::io::Error) -> bool {
    pid_lock_error_is_contended_on(error, cfg!(windows))
}

fn pid_lock_error_is_contended_on(error: &std::io::Error, windows: bool) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        // fs2 forwards LockFileEx's ERROR_LOCK_VIOLATION without mapping it
        // to WouldBlock. It is the exact nonblocking contention result.
        || (windows
            && error.raw_os_error() == Some(crate::WINDOWS_ERROR_LOCK_VIOLATION))
}

pub fn try_lock_pid_file(file: &fs::File) -> std::io::Result<bool> {
    for attempt in 0..PID_LOCK_ACQUIRE_ATTEMPTS {
        match fs2::FileExt::try_lock_exclusive(file) {
            Ok(()) => return Ok(true),
            Err(error) if pid_lock_error_is_contended(&error) => {
                if attempt + 1 < PID_LOCK_ACQUIRE_ATTEMPTS {
                    std::thread::sleep(PID_LOCK_ACQUIRE_RETRY);
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

pub fn pid_lock_path_has_owner(path: &Path, payload: &Value) -> bool {
    let owner_id = payload.get("owner_id").and_then(Value::as_str);
    owner_id.is_some()
        && read_pid_lock_json(path)
            .as_ref()
            .and_then(|value| value.get("owner_id"))
            .and_then(Value::as_str)
            == owner_id
}

pub fn pid_lock_uses_advisory_protocol(value: &Value) -> bool {
    value.get("lock_protocol").and_then(Value::as_str) == Some(PID_LOCK_PROTOCOL)
}

pub fn pid_lock_file_reports_running(
    path: &Path,
    lock_state: Option<ProcessState>,
    status: &str,
) -> bool {
    if let Some(observation) = observe_pid_advisory_lock(path) {
        return observation.held;
    }
    matches!(lock_state, Some(ProcessState::Running))
        || unknown_process_lock_reports_running(path, lock_state, status)
}

pub fn read_pid_lock_file(path: &Path) -> Option<u32> {
    read_pid_lock_json(path).and_then(|value| pid_from_lock_json(&value))
}

pub fn read_pid_lock_json(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn write_pid_lock_json(file: &mut fs::File, value: &Value) -> Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    serde_json::to_writer(&mut *file, value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

pub fn pid_from_lock_json(value: &Value) -> Option<u32> {
    value
        .get("pid")
        .and_then(|value| value.as_u64())
        .and_then(|pid| u32::try_from(pid).ok())
}

pub fn lock_started_at_is_stale(value: &Value) -> bool {
    let Some(started_at_ms) = json_i64(value, "started_at_ms") else {
        return false;
    };
    utc_now().timestamp_millis().saturating_sub(started_at_ms) > DAEMON_LOCK_STALE_AFTER_MS
}

pub fn unknown_process_lock_reports_running(
    lock_path: &Path,
    state: Option<ProcessState>,
    status: &str,
) -> bool {
    matches!(state, Some(ProcessState::Unknown))
        && status == "running"
        && lock_path.exists()
        && !pid_lock_file_is_stale(lock_path)
}

fn json_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_lock_violation_is_lock_contention_for_acquisition_and_observation() {
        let error = std::io::Error::from_raw_os_error(crate::WINDOWS_ERROR_LOCK_VIOLATION);

        assert!(pid_lock_error_is_contended_on(&error, true));
        assert!(!pid_lock_error_is_contended_on(&error, false));
        assert!(pid_lock_error_is_contended_on(
            &std::io::Error::from(std::io::ErrorKind::WouldBlock),
            false,
        ));
    }

    #[test]
    fn held_pid_lock_returns_contention_and_succeeds_after_unlock() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("pid.guard");
        let (owner, _) = open_or_create_pid_lock_file(&path)?;
        let (waiter, _) = open_or_create_pid_lock_file(&path)?;

        assert!(try_lock_pid_file(&owner)?);
        assert!(!try_lock_pid_file(&waiter)?);

        fs2::FileExt::unlock(&owner)?;
        assert!(try_lock_pid_file(&waiter)?);
        fs2::FileExt::unlock(&waiter)?;
        Ok(())
    }

    #[test]
    fn quiescence_guard_excludes_daemon_replacement_until_cleanup_finishes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let quiescence = DaemonQuiescenceGuard::acquire(temp.path())?.expect("quiescence guard");
        assert!(DaemonLock::acquire(temp.path())?.is_none());
        drop(quiescence);
        assert!(DaemonLock::acquire(temp.path())?.is_some());
        Ok(())
    }

    #[test]
    fn reaped_cleanup_rejects_a_same_pid_replacement_owner() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let data_root = temp.path();
        ctx_history_platform::platform_security::establish_private_data_root(data_root)?;
        create_private_dir_all(&daemon_root_path(data_root))?;
        let lock_path = daemon_lock_path(data_root);
        let payload = pid_lock_payload(json!({}));
        let pid = pid_from_lock_json(&payload).expect("test lock pid");
        let owner_id = payload["owner_id"].as_str().expect("test owner id");
        assert!(publish_pid_lock_metadata(&lock_path, &payload)?);

        assert!(!cleanup_reaped_daemon_owner(
            data_root,
            pid,
            "replacement-owner",
            &[],
        )?);
        let unchanged = read_pid_lock_json(&lock_path).expect("unchanged lock");
        assert_eq!(unchanged["owner_id"], owner_id);
        assert_eq!(unchanged["released"], false);

        assert!(cleanup_reaped_daemon_owner(data_root, pid, owner_id, &[],)?);
        let released = read_pid_lock_json(&lock_path).expect("released lock");
        assert_eq!(released["owner_id"], owner_id);
        assert_eq!(released["released"], true);
        Ok(())
    }
}
