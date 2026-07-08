fn semantic_vector_path(data_root: &Path) -> PathBuf {
    data_root.join("vectors.sqlite")
}

fn semantic_worker_lock_path(data_root: &Path) -> PathBuf {
    data_root.join(SEMANTIC_WORKER_LOCK_FILE)
}

fn semantic_worker_status_path(data_root: &Path) -> PathBuf {
    data_root.join(SEMANTIC_WORKER_STATUS_FILE)
}

fn daemon_root_path(data_root: &Path) -> PathBuf {
    data_root.join(DAEMON_DIR)
}

fn daemon_jobs_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_JOBS_DIR)
}

fn daemon_lock_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_LOCK_FILE)
}

fn daemon_status_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_STATUS_FILE)
}

fn daemon_query_socket_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_QUERY_SOCKET_FILE)
}

fn daemon_history_refresh_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(DAEMON_HISTORY_REFRESH_JOB_FILE)
}

fn daemon_semantic_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(DAEMON_SEMANTIC_JOB_FILE)
}

fn daemon_cloud_sync_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(DAEMON_CLOUD_SYNC_JOB_FILE)
}

struct DaemonLock {
    path: PathBuf,
}

impl DaemonLock {
    fn acquire(data_root: &Path) -> Result<Option<Self>> {
        create_private_dir_all(data_root)?;
        let root = daemon_root_path(data_root);
        create_private_dir_all(&root)?;
        let path = daemon_lock_path(data_root);
        for attempt in 0..2 {
            match private_create_new_file(&path) {
                Ok(mut file) => {
                    let payload = json!({
                        "pid": process::id(),
                        "started_at_ms": utc_now().timestamp_millis(),
                        "binary": env::current_exe().ok(),
                        "data_root": data_root,
                    });
                    writeln!(file, "{}", serde_json::to_string(&payload)?)?;
                    return Ok(Some(Self { path }));
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if attempt == 0 && daemon_lock_is_stale(&path) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    return Ok(None);
                }
                Err(err) => {
                    return Err(err)
                        .with_context(|| format!("create ctx daemon lock {}", path.display()));
                }
            }
        }
        Ok(None)
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct SemanticWorkerLock {
    path: PathBuf,
}

impl SemanticWorkerLock {
    fn acquire(data_root: &Path) -> Result<Option<Self>> {
        create_private_dir_all(data_root)?;
        let path = semantic_worker_lock_path(data_root);
        for attempt in 0..2 {
            match private_create_new_file(&path) {
                Ok(mut file) => {
                    let payload = json!({
                        "pid": process::id(),
                        "started_at_ms": utc_now().timestamp_millis(),
                    });
                    writeln!(file, "{}", serde_json::to_string(&payload)?)?;
                    return Ok(Some(Self { path }));
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if attempt == 0 && semantic_worker_lock_is_stale(&path) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    return Ok(None);
                }
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!("create semantic worker lock {}", path.display())
                    });
                }
            }
        }
        Ok(None)
    }
}

impl Drop for SemanticWorkerLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn semantic_worker_lock_is_stale(path: &Path) -> bool {
    pid_lock_file_is_stale(path)
}

fn daemon_lock_is_stale(path: &Path) -> bool {
    pid_lock_file_is_stale(path)
}

fn pid_lock_file_is_stale(path: &Path) -> bool {
    let Some(value) = read_pid_lock_json(path) else {
        return path.exists();
    };
    if lock_started_at_is_stale(&value) {
        return true;
    }
    let Some(pid) = pid_from_lock_json(&value) else {
        return true;
    };
    !pid_is_running(pid)
}

fn read_pid_lock_file(path: &Path) -> Option<u32> {
    read_pid_lock_json(path).and_then(|value| pid_from_lock_json(&value))
}

