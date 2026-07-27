pub(super) fn semantic_vector_path(data_root: &Path) -> PathBuf {
    data_root.join("vectors.sqlite")
}

pub(super) fn semantic_worker_lock_path(data_root: &Path) -> PathBuf {
    data_root.join(SEMANTIC_WORKER_LOCK_FILE)
}

pub(super) fn semantic_worker_status_path(data_root: &Path) -> PathBuf {
    data_root.join(SEMANTIC_WORKER_STATUS_FILE)
}

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

pub(super) fn daemon_history_refresh_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(DAEMON_HISTORY_REFRESH_JOB_FILE)
}

pub(super) fn daemon_semantic_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(DAEMON_SEMANTIC_JOB_FILE)
}

pub(super) struct DaemonLock {
    _inner: PidFileLock,
}

impl DaemonLock {
    pub(super) fn acquire(data_root: &Path) -> Result<Option<Self>> {
        create_private_dir_all(data_root)?;
        let root = daemon_root_path(data_root);
        create_private_dir_all(&root)?;
        let payload = pid_lock_payload(json!({
            "binary": env::current_exe().ok(),
            "data_root": data_root,
        }));
        Ok(PidFileLock::acquire(&daemon_lock_path(data_root), payload)?
            .map(|inner| Self { _inner: inner }))
    }
}

pub(super) struct SemanticWorkerLock {
    _inner: PidFileLock,
}

