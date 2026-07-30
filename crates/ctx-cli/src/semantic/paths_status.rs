pub(super) fn daemon_root_path(data_root: &Path) -> PathBuf {
    data_root.join(DAEMON_DIR)
}

pub(super) fn daemon_jobs_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_JOBS_DIR)
}

pub(super) fn daemon_lock_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_LOCK_FILE)
}

pub(super) fn daemon_status_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_STATUS_FILE)
}

#[cfg(unix)]
pub(super) fn daemon_query_socket_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_QUERY_SOCKET_FILE)
}

pub(super) fn daemon_source_backed_refresh_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join("core-refresh.json")
}

pub(super) fn daemon_semantic_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(DAEMON_SEMANTIC_JOB_FILE)
}

pub(super) struct DaemonLock {
    _inner: PidFileLock,
}

impl DaemonLock {
    pub(super) fn acquire(data_root: &Path) -> Result<Option<Self>> {
        ctx_history_core::platform_security::establish_private_data_root(data_root)?;
        create_private_dir_all(&daemon_root_path(data_root))?;
        let payload = current_daemon_lock_identity(data_root)?;
        Ok(PidFileLock::acquire(&daemon_lock_path(data_root), payload)?
            .map(|inner| Self { _inner: inner }))
    }
}

pub(super) struct PidFileLock {
    guard: fs::File,
    path: PathBuf,
    payload: Value,
}