fn read_pid_lock_json(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn pid_from_lock_json(value: &Value) -> Option<u32> {
    value
        .get("pid")
        .and_then(|value| value.as_u64())
        .and_then(|pid| u32::try_from(pid).ok())
}

fn lock_started_at_is_stale(value: &Value) -> bool {
    let Some(started_at_ms) = json_i64(value, "started_at_ms") else {
        return false;
    };
    utc_now().timestamp_millis().saturating_sub(started_at_ms) > DAEMON_LOCK_STALE_AFTER_MS
}

#[cfg(unix)]
fn pid_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn pid_is_running(pid: u32) -> bool {
    pid != 0
}

#[cfg(unix)]
fn lower_semantic_worker_priority() {
    unsafe {
        let _ = libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

#[cfg(not(unix))]
fn lower_semantic_worker_priority() {}

fn write_private_json_file(path: &Path, value: &Value) -> Result<()> {
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

fn write_semantic_worker_status(data_root: &Path, value: &Value) -> Result<()> {
    write_private_json_file(&semantic_worker_status_path(data_root), value)
}

fn read_semantic_worker_status(data_root: &Path) -> Option<Value> {
    let text = fs::read_to_string(semantic_worker_status_path(data_root)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_daemon_status(data_root: &Path, value: &Value) -> Result<()> {
    write_private_json_file(&daemon_status_path(data_root), value)
}

fn read_daemon_status(data_root: &Path) -> Option<Value> {
    let text = fs::read_to_string(daemon_status_path(data_root)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_daemon_job_status(path: &Path, value: &Value) -> Result<()> {
    write_private_json_file(path, value)
}

fn read_daemon_job_status(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn semantic_status_file_model_matches(status_value: Option<&Value>) -> bool {
    status_value
        .and_then(|value| json_string(value, "model_key"))
        .is_some_and(|model_key| model_key == SEMANTIC_MODEL_KEY)
}

fn semantic_status_file_searchable_items(status_value: Option<&Value>) -> Option<usize> {
    if !semantic_status_file_model_matches(status_value) {
        return None;
    }
    status_value.and_then(|value| json_usize(value, "searchable_items"))
}

fn semantic_status_file_stats(status_value: Option<&Value>) -> SemanticSidecarStats {
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
    semantic_worker_report_with_count_mode(data_root, store, SemanticReportCountMode::ExactOnCacheMiss)
}

pub(crate) fn semantic_worker_report_cached(
    data_root: &Path,
    store: Option<&Store>,
) -> Result<SemanticWorkerReport> {
    semantic_worker_report_with_count_mode(data_root, store, SemanticReportCountMode::CachedOrStatusFile)
}

fn semantic_worker_report_with_count_mode(
    data_root: &Path,
    store: Option<&Store>,
    count_mode: SemanticReportCountMode,
) -> Result<SemanticWorkerReport> {
    let status_value = read_semantic_worker_status(data_root);
    let status_file_model_matches = semantic_status_file_model_matches(status_value.as_ref());
    let current_status_value = status_value.as_ref().filter(|_| status_file_model_matches);
    let (searchable_items, searchable_items_known) = match store {
        Some(store) if count_mode == SemanticReportCountMode::ExactOnCacheMiss => {
            (store.event_embedding_document_count_cached_or_exact()?, true)
        }
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
                SemanticReportCountMode::ExactOnCacheMiss => vector_store.cached_or_exact_stats()?,
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
    let running = lock_pid.is_some_and(pid_is_running);
    let pid = if running {
        lock_pid
    } else {
        current_status_value.and_then(|value| json_u32(value, "pid"))
    };
    let queued_items_estimate = searchable_items
        .saturating_sub(embedded_items)
        .max(dirty_items);
    let mut status = current_status_value
        .and_then(|value| json_string(value, "status"))
        .unwrap_or_else(|| {
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
            || status == "model_acquisition_failed"
            || (status == "failed"
                && sidecar_error.is_none()
                && embedded_items == 0
                && queued_items_estimate > 0);
        status = if preserve_status { status } else { live_status };
    }
    if running {
        status = "running".to_owned();
    } else if lock_path.exists()
        && semantic_worker_lock_is_stale(&lock_path)
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
        last_error: sidecar_error.or_else(|| {
            current_status_value.and_then(|value| json_string(value, "last_error"))
        }),
        searchable_items,
        searchable_items_known,
        embedded_items,
        embedded_chunks,
        dirty_items,
        queued_items_estimate,
        model_cache_available,
        embed_policy: current_status_value
            .and_then(|value| value.get("embed_policy").cloned())
            .or_else(|| Some(semantic_embed_policy_status_json())),
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

fn daemon_report_with_disabled_status(
    data_root: &Path,
    semantic_report: &SemanticWorkerReport,
    disabled_overrides_lifecycle: bool,
) -> Value {
    let enabled = daemon_enabled_for_status(data_root);
    let status_value = read_daemon_status(data_root);
    let lock_path = daemon_lock_path(data_root);
    let status_path = daemon_status_path(data_root);
    let lock_pid = read_pid_lock_file(&lock_path);
    let running = lock_pid.is_some_and(pid_is_running);
    let stale_lock = lock_path.exists() && daemon_lock_is_stale(&lock_path);
    let mut status = status_value
        .as_ref()
        .and_then(|value| json_string(value, "status"))
        .unwrap_or_else(|| "unknown".to_owned());
    let stale_running_status = !running && status == "running";
    if running {
        status = "running".to_owned();
    } else if stale_lock || stale_running_status {
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
    compact_json(json!({
        "status": status,
        "enabled": enabled,
        "running": running,
        "recoverable": stale_lock || stale_running_status,
        "reason": if stale_lock {
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
        "lock_path": lock_path,
        "status_path": status_path,
        "jobs": {
            "history_refresh": daemon_history_refresh_job_report(
                data_root,
                disabled_overrides_lifecycle
            ),
            "semantic_index": daemon_semantic_job_report(
                data_root,
                semantic_report,
                disabled_overrides_lifecycle
            ),
            "cloud_sync": daemon_cloud_sync_job_report(data_root),
        },
    }))
}

fn daemon_history_refresh_job_report(
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
) -> Value {
    let daemon_enabled = daemon_enabled_for_status(data_root);
    let status_value = read_daemon_job_status(&daemon_history_refresh_job_path(data_root));
    let disabled = !daemon_enabled && disabled_overrides_lifecycle;
    let current_status = if disabled {
        "disabled".to_owned()
    } else {
        status_value
            .as_ref()
            .and_then(|value| json_string(value, "status"))
            .unwrap_or_else(|| "unknown".to_owned())
    };
    let reason = if disabled {
        Some("daemon_disabled".to_owned())
    } else {
        status_value
            .as_ref()
            .and_then(|value| json_string(value, "reason"))
    };
    compact_json(json!({
        "status": current_status,
        "enabled": daemon_enabled,
        "reason": reason,
        "mode": status_value
            .as_ref()
            .and_then(|value| json_string(value, "mode"))
            .unwrap_or_else(|| RefreshArg::Background.as_str().to_owned()),
        "last_run_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "last_run_at_ms")),
        "source_count": status_value.as_ref().and_then(|value| value.get("source_count").cloned()),
        "source_fingerprint": status_value
            .as_ref()
            .and_then(|value| json_string(value, "source_fingerprint")),
        "passes": status_value.as_ref().and_then(|value| json_usize(value, "passes")),
        "totals": status_value.as_ref().and_then(|value| value.get("totals").cloned()),
        "budget_reasons": status_value
            .as_ref()
            .and_then(|value| value.get("budget_reasons").cloned()),
        "last_error": status_value
            .as_ref()
            .and_then(|value| json_string(value, "last_error")),
    }))
}

fn daemon_enabled_for_status(data_root: &Path) -> bool {
    AppConfig::load(data_root)
        .map(|config| config.daemon.enabled)
        .unwrap_or_else(|_| AppConfig::default().daemon.enabled)
}

fn semantic_enabled_for_status(data_root: &Path) -> bool {
    AppConfig::load(data_root)
        .map(|config| config.semantic_search_enabled())
        .unwrap_or_else(|_| AppConfig::default().semantic_search_enabled())
}

fn daemon_semantic_job_report(
    data_root: &Path,
    semantic_report: &SemanticWorkerReport,
    disabled_overrides_lifecycle: bool,
) -> Value {
    let daemon_enabled = daemon_enabled_for_status(data_root);
    let semantic_enabled = semantic_enabled_for_status(data_root);
    let semantic_supported = semantic_query_service_supported();
    let status_value = read_daemon_job_status(&daemon_semantic_job_path(data_root))
        .filter(|value| semantic_status_file_model_matches(Some(value)));
    let disabled = (!daemon_enabled || !semantic_enabled || !semantic_supported)
        && disabled_overrides_lifecycle
        && !semantic_report.running;
    let current_status = if disabled {
        "disabled"
    } else if semantic_report.running {
        "running"
    } else if semantic_report.status == "stale_lock" {
        "stale_lock"
    } else if semantic_report.status == "unavailable" {
        "unavailable"
    } else if semantic_report.status == "acquiring_model" {
        "acquiring_model"
    } else if semantic_report.status == "model_acquisition_failed" {
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
    let derived_reason = if disabled {
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
    } else if semantic_report.status == "model_acquisition_failed" {
        Some("model_acquisition_failed".to_owned())
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
        "reason": derived_reason,
        "last_run_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "last_run_at_ms")),
        "last_run_status": status_value
            .as_ref()
            .and_then(|value| json_string(value, "status")),
        "last_run_reason": status_value
            .as_ref()
            .and_then(|value| json_string(value, "reason")),
        "last_error": status_value
            .as_ref()
            .and_then(|value| json_string(value, "last_error"))
            .or_else(|| semantic_report.last_error.clone()),
        "indexed_chunks": status_value.as_ref().and_then(|value| json_usize(value, "indexed_chunks")),
        "model_cache_available": semantic_report.model_cache_available,
        "embed_policy": status_value
            .as_ref()
            .and_then(|value| value.get("embed_policy").cloned())
            .or_else(|| semantic_report.embed_policy.clone()),
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

fn daemon_cloud_sync_job_report(data_root: &Path) -> Value {
    let status_value = read_daemon_job_status(&daemon_cloud_sync_job_path(data_root));
    compact_json(json!({
        "status": "disabled",
        "enabled": false,
        "reason": "not_configured",
        "network_allowed": false,
        "last_run_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "last_run_at_ms")),
        "last_upload_at_ms": Value::Null,
        "queued_items_estimate": 0,
        "last_error": Value::Null,
    }))
}