impl SemanticWorkerLock {
    pub(super) fn acquire(data_root: &Path) -> Result<Option<Self>> {
        create_private_dir_all(data_root)?;
        Ok(PidFileLock::acquire(
            &semantic_worker_lock_path(data_root),
            pid_lock_payload(json!({})),
        )?
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
        // A legacy process may already be committed to unlinking this path and
        // cannot see the guard file. A live or incomplete legacy owner wins;
        // stale legacy metadata is reclaimable for supported upgrade handoff.
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProcessState {
    Running,
    NotRunning,
    Unknown,
}

#[cfg(unix)]
pub(super) fn process_state(pid: u32) -> ProcessState {
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
pub(super) fn process_state(pid: u32) -> ProcessState {
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
pub(super) fn process_state(_pid: u32) -> ProcessState {
    ProcessState::Unknown
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
    let tmp_path = path.with_extension(format!("json.{}.tmp", process::id()));
    if tmp_path.exists() {
        let _ = fs::remove_file(&tmp_path);
    }
    let mut file = private_create_new_file(&tmp_path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("write private status file {}", tmp_path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("write private status file {}", tmp_path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync private status file {}", tmp_path.display()))?;
    drop(file);
    fs::rename(&tmp_path, path)
        .with_context(|| format!("replace private status file {}", path.display()))?;
    secure_private_file_permissions(path)?;
    Ok(())
}

pub(super) fn write_semantic_worker_status(data_root: &Path, value: &Value) -> Result<()> {
    write_private_json_file(&semantic_worker_status_path(data_root), value)
}

pub(super) fn read_semantic_worker_status(data_root: &Path) -> Option<Value> {
    let text = fs::read_to_string(semantic_worker_status_path(data_root)).ok()?;
    serde_json::from_str(&text).ok()
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

pub(super) fn semantic_status_file_model_matches(status_value: Option<&Value>) -> bool {
    status_value
        .and_then(|value| json_string(value, "model_key"))
        .is_some_and(|model_key| model_key == semantic_model_key())
}

pub(super) fn semantic_status_file_searchable_items(status_value: Option<&Value>) -> Option<usize> {
    if !semantic_status_file_model_matches(status_value) {
        return None;
    }
    status_value.and_then(|value| json_usize(value, "searchable_items"))
}

pub(super) fn semantic_status_file_stats(status_value: Option<&Value>) -> SemanticSidecarStats {
    if !semantic_status_file_model_matches(status_value) {
        return SemanticSidecarStats::default();
    }
    SemanticSidecarStats {
        embedded_items: status_value
            .and_then(|value| json_usize(value, "embedded_items"))
            .unwrap_or(0),
        embedded_chunks: status_value
            .and_then(|value| json_usize(value, "embedded_chunks"))
            .unwrap_or(0),
    }
}

pub(crate) fn semantic_worker_report(
    data_root: &Path,
    store: Option<&Store>,
) -> Result<SemanticWorkerReport> {
    semantic_worker_report_with_count_mode(
        data_root,
        store,
        SemanticReportCountMode::ExactOnCacheMiss,
    )
}

pub(crate) fn semantic_worker_report_cached(
    data_root: &Path,
    store: Option<&Store>,
) -> Result<SemanticWorkerReport> {
    semantic_worker_report_with_count_mode(
        data_root,
        store,
        SemanticReportCountMode::CachedOrStatusFile,
    )
}

pub(super) fn semantic_worker_report_with_count_mode(
    data_root: &Path,
    store: Option<&Store>,
    count_mode: SemanticReportCountMode,
) -> Result<SemanticWorkerReport> {
    let status_value = read_semantic_worker_status(data_root);
    let status_file_model_matches = semantic_status_file_model_matches(status_value.as_ref());
    let current_status_value = status_value.as_ref().filter(|_| status_file_model_matches);
    let (searchable_items, searchable_items_known) = match store {
        Some(store) if count_mode == SemanticReportCountMode::ExactOnCacheMiss => (
            store.event_embedding_document_count_cached_or_exact()?,
            true,
        ),
        Some(store) => match store
            .cached_event_embedding_document_count()?
            .or_else(|| semantic_status_file_searchable_items(status_value.as_ref()))
        {
            Some(count) => (count, true),
            None => (0, false),
        },
        None => match semantic_status_file_searchable_items(status_value.as_ref()) {
            Some(count) => (count, true),
            None => (0, false),
        },
    };
    let vector_path = semantic_vector_path(data_root);
    let model_cache_available =
        semantic_model_cache_available(&semantic_worker_cache_dir(data_root));
    let sidecar_state_result = (|| -> Result<(SemanticSidecarStats, usize)> {
        if let Some(vector_store) = SemanticVectorStore::open_read_only(&vector_path)? {
            let dirty_items = vector_store.dirty_event_count()?;
            let mut stats = match count_mode {
                SemanticReportCountMode::ExactOnCacheMiss => {
                    vector_store.cached_or_exact_stats()?
                }
                SemanticReportCountMode::CachedOrStatusFile => vector_store
                    .cached_stats()?
                    .unwrap_or_else(|| semantic_status_file_stats(current_status_value)),
            };
            if count_mode == SemanticReportCountMode::ExactOnCacheMiss
                && semantic_status_needs_exact_sidecar_stats(searchable_items, dirty_items, stats)
            {
                stats = vector_store.exact_stats()?;
            }
            Ok((stats, dirty_items))
        } else if store.is_some() {
            Ok((SemanticSidecarStats::default(), 0))
        } else {
            Ok((semantic_status_file_stats(current_status_value), 0))
        }
    })();
    let (sidecar_stats, dirty_items, sidecar_error) = match sidecar_state_result {
        Ok((stats, dirty_items)) => (stats, dirty_items, None),
        Err(error) => (
            SemanticSidecarStats {
                embedded_items: 0,
                embedded_chunks: 0,
            },
            0,
            Some(format!("{error:#}")),
        ),
    };
    let embedded_items = sidecar_stats.embedded_items;
    let embedded_chunks = sidecar_stats.embedded_chunks;
    let status_path = semantic_worker_status_path(data_root);
    let lock_path = semantic_worker_lock_path(data_root);
    let lock_pid = read_pid_lock_file(&lock_path);
    let lock_state = lock_pid.map(process_state);
    let status_file_status = current_status_value.and_then(|value| json_string(value, "status"));
    let running = pid_lock_file_reports_running(
        &lock_path,
        lock_state,
        status_file_status.as_deref().unwrap_or("unknown"),
    );
    let pid = if running {
        lock_pid
    } else {
        current_status_value.and_then(|value| json_u32(value, "pid"))
    };
    let queued_items_estimate = searchable_items
        .saturating_sub(embedded_items)
        .max(dirty_items);
    let mut status = status_file_status.unwrap_or_else(|| {
        if !searchable_items_known || store.is_none() {
            "unknown".to_owned()
        } else if searchable_items == 0 {
            "empty".to_owned()
        } else if queued_items_estimate == 0 {
            "ready".to_owned()
        } else {
            "pending".to_owned()
        }
    });
    if store.is_some() {
        let live_status = if !searchable_items_known {
            "unknown".to_owned()
        } else if searchable_items == 0 {
            "empty".to_owned()
        } else if sidecar_error.is_some() {
            "unavailable".to_owned()
        } else if queued_items_estimate == 0 {
            "ready".to_owned()
        } else {
            "pending".to_owned()
        };
        let preserve_status = (status == "budget_exhausted" && queued_items_estimate > 0)
            || status == "acquiring_model"
            || status == "loading_model"
            || status == "model_load_deferred"
            || status == "resource_deferred"
            || status == "model_acquisition_failed"
            || status == "model_load_failed"
            || status == "model_integrity_failed"
            || (status == "failed"
                && sidecar_error.is_none()
                && embedded_items == 0
                && queued_items_estimate > 0);
        status = if preserve_status { status } else { live_status };
    }
    if running {
        status = "running".to_owned();
    } else if lock_path.exists()
        && pid_lock_file_is_orphaned(&lock_path)
        && queued_items_estimate > 0
    {
        status = "stale_lock".to_owned();
    }
    Ok(SemanticWorkerReport {
        status,
        running,
        pid,
        started_at_ms: current_status_value.and_then(|value| json_i64(value, "started_at_ms")),
        heartbeat_at_ms: current_status_value.and_then(|value| json_i64(value, "heartbeat_at_ms")),
        finished_at_ms: current_status_value.and_then(|value| json_i64(value, "finished_at_ms")),
        indexed_chunks: current_status_value.and_then(|value| json_usize(value, "indexed_chunks")),
        model_init_ms: current_status_value.and_then(|value| json_usize(value, "model_init_ms")),
        last_error: sidecar_error
            .or_else(|| current_status_value.and_then(|value| json_string(value, "last_error"))),
        searchable_items,
        searchable_items_known,
        embedded_items,
        embedded_chunks,
        dirty_items,
        queued_items_estimate,
        model_cache_available,
        model_acquisition: current_status_value
            .and_then(|value| value.get("model_acquisition").cloned())
            .unwrap_or_else(|| {
                semantic_model_acquisition_status_json(&semantic_worker_cache_dir(data_root))
            }),
        embed_policy: current_status_value
            .and_then(|value| value.get("embed_policy").cloned())
            .or_else(|| Some(semantic_embed_policy_status_json())),
        embedding_runtime: current_status_value
            .and_then(|value| value.get("embedding_runtime").cloned()),
        failure_class: current_status_value.and_then(|value| json_string(value, "failure_class")),
        resource_deferral: current_status_value
            .and_then(|value| value.get("resource_deferral").cloned()),
        vector_path,
        lock_path,
        status_path,
    })
}

pub(crate) fn semantic_worker_report_best_effort(data_root: &Path) -> SemanticWorkerReport {
    semantic_worker_report_cached(data_root, None)
        .unwrap_or_else(|error| SemanticWorkerReport::unavailable(data_root, format!("{error:#}")))
}
pub(crate) fn daemon_report(data_root: &Path, semantic_report: &SemanticWorkerReport) -> Value {
    daemon_report_with_disabled_status(data_root, semantic_report, true)
}
pub(super) fn daemon_report_with_disabled_status(
    data_root: &Path,
    semantic_report: &SemanticWorkerReport,
    disabled_overrides_lifecycle: bool,
) -> Value {
    let enabled = daemon_enabled_for_status(data_root);
    let status_value = read_daemon_status(data_root);
    let lock_path = daemon_lock_path(data_root);
    let status_path = daemon_status_path(data_root);
    let lock_pid = read_pid_lock_file(&lock_path);
    let mut status = status_value
        .as_ref()
        .and_then(|value| json_string(value, "status"))
        .unwrap_or_else(|| "unknown".to_owned());
    let lock_state = lock_pid.map(process_state);
    let running = pid_lock_file_reports_running(&lock_path, lock_state, status.as_str());
    let stale_lock = lock_path.exists() && pid_lock_file_is_orphaned(&lock_path);
    let stale_lock_overrides_lifecycle =
        stale_lock && !["completed", "failed"].contains(&status.as_str());
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
    let jobs = json!({
        "history_refresh": daemon_history_refresh_job_report(
            data_root,
            disabled_overrides_lifecycle
        ),
        "semantic_index": daemon_semantic_job_report_observed(
            data_root,
            semantic_report,
            disabled_overrides_lifecycle,
            running,
            semantic_runtime_active,
            &config_reload,
        ),
    });
    compact_json(json!({
        "status": status,
        "enabled": enabled,
        "running": running,
        "recoverable": stale_lock_overrides_lifecycle || stale_running_status,
        "reason": if stale_lock_overrides_lifecycle {
            Some("daemon_lock_stale".to_owned())
        } else if stale_running_status {
            Some("daemon_status_stale".to_owned())
        } else {
            status_value
                .as_ref()
                .and_then(|value| json_string(value, "reason"))
        },
        "pid": pid,
        "started_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "started_at_ms")),
        "heartbeat_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "heartbeat_at_ms")),
        "finished_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "finished_at_ms")),
        "start_mode": status_value
            .as_ref()
            .and_then(|value| json_string(value, "start_mode")),
        "trigger_command": status_value
            .as_ref()
            .and_then(|value| json_string(value, "trigger_command")),
        "last_error": status_value.as_ref().and_then(|value| json_string(value, "last_error")),
        "semantic_runtime_active": semantic_runtime_active,
        "config_reload": config_reload,
        "lock_path": lock_path,
        "status_path": status_path,
        "jobs": jobs,
    }))
}

pub(super) fn daemon_history_refresh_job_report(
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
) -> Value {
    let daemon_enabled = daemon_enabled_for_status(data_root);
    let status_value = read_daemon_job_status(&daemon_history_refresh_job_path(data_root));
    let job = status_value.as_ref();
    let disabled = !daemon_enabled && disabled_overrides_lifecycle;
    let current_status = if disabled {
        "disabled".to_owned()
    } else {
        job.and_then(|value| json_string(value, "status"))
            .unwrap_or_else(|| "unknown".to_owned())
    };
    let reason = if disabled {
        Some("daemon_disabled".to_owned())
    } else {
        job.and_then(|value| json_string(value, "reason"))
    };
    compact_json(json!({
        "status": current_status,
        "enabled": daemon_enabled,
        "reason": reason,
        "mode": job
            .and_then(|value| json_string(value, "mode"))
            .unwrap_or_else(|| RefreshArg::Background.as_str().to_owned()),
        "last_run_at_ms": job.and_then(|value| json_i64(value, "last_run_at_ms")),
        "source_count": job.and_then(|value| value.get("source_count").cloned()),
        "source_fingerprint": job
            .and_then(|value| json_string(value, "source_fingerprint")),
        "passes": job.and_then(|value| json_usize(value, "passes")),
        "totals": job.and_then(|value| value.get("totals").cloned()),
        "rejection_diagnostics": job
            .and_then(|value| value.get("rejection_diagnostics")).cloned(),
        "budget_reasons": job
            .and_then(|value| value.get("budget_reasons").cloned()),
        "last_error": job.and_then(|value| json_string(value, "last_error")),
    }))
}

pub(super) fn daemon_enabled_for_status(data_root: &Path) -> bool {
    AppConfig::load(data_root)
        .map(|config| config.daemon.enabled)
        .unwrap_or_else(|_| AppConfig::default().daemon.enabled)
}

pub(super) fn semantic_enabled_for_status(data_root: &Path) -> bool {
    AppConfig::load(data_root)
        .map(|config| config.semantic_search_enabled())
        .unwrap_or_else(|_| AppConfig::default().semantic_search_enabled())
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
    let applied_semantic_enabled = persisted
        .get("applied")
        .and_then(|value| value.get("semantic_enabled"))
        .and_then(Value::as_bool);
    let requested_daemon_enabled = current_config.as_ref().map(|config| config.daemon.enabled);
    let requested_semantic_enabled = current_config
        .as_ref()
        .map(AppConfig::semantic_search_enabled);
    let out_of_sync = running
        && (requested_daemon_enabled != applied_daemon_enabled
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
            "semantic_enabled": requested_semantic_enabled,
        },
        "applied": {
            "daemon_enabled": applied_daemon_enabled,
            "semantic_enabled": applied_semantic_enabled,
        },
        "last_error": persisted.get("last_error").cloned(),
    }))
}

#[cfg(test)]
pub(super) fn daemon_semantic_job_report(
    data_root: &Path,
    semantic_report: &SemanticWorkerReport,
    disabled_overrides_lifecycle: bool,
) -> Value {
    let daemon_status = read_daemon_status(data_root);
    let lock_path = daemon_lock_path(data_root);
    let lock_pid = read_pid_lock_file(&lock_path);
    let persisted_status = daemon_status
        .as_ref()
        .and_then(|value| json_string(value, "status"))
        .unwrap_or_else(|| "unknown".to_owned());
    let running = pid_lock_file_reports_running(
        &lock_path,
        lock_pid.map(process_state),
        persisted_status.as_str(),
    );
    let semantic_runtime_active = running
        && daemon_status
            .as_ref()
            .and_then(|value| value.get("semantic_runtime_active"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let config_reload = daemon_config_reload_report(data_root, daemon_status.as_ref(), running);
    daemon_semantic_job_report_observed(
        data_root,
        semantic_report,
        disabled_overrides_lifecycle,
        running,
        semantic_runtime_active,
        &config_reload,
    )
}

fn daemon_semantic_job_report_observed(
    data_root: &Path,
    semantic_report: &SemanticWorkerReport,
    disabled_overrides_lifecycle: bool,
    daemon_running: bool,
    semantic_runtime_active: bool,
    config_reload: &Value,
) -> Value {
    let applied_daemon_enabled = config_reload
        .get("applied")
        .and_then(|value| value.get("daemon_enabled"))
        .and_then(Value::as_bool);
    let applied_semantic_enabled = config_reload
        .get("applied")
        .and_then(|value| value.get("semantic_enabled"))
        .and_then(Value::as_bool);
    let config_reload_status = config_reload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let requested_daemon_enabled = config_reload
        .get("requested")
        .and_then(|value| value.get("daemon_enabled"))
        .and_then(Value::as_bool);
    let requested_semantic_enabled = config_reload
        .get("requested")
        .and_then(|value| value.get("semantic_enabled"))
        .and_then(Value::as_bool);
    let daemon_enabled = requested_daemon_enabled
        .or(applied_daemon_enabled)
        .unwrap_or_else(|| daemon_enabled_for_status(data_root));
    let semantic_enabled = requested_semantic_enabled
        .or(applied_semantic_enabled)
        .unwrap_or_else(|| semantic_enabled_for_status(data_root));
    let semantic_supported = semantic_query_service_supported();
    let config_out_of_sync = config_reload
        .get("out_of_sync")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let activation_failed = config_reload_status == "activation_failed";
    let reload_pending = config_reload_status == "pending" && config_out_of_sync;
    let status_value = read_daemon_job_status(&daemon_semantic_job_path(data_root))
        .filter(|value| semantic_status_file_model_matches(Some(value)));
    let disabled = (!daemon_enabled || !semantic_enabled || !semantic_supported)
        && disabled_overrides_lifecycle
        && !semantic_runtime_active
        && !semantic_report.running;
    let current_status = if activation_failed && semantic_enabled {
        "failed"
    } else if daemon_running
        && (reload_pending
            || (daemon_enabled
                && semantic_enabled
                && semantic_supported
                && !semantic_runtime_active))
    {
        "pending"
    } else if disabled {
        "disabled"
    } else if semantic_report.running {
        "running"
    } else if semantic_report.status == "stale_lock" {
        "stale_lock"
    } else if semantic_report.status == "unavailable" {
        "unavailable"
    } else if semantic_report.status == "acquiring_model" {
        "acquiring_model"
    } else if semantic_report.status == "loading_model" {
        "loading_model"
    } else if matches!(
        semantic_report.status.as_str(),
        "model_load_deferred"
            | "model_acquisition_failed"
            | "model_load_failed"
            | "model_integrity_failed"
            | "resource_deferred"
    ) {
        "skipped"
    } else if !semantic_report.searchable_items_known {
        "unknown"
    } else if semantic_report.searchable_items == 0 {
        "empty"
    } else if semantic_report.queued_items_estimate == 0 {
        "ready"
    } else if !semantic_report.model_cache_available {
        "skipped"
    } else if semantic_report.status == "failed" {
        "failed"
    } else {
        "pending"
    };
    let derived_reason = if activation_failed && semantic_enabled {
        Some("semantic_activation_failed".to_owned())
    } else if daemon_running && reload_pending {
        Some("daemon_config_reload_pending".to_owned())
    } else if daemon_running
        && daemon_enabled
        && semantic_enabled
        && semantic_supported
        && !semantic_runtime_active
    {
        Some("semantic_runtime_inactive".to_owned())
    } else if disabled {
        Some(if semantic_enabled {
            if semantic_supported {
                "daemon_disabled".to_owned()
            } else {
                "unsupported_platform".to_owned()
            }
        } else {
            "semantic_disabled".to_owned()
        })
    } else if semantic_report.status == "stale_lock" {
        Some("worker_lock_stale".to_owned())
    } else if semantic_report.status == "unavailable" {
        Some("sidecar_unavailable".to_owned())
    } else if semantic_report.status == "acquiring_model" {
        Some("acquiring_model".to_owned())
    } else if semantic_report.status == "loading_model" {
        Some("loading_model".to_owned())
    } else if semantic_report.status == "model_load_deferred" {
        Some("memory_pressure".to_owned())
    } else if semantic_report.status == "resource_deferred" {
        semantic_report
            .resource_deferral
            .as_ref()
            .and_then(|value| json_string(value, "reason"))
            .or_else(|| Some("resource_pressure".to_owned()))
    } else if semantic_report.status == "model_acquisition_failed" {
        Some("model_acquisition_failed".to_owned())
    } else if semantic_report.status == "model_load_failed" {
        Some("model_load_failed".to_owned())
    } else if semantic_report.status == "model_integrity_failed" {
        Some("model_integrity_failed".to_owned())
    } else if !semantic_report.searchable_items_known {
        Some("searchable_items_unknown".to_owned())
    } else if semantic_report.searchable_items == 0 {
        Some("no_searchable_items".to_owned())
    } else if semantic_report.queued_items_estimate > 0 && !semantic_report.model_cache_available {
        Some("model_cache_missing".to_owned())
    } else if semantic_report.status == "failed" {
        Some("worker_failed".to_owned())
    } else {
        None
    };
    compact_json(json!({
        "status": current_status,
        "enabled": daemon_enabled && semantic_enabled && semantic_supported,
        "semantic_enabled": semantic_enabled,
        "daemon_configured": applied_daemon_enabled,
        "semantic_configured": applied_semantic_enabled,
        "runtime_active": semantic_runtime_active,
        "config_reload_status": config_reload_status,
        "configuration_pending": reload_pending,
        "reason": derived_reason,
        "last_run_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "last_run_at_ms")),
        "last_run_status": status_value
            .as_ref()
            .and_then(|value| json_string(value, "status")),
        "last_run_reason": status_value
            .as_ref()
            .and_then(|value| json_string(value, "reason")),
        "last_error": activation_failed
            .then(|| {
                config_reload
                    .get("last_error")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .flatten()
            .or_else(|| {
                status_value
                    .as_ref()
                    .and_then(|value| json_string(value, "last_error"))
            })
            .or_else(|| semantic_report.last_error.clone()),
        "retryable": status_value
            .as_ref()
            .and_then(|value| value.get("retryable").and_then(Value::as_bool)),
        "failure_class": status_value
            .as_ref()
            .and_then(|value| json_string(value, "failure_class"))
            .or_else(|| semantic_report.failure_class.clone()),
        "resource_deferral": semantic_report.resource_deferral.clone(),
        "available_memory_bytes": status_value
            .as_ref()
            .and_then(|value| value.get("available_memory_bytes").and_then(Value::as_u64)),
        "required_available_memory_bytes": status_value
            .as_ref()
            .and_then(|value| value.get("required_available_memory_bytes").and_then(Value::as_u64)),
        "indexed_chunks": status_value.as_ref().and_then(|value| json_usize(value, "indexed_chunks")),
        "model_cache_available": semantic_report.model_cache_available,
        "model_acquisition": semantic_report.model_acquisition.clone(),
        "embed_policy": semantic_report.embed_policy.clone(),
        "embedding_runtime": semantic_report.embedding_runtime.clone(),
        "worker_status": semantic_report.status,
        "coverage": {
            "searchable_items": semantic_report.searchable_items,
            "searchable_items_known": semantic_report.searchable_items_known,
            "completed_items": semantic_report.embedded_items,
            "embedded_items": semantic_report.embedded_items,
            "embedded_chunks": semantic_report.embedded_chunks,
            "dirty_items": semantic_report.dirty_items,
            "queued_items_estimate": semantic_report.queued_items_estimate,
        },
    }))
}
use std::{
    env, fs,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process,
    time::SystemTime,
};

use anyhow::{Context, Result};
use ctx_history_core::utc_now;
use ctx_history_store::Store;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{commands::search::RefreshArg, compact_json, config::AppConfig};

use super::{
    health_search::{
        create_private_dir_all, json_i64, json_string, json_u32, json_usize,
        private_create_new_file, private_create_new_lock_file, private_open_existing_lock_file,
        secure_private_file_permissions, semantic_embed_policy_status_json,
        semantic_model_acquisition_status_json, semantic_model_cache_available,
        semantic_status_needs_exact_sidecar_stats, semantic_worker_cache_dir,
    },
    model_contract::semantic_model_key,
    query_service::semantic_query_service_supported,
    reports::{SemanticReportCountMode, SemanticWorkerReport},
    runtime_limits::{
        DAEMON_DIR, DAEMON_HISTORY_REFRESH_JOB_FILE, DAEMON_JOBS_DIR, DAEMON_LOCK_FILE,
        DAEMON_LOCK_STALE_AFTER_MS, DAEMON_SEMANTIC_JOB_FILE, DAEMON_STATUS_FILE,
        PID_LOCK_ACQUIRE_ATTEMPTS, PID_LOCK_ACQUIRE_RETRY, PID_LOCK_INCOMPLETE_GRACE,
        PID_LOCK_PROTOCOL, SEMANTIC_WORKER_LOCK_FILE, SEMANTIC_WORKER_STATUS_FILE,
    },
    vector_store::{SemanticSidecarStats, SemanticVectorStore},
};

#[cfg(unix)]
use super::runtime_limits::DAEMON_QUERY_SOCKET_FILE;