impl PidFileLock {
    pub(super) fn acquire(path: &Path, payload: Value) -> Result<Option<Self>> {
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

pub(super) fn pid_lock_guard_path(path: &Path) -> PathBuf {
    path.with_extension("guard")
}

pub(super) fn open_or_create_pid_lock_file(path: &Path) -> std::io::Result<(fs::File, bool)> {
    match private_create_new_lock_file(path) {
        Ok(file) => Ok((file, true)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            private_open_existing_lock_file(path).map(|file| (file, false))
        }
        Err(error) => Err(error),
    }
}

pub(super) fn publish_pid_lock_metadata(path: &Path, payload: &Value) -> Result<bool> {
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

pub(super) fn pid_lock_payload(extra: Value) -> Value {
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

pub(super) fn daemon_lock_is_stale(path: &Path) -> bool {
    pid_lock_file_is_stale(path)
}

pub(super) fn daemon_lock_is_active(data_root: &Path) -> bool {
    let path = daemon_lock_path(data_root);
    path.exists() && !daemon_lock_is_stale(&path)
}

pub(super) fn daemon_lock_is_owned_by(data_root: &Path, pid: u32) -> bool {
    let path = daemon_lock_path(data_root);
    read_pid_lock_file(&path) == Some(pid) && !daemon_lock_is_stale(&path)
}

pub(super) fn pid_lock_file_is_stale(path: &Path) -> bool {
    if let Some(observation) = observe_pid_advisory_lock(path) {
        return !observation.held;
    }
    let value = read_pid_lock_json(path);
    legacy_pid_lock_value_is_stale(path, value.as_ref())
}

pub(super) fn pid_lock_file_is_orphaned(path: &Path) -> bool {
    if let Some(observation) = observe_pid_advisory_lock(path) {
        return !observation.held && !observation.released;
    }
    let value = read_pid_lock_json(path);
    legacy_pid_lock_value_is_stale(path, value.as_ref())
}

pub(super) fn legacy_pid_lock_value_is_stale(path: &Path, value: Option<&Value>) -> bool {
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

pub(super) fn incomplete_pid_lock_is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > PID_LOCK_INCOMPLETE_GRACE)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PidAdvisoryLockObservation {
    pub(super) held: bool,
    pub(super) released: bool,
}

pub(super) fn observe_pid_advisory_lock(path: &Path) -> Option<PidAdvisoryLockObservation> {
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
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            Some(PidAdvisoryLockObservation {
                held: true,
                released: false,
            })
        }
        Err(_) => None,
    }
}

pub(super) fn try_lock_pid_file(file: &fs::File) -> std::io::Result<bool> {
    for attempt in 0..PID_LOCK_ACQUIRE_ATTEMPTS {
        match fs2::FileExt::try_lock_exclusive(file) {
            Ok(()) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if attempt + 1 < PID_LOCK_ACQUIRE_ATTEMPTS {
                    std::thread::sleep(PID_LOCK_ACQUIRE_RETRY);
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

pub(super) fn pid_lock_path_has_owner(path: &Path, payload: &Value) -> bool {
    let owner_id = payload.get("owner_id").and_then(Value::as_str);
    owner_id.is_some()
        && read_pid_lock_json(path)
            .as_ref()
            .and_then(|value| value.get("owner_id"))
            .and_then(Value::as_str)
            == owner_id
}

pub(super) fn pid_lock_uses_advisory_protocol(value: &Value) -> bool {
    value.get("lock_protocol").and_then(Value::as_str) == Some(PID_LOCK_PROTOCOL)
}

pub(super) fn pid_lock_file_reports_running(
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

pub(super) fn read_pid_lock_file(path: &Path) -> Option<u32> {
    read_pid_lock_json(path).and_then(|value| pid_from_lock_json(&value))
}

pub(super) fn read_pid_lock_json(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub(super) fn write_pid_lock_json(file: &mut fs::File, value: &Value) -> Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    serde_json::to_writer(&mut *file, value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

pub(super) fn pid_from_lock_json(value: &Value) -> Option<u32> {
    value
        .get("pid")
        .and_then(|value| value.as_u64())
        .and_then(|pid| u32::try_from(pid).ok())
}

pub(super) fn lock_started_at_is_stale(value: &Value) -> bool {
    let Some(started_at_ms) = json_i64(value, "started_at_ms") else {
        return false;
    };
    utc_now().timestamp_millis().saturating_sub(started_at_ms) > DAEMON_LOCK_STALE_AFTER_MS
}

pub(super) fn unknown_process_lock_reports_running(
    lock_path: &Path,
    state: Option<ProcessState>,
    status: &str,
) -> bool {
    matches!(state, Some(ProcessState::Unknown))
        && status == "running"
        && lock_path.exists()
        && !pid_lock_file_is_stale(lock_path)
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn lower_semantic_worker_priority() {
    unsafe {
        let _ = libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

#[cfg(target_os = "macos")]
pub(super) fn lower_semantic_worker_priority() {
    unsafe {
        let _ = libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}

#[cfg(not(unix))]
pub(super) fn lower_semantic_worker_priority() {}

pub(super) fn write_private_json_file(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent)?;
    }
    let (tmp_path, mut file) = (0..PRIVATE_JSON_TEMP_ATTEMPTS)
        .find_map(|_| {
            let sequence = PRIVATE_JSON_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let tmp_path = path.with_extension(format!("json.{}.{}.tmp", process::id(), sequence));
            match private_create_new_file(&tmp_path) {
                Ok(file) => Some(Ok((tmp_path, file))),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .transpose()?
        .with_context(|| format!("allocate private status file beside {}", path.display()))?;
    let write_result = (|| -> Result<()> {
        file.write_all(&serde_json::to_vec_pretty(value)?)
            .with_context(|| format!("write private status file {}", tmp_path.display()))?;
        file.write_all(b"\n")
            .with_context(|| format!("write private status file {}", tmp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("sync private status file {}", tmp_path.display()))?;
        Ok(())
    })();
    drop(file);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&tmp_path);
        return Err(error);
    }
    if let Err(error) = replace_private_file(&tmp_path, path)
        .with_context(|| format!("replace private status file {}", path.display()))
    {
        let _ = fs::remove_file(&tmp_path);
        return Err(error);
    }
    secure_private_file_permissions(path)?;
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn replace_private_file(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
pub(super) fn replace_private_file(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    retry_windows_private_file_replacement(
        || {
            let moved = unsafe {
                MoveFileExW(
                    source.as_ptr(),
                    target.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            };
            if moved == 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        },
        || std::thread::sleep(PRIVATE_FILE_REPLACE_RETRY),
    )
}

#[cfg(any(test, windows))]
fn retry_windows_private_file_replacement(
    mut replace: impl FnMut() -> std::io::Result<()>,
    mut wait: impl FnMut(),
) -> std::io::Result<()> {
    for attempt in 1..=PRIVATE_FILE_REPLACE_ATTEMPTS {
        match replace() {
            Ok(()) => return Ok(()),
            Err(error)
                if windows_file_replacement_error_is_retryable(&error)
                    && attempt < PRIVATE_FILE_REPLACE_ATTEMPTS =>
            {
                // Virus scanners and indexers can briefly open a newly
                // published status file without delete sharing. Keep atomic
                // replacement semantics while allowing that handle to close.
                wait();
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the bounded replacement loop always returns")
}

#[cfg(any(test, windows))]
fn windows_file_replacement_error_is_retryable(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(
            WINDOWS_ERROR_ACCESS_DENIED
                | WINDOWS_ERROR_SHARING_VIOLATION
                | WINDOWS_ERROR_LOCK_VIOLATION
        )
    )
}

pub(super) fn write_daemon_status(data_root: &Path, value: &Value) -> Result<()> {
    write_private_json_file(&daemon_status_path(data_root), value)
}

pub(super) fn read_daemon_status(data_root: &Path) -> Option<Value> {
    let text = fs::read_to_string(daemon_status_path(data_root)).ok()?;
    serde_json::from_str(&text).ok()
}

pub(super) fn write_daemon_job_status(path: &Path, value: &Value) -> Result<()> {
    write_private_json_file(path, value)
}

pub(super) fn read_daemon_job_status(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub(super) fn daemon_report(data_root: &Path) -> Value {
    daemon_report_with_disabled_status(data_root, true)
}
pub(super) fn daemon_report_with_disabled_status(
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
) -> Value {
    let status_value = read_daemon_status(data_root);
    let current_config = AppConfig::load(data_root).ok();
    let enabled = current_config
        .as_ref()
        .map(|config| config.daemon.enabled)
        .unwrap_or_else(|| AppConfig::default().daemon.enabled);
    let daemon_mode = current_config
        .as_ref()
        .map(|config| config.daemon.mode)
        .or_else(|| {
            status_value
                .as_ref()
                .and_then(|status| status.get("config_reload"))
                .and_then(|reload| reload.get("applied"))
                .and_then(|applied| applied.get("daemon_mode"))
                .and_then(Value::as_str)
                .and_then(crate::config::DaemonMode::parse)
        })
        .unwrap_or_default();
    let lock_path = daemon_lock_path(data_root);
    let status_path = daemon_status_path(data_root);
    let lock_value = read_pid_lock_json(&lock_path);
    let lock_pid = read_pid_lock_file(&lock_path);
    let mut status = status_value
        .as_ref()
        .and_then(|value| json_string(value, "status"))
        .unwrap_or_else(|| "unknown".to_owned());
    let lock_state = lock_pid.map(process_state);
    let lock_reports_running =
        pid_lock_file_reports_running(&lock_path, lock_state, status.as_str());
    let owner_identity_matches = lock_reports_running
        && lock_value.as_ref().is_some_and(|identity| {
            crate::upgrade::installation_executable_path()
                .ok()
                .and_then(|executable| {
                    daemon_owner_binary_identity_matches(identity, &executable).ok()
                })
                .unwrap_or(false)
        });
    let owner_identity_mismatch = lock_reports_running && !owner_identity_matches;
    let running = lock_reports_running && owner_identity_matches;
    let stale_lock = lock_path.exists() && pid_lock_file_is_orphaned(&lock_path);
    let stale_lock_overrides_lifecycle = (stale_lock || owner_identity_mismatch)
        && !["completed", "failed"].contains(&status.as_str());
    let stale_running_status = !running && status == "running";
    if running {
        status = "running".to_owned();
    } else if stale_lock_overrides_lifecycle || stale_running_status {
        status = "stale_lock".to_owned();
    } else if !enabled && (disabled_overrides_lifecycle || status == "unknown") {
        status = "disabled".to_owned();
    }
    let pid = if running {
        lock_pid
    } else {
        status_value
            .as_ref()
            .and_then(|value| json_u32(value, "pid"))
    };
    let config_reload = daemon_config_reload_report(data_root, status_value.as_ref(), running);
    let semantic_runtime_active = running
        && status_value
            .as_ref()
            .and_then(|value| value.get("semantic_runtime_active"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let start_mode = status_value
        .as_ref()
        .and_then(|value| json_string(value, "start_mode"));
    let trigger_command = status_value
        .as_ref()
        .and_then(|value| json_string(value, "trigger_command"));
    let trigger_provenance = if start_mode.as_deref() == Some("auto") {
        Some("autostart".to_owned())
    } else {
        trigger_command
            .clone()
            .or_else(|| Some("manual".to_owned()))
    };
    let suppressed_job = |kind: &str, reason: &str| {
        compact_json(json!({
            "status": "disabled",
            "enabled": false,
            "kind": kind,
            "reason": reason,
            "daemon_mode": daemon_mode.as_str(),
        }))
    };
    let legacy_history_reason = if daemon_mode.runs_only_source_refresh() {
        "daemon_mode_source_refresh_only"
    } else {
        "history_epoch_source_backed"
    };
    let jobs = json!({
        "history_refresh": suppressed_job("history_refresh", legacy_history_reason),
        "source_backed_refresh": daemon_source_backed_refresh_job_report(
            data_root,
            disabled_overrides_lifecycle
        ),
        "semantic_index": daemon_semantic_job_report(
            data_root,
            daemon_mode,
            disabled_overrides_lifecycle,
            running,
            semantic_runtime_active,
            &config_reload,
        ),
    });
    let lock_identity = compact_json(json!({
        "path": lock_path,
        "active": running,
        "owner_id": lock_value
            .as_ref()
            .and_then(|value| json_string(value, "owner_id")),
        "pid": lock_pid,
        "binary": lock_value
            .as_ref()
            .and_then(|value| json_string(value, "binary")),
        "binary_sha256": lock_value
            .as_ref()
            .and_then(|value| json_string(value, "binary_sha256")),
        "owner_image_matches": owner_identity_matches,
        "protocol": lock_value
            .as_ref()
            .and_then(|value| json_string(value, "lock_protocol")),
    }));
    compact_json(json!({
        "status": status,
        "enabled": enabled,
        "mode": daemon_mode.as_str(),
        "running": running,
        "recoverable": stale_lock_overrides_lifecycle || stale_running_status,
        "reason": if owner_identity_mismatch {
            Some("daemon_owner_identity_mismatch".to_owned())
        } else if stale_lock_overrides_lifecycle {
            Some("daemon_lock_stale".to_owned())
        } else if stale_running_status {
            Some("daemon_status_stale".to_owned())
        } else {
            status_value
                .as_ref()
                .and_then(|value| json_string(value, "reason"))
        },
        "pid": pid,
        "live_pid": running.then_some(pid).flatten(),
        "started_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "started_at_ms")),
        "heartbeat_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "heartbeat_at_ms")),
        "finished_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "finished_at_ms")),
        "start_mode": start_mode,
        "trigger_command": trigger_command,
        "trigger_provenance": trigger_provenance,
        "last_error": status_value.as_ref().and_then(|value| json_string(value, "last_error")),
        "semantic_runtime_active": semantic_runtime_active,
        "config_reload": config_reload,
        "lock_path": lock_path,
        "lock_identity": lock_identity,
        "source_refresh_endpoint": daemon_source_refresh_endpoint_report(data_root),
        "supervisor": super::daemon_supervisor::daemon_supervisor_report(data_root),
        "wakeup": super::daemon_wakeup::daemon_wakeup_report(data_root),
        "status_path": status_path,
        "jobs": jobs,
    }))
}

fn daemon_semantic_job_report(
    data_root: &Path,
    daemon_mode: crate::config::DaemonMode,
    disabled_overrides_lifecycle: bool,
    daemon_running: bool,
    semantic_runtime_active: bool,
    config_reload: &Value,
) -> Value {
    let requested_daemon_enabled = config_reload
        .pointer("/requested/daemon_enabled")
        .and_then(Value::as_bool);
    let requested_semantic_enabled = config_reload
        .pointer("/requested/semantic_enabled")
        .and_then(Value::as_bool);
    let applied_daemon_enabled = config_reload
        .pointer("/applied/daemon_enabled")
        .and_then(Value::as_bool);
    let applied_semantic_enabled = config_reload
        .pointer("/applied/semantic_enabled")
        .and_then(Value::as_bool);
    let daemon_enabled = requested_daemon_enabled
        .or(applied_daemon_enabled)
        .unwrap_or_else(|| daemon_enabled_for_status(data_root));
    let semantic_enabled = requested_semantic_enabled
        .or(applied_semantic_enabled)
        .unwrap_or_else(|| {
            AppConfig::load(data_root)
                .map(|config| config.semantic_search_enabled())
                .unwrap_or(false)
        });
    let semantic_supported = super::semantic_query_service_supported();
    let mode_allows_semantic = !daemon_mode.runs_only_source_refresh();
    let enabled = daemon_enabled && semantic_enabled && semantic_supported && mode_allows_semantic;
    let config_reload_status = config_reload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let config_out_of_sync = config_reload
        .get("out_of_sync")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let activation_failed = config_reload_status == "activation_failed" && semantic_enabled;
    let reload_pending = daemon_running && config_reload_status == "pending" && config_out_of_sync;
    let disabled = !enabled && disabled_overrides_lifecycle && !semantic_runtime_active;
    let status_value = read_daemon_job_status(&daemon_semantic_job_path(data_root));
    let last_run_status = status_value
        .as_ref()
        .and_then(|value| json_string(value, "status"));
    let last_run_reason = status_value
        .as_ref()
        .and_then(|value| json_string(value, "reason"));
    let status = if activation_failed {
        "failed"
    } else if reload_pending || (daemon_running && enabled && !semantic_runtime_active) {
        "pending"
    } else if disabled {
        "disabled"
    } else {
        last_run_status.as_deref().unwrap_or("unknown")
    };
    let reason = if activation_failed {
        Some("semantic_activation_failed".to_owned())
    } else if reload_pending {
        Some("daemon_config_reload_pending".to_owned())
    } else if daemon_running && enabled && !semantic_runtime_active {
        Some("semantic_runtime_inactive".to_owned())
    } else if disabled {
        Some(if daemon_mode.runs_only_source_refresh() {
            "daemon_mode_source_refresh_only".to_owned()
        } else if !semantic_enabled {
            "semantic_disabled".to_owned()
        } else if !semantic_supported {
            "unsupported_platform".to_owned()
        } else {
            "daemon_disabled".to_owned()
        })
    } else {
        last_run_reason.clone()
    };
    compact_json(json!({
        "status": status,
        "enabled": enabled,
        "semantic_enabled": semantic_enabled,
        "daemon_configured": applied_daemon_enabled,
        "semantic_configured": applied_semantic_enabled,
        "runtime_active": semantic_runtime_active,
        "config_reload_status": config_reload_status,
        "configuration_pending": reload_pending,
        "reason": reason,
        "last_run_at_ms": status_value
            .as_ref()
            .and_then(|value| json_i64(value, "last_run_at_ms")),
        "last_run_status": last_run_status,
        "last_run_reason": last_run_reason,
        "last_error": if activation_failed {
            config_reload
                .get("last_error")
                .and_then(Value::as_str)
                .map(str::to_owned)
        } else {
            status_value
                .as_ref()
                .and_then(|value| json_string(value, "last_error"))
        },
        "retryable": status_value
            .as_ref()
            .and_then(|value| value.get("retryable").and_then(Value::as_bool)),
        "failure_class": status_value
            .as_ref()
            .and_then(|value| json_string(value, "failure_class")),
        "indexed_chunks": status_value
            .as_ref()
            .and_then(|value| value.get("indexed_chunks").and_then(Value::as_u64)),
        "model_key": status_value
            .as_ref()
            .and_then(|value| json_string(value, "model_key")),
        "daemon_mode": daemon_mode.as_str(),
    }))
}

pub(super) fn daemon_source_backed_refresh_job_report(
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
) -> Value {
    let daemon_enabled = daemon_enabled_for_status(data_root);
    let status_value = read_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root));
    let job = status_value.as_ref();
    let disabled = !daemon_enabled && disabled_overrides_lifecycle;
    compact_json(json!({
        "status": if disabled {
            "disabled"
        } else {
            job.and_then(|value| value.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        },
        "enabled": daemon_enabled,
        "reason": if disabled {
            Some("daemon_disabled".to_owned())
        } else {
            job.and_then(|value| json_string(value, "reason"))
        },
        "error_code": job.and_then(|value| json_string(value, "error_code")),
        "mode": job.and_then(|value| json_string(value, "mode")),
        "owner": job.and_then(|value| json_string(value, "owner")),
        "kind": job.and_then(|value| json_string(value, "kind")),
        "request_id": job.and_then(|value| json_string(value, "request_id")),
        "request_state": job.and_then(|value| json_string(value, "request_state")),
        "last_run_at_ms": job.and_then(|value| json_i64(value, "last_run_at_ms")),
        "source_count": job.and_then(|value| value.get("source_count").cloned()),
        "previous_generation": job
            .and_then(|value| json_string(value, "previous_generation")),
        "published_generation": job
            .and_then(|value| json_string(value, "published_generation")),
        "generation_changed": job
            .and_then(|value| value.get("generation_changed").cloned()),
        "receipt": job.and_then(|value| value.get("receipt").cloned()),
        "coalesced_requests": job
            .and_then(|value| value.get("coalesced_requests").cloned()),
        "progress": job.and_then(|value| value.get("progress").cloned()),
        "daemon_mode": job.and_then(|value| json_string(value, "daemon_mode")),
        "trigger": job.and_then(|value| json_string(value, "trigger")),
        "trigger_provenance": job
            .and_then(|value| json_string(value, "trigger_provenance")),
        "scanned_routes": job.and_then(|value| value.get("scanned_routes").cloned()),
        "unsupported_routes": job
            .and_then(|value| value.get("unsupported_routes").cloned()),
        "certified_source_count": job
            .and_then(|value| value.get("certified_source_count").cloned()),
        "certified_source_bytes": job
            .and_then(|value| value.get("certified_source_bytes").cloned()),
        "timings_us": job.and_then(|value| value.get("timings_us").cloned()),
        "retryable": job.and_then(|value| value.get("retryable").cloned()),
        "retry_after_ms": job.and_then(|value| value.get("retry_after_ms").cloned()),
        "consecutive_failures": job
            .and_then(|value| value.get("consecutive_failures").cloned()),
        "retry_not_before_at_ms": job
            .and_then(|value| value.get("retry_not_before_at_ms").cloned()),
        "last_error": job.and_then(|value| json_string(value, "last_error")),
    }))
}

fn daemon_source_refresh_endpoint_report(data_root: &Path) -> Value {
    let identity_path = daemon_root_path(data_root).join("source-refresh-endpoint.json");
    let identity = fs::read_to_string(&identity_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    compact_json(json!({
        "identity_path": identity_path,
        "available": identity.is_some(),
        "transport": identity
            .as_ref()
            .and_then(|value| json_string(value, "transport")),
        "owner_pid": identity.as_ref().and_then(|value| json_u32(value, "pid")),
        "address": identity.as_ref().and_then(|value| {
            json_string(value, "path").or_else(|| json_string(value, "pipe_name"))
        }),
    }))
}

pub(super) fn daemon_enabled_for_status(data_root: &Path) -> bool {
    AppConfig::load(data_root)
        .map(|config| config.daemon.enabled)
        .unwrap_or_else(|_| AppConfig::default().daemon.enabled)
}

fn daemon_config_reload_report(
    data_root: &Path,
    daemon_status: Option<&Value>,
    running: bool,
) -> Value {
    let current_config = AppConfig::load(data_root).ok();
    let persisted = daemon_status
        .and_then(|value| value.get("config_reload"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let applied_daemon_enabled = persisted
        .get("applied")
        .and_then(|value| value.get("daemon_enabled"))
        .and_then(Value::as_bool);
    let applied_daemon_mode = persisted
        .get("applied")
        .and_then(|value| value.get("daemon_mode"))
        .and_then(Value::as_str);
    let applied_semantic_enabled = persisted
        .get("applied")
        .and_then(|value| value.get("semantic_enabled"))
        .and_then(Value::as_bool);
    let requested_daemon_enabled = current_config.as_ref().map(|config| config.daemon.enabled);
    let requested_daemon_mode = current_config
        .as_ref()
        .map(|config| config.daemon.mode.as_str());
    let requested_semantic_enabled = current_config
        .as_ref()
        .map(AppConfig::semantic_search_enabled);
    let out_of_sync = running
        && (requested_daemon_enabled != applied_daemon_enabled
            || requested_daemon_mode != applied_daemon_mode
            || requested_semantic_enabled != applied_semantic_enabled);
    let persisted_status = persisted
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let status = if out_of_sync && persisted_status == "applied" {
        "pending"
    } else {
        persisted_status
    };
    let reason = if out_of_sync && persisted_status == "applied" {
        Some("config_changed")
    } else {
        None
    };

    compact_json(json!({
        "status": status,
        "reason": reason,
        "out_of_sync": out_of_sync,
        "last_attempt_at_ms": persisted.get("last_attempt_at_ms").cloned(),
        "last_applied_at_ms": persisted.get("last_applied_at_ms").cloned(),
        "requested": {
            "daemon_enabled": requested_daemon_enabled,
            "daemon_mode": requested_daemon_mode,
            "semantic_enabled": requested_semantic_enabled,
        },
        "applied": {
            "daemon_enabled": applied_daemon_enabled,
            "daemon_mode": applied_daemon_mode,
            "semantic_enabled": applied_semantic_enabled,
        },
        "last_error": persisted.get("last_error").cloned(),
    }))
}

#[cfg(windows)]
use std::time::Duration;
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

use anyhow::{Context, Result};
use ctx_history_core::utc_now;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{compact_json, config::AppConfig};

use super::{
    health_search::{
        create_private_dir_all, json_i64, json_string, json_u32, private_create_new_file,
        private_create_new_lock_file, private_open_existing_lock_file,
        secure_private_file_permissions,
    },
    runtime_limits::{
        DAEMON_DIR, DAEMON_JOBS_DIR, DAEMON_LOCK_FILE, DAEMON_LOCK_STALE_AFTER_MS,
        DAEMON_SEMANTIC_JOB_FILE, DAEMON_STATUS_FILE, PID_LOCK_ACQUIRE_ATTEMPTS,
        PID_LOCK_ACQUIRE_RETRY, PID_LOCK_INCOMPLETE_GRACE, PID_LOCK_PROTOCOL,
    },
};

#[cfg(unix)]
use super::runtime_limits::DAEMON_QUERY_SOCKET_FILE;

mod binary_identity;
pub(super) use binary_identity::*;

const PRIVATE_JSON_TEMP_ATTEMPTS: usize = 16;
static PRIVATE_JSON_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
#[cfg(any(test, windows))]
const PRIVATE_FILE_REPLACE_ATTEMPTS: usize = 40;
#[cfg(windows)]
const PRIVATE_FILE_REPLACE_RETRY: Duration = Duration::from_millis(50);
#[cfg(any(test, windows))]
const WINDOWS_ERROR_ACCESS_DENIED: i32 = 5;
#[cfg(any(test, windows))]
const WINDOWS_ERROR_SHARING_VIOLATION: i32 = 32;
#[cfg(any(test, windows))]
const WINDOWS_ERROR_LOCK_VIOLATION: i32 = 33;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn durable_state_path_is_purpose_based() {
        assert_eq!(
            daemon_source_backed_refresh_job_path(Path::new("ctx-data")),
            Path::new("ctx-data/daemon/jobs/core-refresh.json")
        );
    }

    #[test]
    fn concurrent_status_writers_use_distinct_atomic_staging_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = Arc::new(temp.path().join("daemon/jobs/core-refresh.json"));
        let barrier = Arc::new(Barrier::new(16));
        let writers = (0..16)
            .map(|writer| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    for iteration in 0..32 {
                        write_private_json_file(
                            path.as_ref(),
                            &json!({"writer": writer, "iteration": iteration}),
                        )
                        .unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().unwrap();
        }

        let published: Value = serde_json::from_slice(&fs::read(path.as_ref()).unwrap()).unwrap();
        assert!(published["writer"].as_u64().is_some());
        assert!(published["iteration"].as_u64().is_some());
        assert!(fs::read_dir(path.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));
    }

    #[test]
    fn windows_private_file_replacement_retries_transient_lock_errors() {
        let errors = [
            WINDOWS_ERROR_ACCESS_DENIED,
            WINDOWS_ERROR_SHARING_VIOLATION,
            WINDOWS_ERROR_LOCK_VIOLATION,
        ];
        let mut attempts = 0;
        let mut waits = 0;
        retry_windows_private_file_replacement(
            || {
                let attempt = attempts;
                attempts += 1;
                if let Some(code) = errors.get(attempt) {
                    Err(std::io::Error::from_raw_os_error(*code))
                } else {
                    Ok(())
                }
            },
            || waits += 1,
        )
        .unwrap();

        assert_eq!(attempts, 4);
        assert_eq!(waits, 3);
    }

    #[test]
    fn windows_private_file_replacement_does_not_retry_other_errors() {
        let mut attempts = 0;
        let mut waits = 0;
        let error = retry_windows_private_file_replacement(
            || {
                attempts += 1;
                Err(std::io::Error::from_raw_os_error(2))
            },
            || waits += 1,
        )
        .unwrap_err();

        assert_eq!(error.raw_os_error(), Some(2));
        assert_eq!(attempts, 1);
        assert_eq!(waits, 0);
    }

    #[test]
    fn windows_private_file_replacement_has_a_bounded_retry_window() {
        let mut attempts = 0;
        let mut waits = 0;
        let error = retry_windows_private_file_replacement(
            || {
                attempts += 1;
                Err(std::io::Error::from_raw_os_error(
                    WINDOWS_ERROR_ACCESS_DENIED,
                ))
            },
            || waits += 1,
        )
        .unwrap_err();

        assert_eq!(error.raw_os_error(), Some(WINDOWS_ERROR_ACCESS_DENIED));
        assert_eq!(attempts, PRIVATE_FILE_REPLACE_ATTEMPTS);
        assert_eq!(waits, PRIVATE_FILE_REPLACE_ATTEMPTS - 1);
    }
}
