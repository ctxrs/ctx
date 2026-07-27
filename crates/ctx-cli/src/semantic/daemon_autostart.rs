pub(super) fn daemon_autostart_exe() -> Result<PathBuf> {
    env::var("CTX_DAEMON_AUTOSTART_EXE")
        .ok()
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| env::current_exe().context("resolve ctx daemon autostart executable"))
}

pub(super) fn write_daemon_autostart_status(
    data_root: &Path,
    trigger: DaemonTriggerCommandArg,
    status: &str,
    reason: Option<&str>,
    last_error: Option<String>,
    pid: Option<u32>,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_daemon_status(
        data_root,
        &compact_json(json!({
            "schema_version": 1,
            "status": status,
            "reason": reason,
            "pid": pid,
            "started_at_ms": Value::Null,
            "heartbeat_at_ms": now,
            "finished_at_ms": now,
            "start_mode": DaemonStartModeArg::Auto.as_str(),
            "trigger_command": trigger.as_str(),
            "last_error": last_error,
        })),
    )
}

pub(super) fn daemon_autostart_u64_env(name: &str, default: u64, max: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(max))
        .unwrap_or(default)
}

const DAEMON_UPGRADE_STOP_TIMEOUT: StdDuration = StdDuration::from_secs(75);
const DAEMON_UPGRADE_RESTART_TIMEOUT: StdDuration = StdDuration::from_secs(5);
const DAEMON_UPGRADE_POLL_INTERVAL: StdDuration = StdDuration::from_millis(50);
const DAEMON_UPGRADE_HANDOFF_STALE_AFTER: StdDuration = StdDuration::from_secs(15 * 60);
const DAEMON_INSTALLATION_QUIESCE_TIMEOUT: StdDuration = StdDuration::from_secs(75);
const DAEMON_UPGRADE_HANDOFF_FILE: &str = "upgrade-handoff.json";
const DAEMON_UPGRADE_RESTART_REQUEST_DIR: &str = "upgrade-restart-requests";
const DAEMON_UPGRADE_HANDOFF_TOKEN_ENV: &str = "CTX_DAEMON_UPGRADE_HANDOFF_TOKEN";
// Installation recovery may restart several registered data-root daemons
// serially before this daemon can publish final readiness. Keep setup bounded,
// but allow that established five-second-per-registration path ample room.
const DAEMON_SETUP_HANDOFF_POLL_ATTEMPTS: usize = 12_001;
const DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS: i64 = 30_000;
const DAEMON_SETUP_HANDOFF_MAX_FUTURE_HEARTBEAT_MS: i64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DaemonHandoff {
    pub(crate) pid: u32,
    pub(crate) heartbeat_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DaemonHandoffObservation {
    Pending,
    Running(DaemonHandoff),
    Failed(String),
}

enum DaemonAutostartRequest {
    Suppressed(&'static str),
    Existing,
    Deferred(PathBuf),
    Spawned(Child),
}

pub(super) struct InstallationDaemonLease {
    lock: fs::File,
    registration_path: PathBuf,
    registration_id: String,
    data_root: PathBuf,
    trigger: DaemonTriggerCommandArg,
    idle_exit_seconds: u64,
    loop_interval_seconds: u64,
    status: &'static str,
}

#[derive(Debug)]
struct InstallationDaemonRestart {
    registration_path: PathBuf,
    data_root: PathBuf,
    trigger: DaemonTriggerCommandArg,
    idle_exit_seconds: u64,
    loop_interval_seconds: u64,
}

impl InstallationDaemonLease {
    pub(super) fn acquire(
        data_root: &Path,
        trigger: DaemonTriggerCommandArg,
        idle_exit_seconds: u64,
        loop_interval_seconds: u64,
        allow_active_upgrade: bool,
    ) -> Result<Option<Self>> {
        let lock = open_installation_daemon_quiescence_lock()?;
        match fs2::FileExt::try_lock_shared(&lock) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => {
                return Err(error).context("acquire ctx installation daemon lease");
            }
        }
        if !allow_active_upgrade && crate::upgrade::installation_upgrade_is_active()? {
            let _ = fs2::FileExt::unlock(&lock);
            return Ok(None);
        }
        let (_, registration_root) = crate::upgrade::installation_daemon_coordination_paths()?;
        create_private_dir_all(&registration_root)?;
        let registration_id = Uuid::now_v7().to_string();
        let registration_path = registration_root.join(format!("{registration_id}.json"));
        let mut lease = Self {
            lock,
            registration_path,
            registration_id,
            data_root: data_root.to_path_buf(),
            trigger,
            idle_exit_seconds,
            loop_interval_seconds,
            status: "live",
        };
        lease.write_status("live", None)?;
        if !allow_active_upgrade && crate::upgrade::installation_upgrade_is_active()? {
            lease.status = "removed";
            let _ = fs::remove_file(&lease.registration_path);
            return Ok(None);
        }
        Ok(Some(lease))
    }

    pub(super) fn acknowledge(mut self, attempt_id: &str) -> Result<()> {
        self.status = "quiescing";
        self.write_status("quiescing", Some(attempt_id))?;
        write_daemon_restart_request(self.data_root.as_path(), self.trigger, attempt_id)?;
        self.write_status("acknowledged", Some(attempt_id))?;
        self.status = "acknowledged";
        Ok(())
    }

    fn write_status(&self, status: &str, attempt_id: Option<&str>) -> Result<()> {
        write_private_json_file(
            &self.registration_path,
            &compact_json(json!({
                "schema_version": 1,
                "registration_id": self.registration_id,
                "status": status,
                "attempt_id": attempt_id,
                "pid": process::id(),
                "data_root": self.data_root,
                "trigger_command": self.trigger.as_str(),
                "idle_exit_seconds": self.idle_exit_seconds,
                "loop_interval_seconds": self.loop_interval_seconds,
                "updated_at_ms": utc_now().timestamp_millis(),
            })),
        )
    }
}

impl Drop for InstallationDaemonLease {
    fn drop(&mut self) {
        if self.status == "live" {
            let _ = fs::remove_file(&self.registration_path);
        }
        let _ = fs2::FileExt::unlock(&self.lock);
    }
}

fn open_installation_daemon_quiescence_lock() -> Result<fs::File> {
    let (path, _) = crate::upgrade::installation_daemon_coordination_paths()?;
    open_installation_daemon_quiescence_lock_at(&path)
}

fn open_installation_daemon_quiescence_lock_at(path: &Path) -> Result<fs::File> {
    let (file, _) = open_or_create_pid_lock_file(path)
        .with_context(|| format!("open ctx installation daemon lock {}", path.display()))?;
    secure_private_file_permissions(path)?;
    Ok(file)
}

fn wait_for_installation_daemon_quiescence(attempt_id: &str) -> Result<()> {
    let (lock_path, registration_root) = crate::upgrade::installation_daemon_coordination_paths()?;
    wait_for_installation_daemon_quiescence_at(
        &lock_path,
        &registration_root,
        attempt_id,
        DAEMON_INSTALLATION_QUIESCE_TIMEOUT,
    )
}

fn wait_for_installation_daemon_quiescence_at(
    lock_path: &Path,
    registration_root: &Path,
    attempt_id: &str,
    timeout: StdDuration,
) -> Result<()> {
    let lock = open_installation_daemon_quiescence_lock_at(lock_path)?;
    let deadline = Instant::now() + timeout;
    loop {
        match fs2::FileExt::try_lock_exclusive(&lock) {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(anyhow!(
                        "timed out waiting for all ctx daemons to acknowledge installation quiescence"
                    ));
                }
                std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
            }
            Err(error) => {
                return Err(error).context("acquire ctx installation daemon quiescence");
            }
        }
    }
    let result =
        read_installation_daemon_restarts_from(registration_root, attempt_id, true).map(|_| ());
    let _ = fs2::FileExt::unlock(&lock);
    result
}

fn read_installation_daemon_restarts(
    executable: &Path,
    attempt_id: &str,
) -> Result<Vec<InstallationDaemonRestart>> {
    let (_, root) = crate::upgrade::installation_daemon_coordination_paths_for(executable);
    read_installation_daemon_restarts_from(&root, attempt_id, false)
}

fn read_installation_daemon_restarts_from(
    root: &Path,
    attempt_id: &str,
    fail_on_live: bool,
) -> Result<Vec<InstallationDaemonRestart>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read ctx daemon acknowledgements {}", root.display()));
        }
    };
    let mut restarts = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("inspect ctx daemon acknowledgement {}", path.display()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(anyhow!(
                "ctx daemon acknowledgement is not a regular file: {}",
                path.display()
            ));
        }
        let value = fs::read(&path)
            .with_context(|| format!("read ctx daemon acknowledgement {}", path.display()))
            .and_then(|bytes| {
                serde_json::from_slice::<Value>(&bytes)
                    .with_context(|| format!("parse ctx daemon acknowledgement {}", path.display()))
            })?;
        validate_installation_daemon_registration(&value, &path)?;
        let status = value["status"].as_str().unwrap_or_default();
        let registration_attempt = value.get("attempt_id").and_then(Value::as_str);
        if status == "quiescing" && registration_attempt == Some(attempt_id) {
            return Err(anyhow!(
                "ctx daemon exited without completing its quiescence acknowledgement"
            ));
        }
        if status == "live" {
            let pid = value["pid"]
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok())
                .unwrap_or_default();
            if fail_on_live {
                match process_state(pid) {
                    ProcessState::NotRunning => continue,
                    ProcessState::Running | ProcessState::Unknown => {
                        return Err(anyhow!(
                            "ctx daemon registration remains live after installation quiescence"
                        ));
                    }
                }
            }
            continue;
        }
        if status != "acknowledged" || registration_attempt != Some(attempt_id) {
            continue;
        }
        let data_root = PathBuf::from(value["data_root"].as_str().unwrap_or_default());
        let trigger = parse_daemon_trigger(value["trigger_command"].as_str())
            .ok_or_else(|| anyhow!("ctx daemon acknowledgement has an invalid trigger"))?;
        let idle_exit_seconds = value["idle_exit_seconds"]
            .as_u64()
            .filter(|value| *value > 0 && *value <= DAEMON_IDLE_EXIT_SECONDS_CAP)
            .ok_or_else(|| anyhow!("ctx daemon acknowledgement has an invalid idle timeout"))?;
        let loop_interval_seconds = value["loop_interval_seconds"]
            .as_u64()
            .filter(|value| *value > 0 && *value <= 3_600)
            .ok_or_else(|| anyhow!("ctx daemon acknowledgement has an invalid loop interval"))?;
        restarts.push(InstallationDaemonRestart {
            registration_path: path,
            data_root,
            trigger,
            idle_exit_seconds,
            loop_interval_seconds,
        });
    }
    restarts.sort_by(|left, right| left.data_root.cmp(&right.data_root));
    restarts.dedup_by(|left, right| left.data_root == right.data_root);
    Ok(restarts)
}

fn validate_installation_daemon_registration(value: &Value, path: &Path) -> Result<()> {
    let valid_status = matches!(
        value.get("status").and_then(Value::as_str),
        Some("live" | "released" | "quiescing" | "acknowledged")
    );
    let valid_root = value
        .get("data_root")
        .and_then(Value::as_str)
        .map(Path::new)
        .is_some_and(Path::is_absolute);
    if value.get("schema_version").and_then(Value::as_u64) != Some(1)
        || !value
            .get("registration_id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty())
        || !valid_status
        || !valid_root
    {
        return Err(anyhow!(
            "invalid ctx daemon acknowledgement at {}",
            path.display()
        ));
    }
    Ok(())
}

fn daemon_upgrade_handoff_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_UPGRADE_HANDOFF_FILE)
}

fn daemon_upgrade_restart_request_root(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_UPGRADE_RESTART_REQUEST_DIR)
}

fn daemon_query_endpoint_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_QUERY_ENDPOINT_FILE)
}

fn read_daemon_upgrade_handoff(data_root: &Path) -> Option<Value> {
    let text = fs::read_to_string(daemon_upgrade_handoff_path(data_root)).ok()?;
    serde_json::from_str(&text).ok()
}

fn daemon_upgrade_handoff_is_active(data_root: &Path) -> bool {
    let path = daemon_upgrade_handoff_path(data_root);
    let Some(value) = read_daemon_upgrade_handoff(data_root) else {
        return false;
    };
    let marker_is_fresh = fs::metadata(&path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .is_some_and(|modified| {
            SystemTime::now()
                .duration_since(modified)
                .map_or(true, |age| age <= DAEMON_UPGRADE_HANDOFF_STALE_AFTER)
        });
    if !marker_is_fresh {
        return false;
    }
    let pid_keys: &[&str] = match value.get("phase").and_then(Value::as_str) {
        Some("completed" | "aborted") => return false,
        Some("scheduled") => &["helper_pid"],
        _ => &["owner_pid"],
    };
    pid_keys.iter().any(|key| {
        let Some(pid) = value
            .get(*key)
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
        else {
            return false;
        };
        match process_state(pid) {
            ProcessState::Running | ProcessState::Unknown => true,
            ProcessState::NotRunning => false,
        }
    })
}

pub(super) fn daemon_upgrade_handoff_blocks_current_process(data_root: &Path) -> bool {
    if !daemon_upgrade_handoff_is_active(data_root) {
        return false;
    }
    !current_process_owns_daemon_upgrade_handoff(data_root)
}

pub(super) fn current_process_owns_daemon_upgrade_handoff(data_root: &Path) -> bool {
    if !daemon_upgrade_handoff_is_active(data_root) {
        return false;
    }
    let expected = read_daemon_upgrade_handoff(data_root).and_then(|value| {
        value
            .get("handoff_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    });
    expected.is_some() && expected == env::var(DAEMON_UPGRADE_HANDOFF_TOKEN_ENV).ok()
}

pub(crate) struct DaemonUpgradeHandoff {
    data_root: PathBuf,
    handoff_id: String,
    restart_trigger: Option<DaemonTriggerCommandArg>,
    release_on_drop: bool,
}

impl DaemonUpgradeHandoff {
    pub(crate) fn wait_for_installation_quiescence(&self) -> Result<()> {
        wait_for_installation_daemon_quiescence(&self.handoff_id)?;
        pause_after_installation_quiescence_for_test()
    }

    /// Capture the effective auto-daemon restart request in data that can be
    /// embedded in a durable platform replacement helper.
    pub(crate) fn replacement_restart(&self) -> Option<(&'static str, u64, u64)> {
        let trigger = self
            .restart_trigger
            .or_else(|| read_daemon_restart_request(&self.data_root).map(|(_, trigger)| trigger))?;
        Some((
            trigger.as_str(),
            daemon_autostart_u64_env(
                "CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS",
                DAEMON_AUTOSTART_IDLE_EXIT_SECONDS_DEFAULT,
                DAEMON_IDLE_EXIT_SECONDS_CAP,
            ),
            daemon_autostart_u64_env(
                "CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS",
                DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS_DEFAULT,
                3_600,
            ),
        ))
    }

    /// Preserve restart intent across a Unix rollback re-exec. The restored
    /// executable consumes this request only after it is running.
    pub(crate) fn prepare_reexec(mut self) -> Result<()> {
        if read_daemon_restart_request(&self.data_root).is_none() {
            if let Some(trigger) = self.restart_trigger {
                write_daemon_restart_request(&self.data_root, trigger, &self.handoff_id)?;
            }
        }
        self.release("aborted", None)?;
        self.release_on_drop = false;
        Ok(())
    }

    /// Release the upgrade fence and restart the auto-daemon when requested.
    /// Call this after publication succeeds, or with the restored executable
    /// after a rollback has made that path safe to run.
    pub(crate) fn resume_with(mut self, executable: &Path) -> Result<()> {
        let restart_trigger = self
            .restart_trigger
            .or_else(|| read_daemon_restart_request(&self.data_root).map(|(_, trigger)| trigger));
        if daemon_restart_allowed(&self.data_root)? {
            if let Some(trigger) = restart_trigger {
                let mut child = configured_daemon_autostart_command(
                    executable,
                    &self.data_root,
                    trigger,
                    Some(&self.handoff_id),
                )
                .spawn()
                .context("restart ctx daemon after upgrade")?;
                wait_for_replacement_daemon(&self.data_root, &mut child)?;
            }
        }
        remove_daemon_restart_requests(&self.data_root);
        restart_acknowledged_installation_daemons(
            executable,
            &self.handoff_id,
            Some(&self.data_root),
        )?;
        self.release("completed", None)?;
        self.release_on_drop = false;
        Ok(())
    }

    /// Restart an executable from v0.25, which predates durable restart
    /// acknowledgement. Clear the v0.26-only request before launching it and
    /// verify the strongest readiness signal that version provides: ownership
    /// of the daemon lifecycle lock. A restart failure is returned as a warning
    /// rather than blocking the required rollback re-exec.
    pub(crate) fn resume_legacy_reexec_with(mut self, executable: &Path) -> Result<Option<String>> {
        let restart_trigger = self
            .restart_trigger
            .or_else(|| read_daemon_restart_request(&self.data_root).map(|(_, trigger)| trigger));
        remove_daemon_restart_requests(&self.data_root);
        let restart = (|| -> Result<()> {
            let config = AppConfig::load(&self.data_root)?;
            if daemon_autostart_allowed(&self.data_root, &config) {
                if let Some(trigger) = restart_trigger {
                    clear_legacy_daemon_readiness(&self.data_root)?;
                    let mut child = configured_daemon_autostart_command(
                        executable,
                        &self.data_root,
                        trigger,
                        Some(&self.handoff_id),
                    )
                    .spawn()
                    .context("restart legacy ctx daemon after rollback")?;
                    wait_for_legacy_replacement_daemon(
                        &self.data_root,
                        &mut child,
                        trigger,
                        config.semantic_search_enabled()
                            && super::semantic_query_service_supported(),
                    )?;
                }
            }
            restart_acknowledged_legacy_installation_daemons(
                executable,
                &self.handoff_id,
                Some(&self.data_root),
            )?;
            Ok(())
        })();
        // A v0.25 executable cannot consume the v0.26 restart-request
        // protocol. A failed best-effort daemon restart must therefore not
        // restore that request or prevent rollback recovery from re-execing.
        remove_daemon_restart_requests(&self.data_root);
        let warning = restart.err().map(|error| {
            format!(
                "legacy ctx daemon restart failed; continuing rollback recovery without a running daemon: {error:#}"
            )
        });
        self.release(
            if warning.is_none() {
                "completed"
            } else {
                "aborted"
            },
            None,
        )?;
        self.release_on_drop = false;
        Ok(warning)
    }

    /// Keep the fence owned by a platform replacement helper after apply
    /// returns `Scheduled`. Autostart remains blocked while that helper is live
    /// and becomes eligible only after it exits.
    pub(crate) fn transfer_to_replacement_helper(mut self, helper_pid: u32) -> Result<()> {
        let already_transferred =
            read_daemon_upgrade_handoff(&self.data_root).is_some_and(|value| {
                value.get("handoff_id").and_then(Value::as_str) == Some(self.handoff_id.as_str())
                    && value.get("phase").and_then(Value::as_str) == Some("scheduled")
                    && value
                        .get("helper_pid")
                        .and_then(Value::as_u64)
                        .and_then(|pid| u32::try_from(pid).ok())
                        == Some(helper_pid)
            });
        if !already_transferred {
            self.release("scheduled", Some(helper_pid))?;
        }
        self.release_on_drop = false;
        Ok(())
    }

    fn release(&self, phase: &str, helper_pid: Option<u32>) -> Result<()> {
        let current = read_daemon_upgrade_handoff(&self.data_root);
        if current
            .as_ref()
            .and_then(|value| value.get("handoff_id").and_then(Value::as_str))
            != Some(self.handoff_id.as_str())
        {
            return Ok(());
        }
        write_daemon_upgrade_handoff(&self.data_root, &self.handoff_id, phase, helper_pid)
    }
}

impl Drop for DaemonUpgradeHandoff {
    fn drop(&mut self) {
        if self.release_on_drop {
            let _ = self.release("aborted", None);
        }
    }
}

/// Fence daemon starts, request a cooperative exit from the current daemon, and
/// wait until its process lock is released before binary replacement begins.
///
/// The actual upgrade owner must already hold the upgrade transaction lock.
/// This handoff deliberately does not schedule or serialize upgrades.
pub(crate) fn begin_daemon_upgrade_handoff(
    data_root: &Path,
    upgrade_attempt_id: &str,
) -> Result<DaemonUpgradeHandoff> {
    if daemon_upgrade_handoff_is_active(data_root) {
        return Err(anyhow!(
            "another ctx upgrade owns the daemon lifecycle handoff"
        ));
    }
    let restart_trigger = daemon_restart_trigger(data_root);
    if !crate::upgrade::is_valid_upgrade_attempt_id(upgrade_attempt_id) {
        return Err(anyhow!(
            "invalid upgrade attempt identity for daemon handoff"
        ));
    }
    let handoff_id = upgrade_attempt_id.to_owned();
    // Durably record restart intent before fencing or stopping the daemon.
    // A process crash at any later point must leave a replacement request for
    // the next safe upgrade/daemon entry point to fulfill.
    if let Some(trigger) = restart_trigger {
        write_daemon_restart_request(data_root, trigger, &handoff_id)?;
    }
    write_daemon_upgrade_handoff(data_root, &handoff_id, "preparing", None)?;
    let handoff = DaemonUpgradeHandoff {
        data_root: data_root.to_path_buf(),
        handoff_id,
        restart_trigger,
        release_on_drop: true,
    };
    let deadline = Instant::now() + DAEMON_UPGRADE_STOP_TIMEOUT;
    while daemon_lock_is_active(data_root) {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out waiting for the ctx daemon to stop before upgrade"
            ));
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    }
    write_daemon_upgrade_handoff(data_root, &handoff.handoff_id, "ready", None)?;
    handoff.wait_for_installation_quiescence()?;
    Ok(handoff)
}

/// Fence new daemon starts while the daemon that owns `data_root` is still
/// quiescing. Unlike the manual path, this must not wait for the daemon lock:
/// the caller is that daemon and will release the lock only after this fence is
/// durable.
pub(crate) fn begin_current_daemon_upgrade_handoff(
    data_root: &Path,
    upgrade_attempt_id: &str,
    restart_trigger: DaemonTriggerCommandArg,
) -> Result<DaemonUpgradeHandoff> {
    if !daemon_lock_is_active(data_root) {
        return Err(anyhow!(
            "automatic upgrade handoff requires current daemon ownership"
        ));
    }
    if !crate::upgrade::is_valid_upgrade_attempt_id(upgrade_attempt_id) {
        return Err(anyhow!(
            "invalid upgrade attempt identity for daemon handoff"
        ));
    }
    if daemon_upgrade_handoff_is_active(data_root) {
        let current = read_daemon_upgrade_handoff(data_root)
            .ok_or_else(|| anyhow!("active daemon handoff disappeared"))?;
        if current.get("handoff_id").and_then(Value::as_str) != Some(upgrade_attempt_id)
            || !current_process_owns_daemon_upgrade_handoff(data_root)
        {
            return Err(anyhow!(
                "another ctx upgrade owns the daemon lifecycle handoff"
            ));
        }
        return Ok(DaemonUpgradeHandoff {
            data_root: data_root.to_path_buf(),
            handoff_id: upgrade_attempt_id.to_owned(),
            restart_trigger: Some(restart_trigger),
            release_on_drop: true,
        });
    }
    write_daemon_restart_request(data_root, restart_trigger, upgrade_attempt_id)?;
    write_daemon_upgrade_handoff(data_root, upgrade_attempt_id, "ready", None)?;
    Ok(DaemonUpgradeHandoff {
        data_root: data_root.to_path_buf(),
        handoff_id: upgrade_attempt_id.to_owned(),
        restart_trigger: Some(restart_trigger),
        release_on_drop: true,
    })
}

fn pause_after_installation_quiescence_for_test() -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }
    let Some(path) = env::var_os("CTX_UPGRADE_PAUSE_AFTER_QUIESCENCE_FOR_TESTS") else {
        return Ok(());
    };
    let path = PathBuf::from(path);
    fs::write(&path, b"ready\n")?;
    let release = path.with_extension("continue");
    let deadline = Instant::now() + StdDuration::from_secs(15);
    while !release.exists() {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out waiting to continue after test installation quiescence"
            ));
        }
        std::thread::sleep(StdDuration::from_millis(25));
    }
    Ok(())
}

/// Make helper ownership durable before its parent accepts the readiness
/// receipt. This closes the parent-exit window in which a live replacement
/// helper could otherwise lose the daemon-start fence.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn mark_replacement_helper_handoff(
    data_root: &Path,
    handoff_id: &str,
    helper_pid: u32,
) -> Result<()> {
    if helper_pid == 0 {
        return Err(anyhow!("replacement helper PID must be nonzero"));
    }
    let current = read_daemon_upgrade_handoff(data_root)
        .ok_or_else(|| anyhow!("replacement helper has no daemon handoff"))?;
    if current.get("handoff_id").and_then(Value::as_str) != Some(handoff_id) {
        return Err(anyhow!(
            "replacement helper daemon handoff identity does not match"
        ));
    }
    write_daemon_upgrade_handoff(data_root, handoff_id, "scheduled", Some(helper_pid))
}

/// Complete a durable replacement handoff from the Windows helper.
///
/// The helper passes the origin-root identity and daemon parameters captured
/// before the old daemon stopped. Success means either no daemon had been
/// running, or the replacement process owns the existing daemon lifecycle
/// lock; a successful `spawn` alone is never treated as readiness.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn complete_replacement_daemon_handoff(
    data_root: &Path,
    executable: &Path,
    handoff_id: &str,
    restart: Option<(&str, u64, u64)>,
) -> Result<()> {
    if let Some(current) = read_daemon_upgrade_handoff(data_root) {
        if current.get("handoff_id").and_then(Value::as_str) != Some(handoff_id) {
            return Err(anyhow!(
                "replacement daemon handoff identity does not match its install journal"
            ));
        }
    }
    let captured_restart = if let Some((trigger, idle_exit, loop_interval)) = restart {
        Some((
            parse_daemon_trigger(Some(trigger))
                .ok_or_else(|| anyhow!("replacement daemon handoff has an invalid trigger"))?,
            idle_exit,
            loop_interval,
        ))
    } else {
        None
    };
    let requested_trigger = read_daemon_restart_request(data_root).map(|(_path, trigger)| trigger);
    if let Some(trigger) = captured_restart
        .map(|(trigger, _, _)| trigger)
        .or(requested_trigger)
    {
        if !daemon_lock_is_active(data_root) {
            // Recreate the durable acknowledgement token if an earlier ready
            // daemon consumed it and then exited before handoff completion.
            if read_daemon_restart_request(data_root).is_none() {
                write_daemon_restart_request(data_root, trigger, handoff_id)?;
            }
            let mut command = if let Some((_trigger, idle_exit, loop_interval)) = captured_restart {
                daemon_autostart_command(
                    executable,
                    data_root,
                    trigger,
                    idle_exit,
                    loop_interval,
                    Some(handoff_id),
                )
            } else {
                configured_daemon_autostart_command(
                    executable,
                    data_root,
                    trigger,
                    Some(handoff_id),
                )
            };
            let mut child = command
                .spawn()
                .context("restart ctx daemon after replacement")?;
            wait_for_replacement_daemon(data_root, &mut child)?;
        } else {
            wait_for_daemon_ready_ack(data_root)?;
        }
        if !daemon_lock_is_active(data_root) || read_daemon_restart_request(data_root).is_some() {
            return Err(anyhow!(
                "replacement ctx daemon did not reach lifecycle readiness"
            ));
        }
    }
    Ok(())
}

/// Mark the helper-owned handoff complete only after its terminal journal is
/// durable and its installation lock has been released.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn finish_replacement_daemon_handoff(data_root: &Path, handoff_id: &str) -> Result<()> {
    if read_daemon_upgrade_handoff(data_root)
        .as_ref()
        .and_then(|value| value.get("handoff_id").and_then(Value::as_str))
        != Some(handoff_id)
    {
        return Ok(());
    }
    write_daemon_upgrade_handoff(data_root, handoff_id, "completed", None)
}

pub(crate) fn maybe_autostart_daemon(
    data_root: &Path,
    config: &AppConfig,
    trigger: DaemonTriggerCommandArg,
) {
    maybe_autostart_daemon_inner(data_root, config, trigger);
}

pub(crate) fn maybe_autostart_daemon_for_search(data_root: &Path, config: &AppConfig) {
    maybe_autostart_daemon_inner(data_root, config, DaemonTriggerCommandArg::Search);
}

pub(super) fn maybe_autostart_daemon_inner(
    data_root: &Path,
    config: &AppConfig,
    trigger: DaemonTriggerCommandArg,
) {
    let _ = request_daemon_autostart(data_root, config, trigger);
}

pub(crate) fn daemon_autostart_suppression_reason() -> Option<&'static str> {
    if semantic_env_flag(DAEMON_BACKGROUND_CHILD_ENV) {
        Some("daemon_child")
    } else if semantic_env_flag("CI") {
        Some("ci")
    } else if semantic_env_flag(DAEMON_AUTOSTART_OFF_ENV) {
        Some("autostart_disabled")
    } else {
        None
    }
}

pub(crate) fn autostart_daemon_and_wait(
    data_root: &Path,
    config: &AppConfig,
    trigger: DaemonTriggerCommandArg,
) -> Result<DaemonHandoff> {
    let request = request_daemon_autostart(data_root, config, trigger).map_err(|error| {
        anyhow!(
            "ctx daemon did not start: {error:#}. Run `ctx daemon status --json`, then `ctx daemon run` for details"
        )
    })?;
    let (mut child, pending_restart_request) = match request {
        DaemonAutostartRequest::Existing => (None, None),
        DaemonAutostartRequest::Deferred(path) => (None, Some(path)),
        DaemonAutostartRequest::Spawned(child) => (Some(child), None),
        DaemonAutostartRequest::Suppressed(reason) => {
            return Err(anyhow!(
                "ctx daemon start was suppressed ({reason}); retry after it clears or run `ctx setup --no-daemon`"
            ));
        }
    };
    let expected_failure_pid = child.as_ref().map(Child::id);
    wait_for_daemon_handoff_with(
        DAEMON_SETUP_HANDOFF_POLL_ATTEMPTS,
        || {
            if pending_restart_request
                .as_ref()
                .is_some_and(|path| path.exists())
            {
                DaemonHandoffObservation::Pending
            } else {
                daemon_handoff_observation(data_root, expected_failure_pid)
            }
        },
        || {
            let Some(child) = child.as_mut() else {
                return Ok(None);
            };
            let Some(exit) = child.try_wait()? else {
                return Ok(None);
            };
            let detail = read_daemon_status(data_root)
                .and_then(|status| {
                    (status.get("pid").and_then(Value::as_u64)
                        == Some(u64::from(child.id())))
                    .then(|| {
                        status
                            .get("last_error")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .flatten()
                })
                .unwrap_or_else(|| format!("daemon process exited with {exit}"));
            Ok(Some(detail))
        },
        || std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL),
    )
    .map_err(|error| {
        anyhow!(
            "ctx daemon did not become ready: {error}. Run `ctx daemon status --json`, then `ctx daemon run` for details"
        )
    })
}

fn request_daemon_autostart(
    data_root: &Path,
    config: &AppConfig,
    trigger: DaemonTriggerCommandArg,
) -> Result<DaemonAutostartRequest> {
    if let Some(reason) = daemon_autostart_suppression_reason() {
        return Ok(DaemonAutostartRequest::Suppressed(reason));
    }
    if !daemon_autostart_allowed(data_root, config) {
        return Ok(DaemonAutostartRequest::Suppressed("not_allowed"));
    }
    if crate::upgrade::installation_upgrade_is_active().unwrap_or(false) {
        return Ok(DaemonAutostartRequest::Suppressed(
            "installation_upgrade_active",
        ));
    }
    if daemon_upgrade_handoff_is_active(data_root) {
        let request =
            write_daemon_restart_request(data_root, trigger, &Uuid::now_v7().to_string())?;
        return Ok(DaemonAutostartRequest::Deferred(request));
    }
    let lock_path = daemon_lock_path(data_root);
    if lock_path.exists() && !daemon_lock_is_stale(&lock_path) {
        return Ok(DaemonAutostartRequest::Existing);
    }
    let exe = match daemon_autostart_exe() {
        Ok(exe) => exe,
        Err(error) => {
            let _ = write_daemon_autostart_status(
                data_root,
                trigger,
                "failed",
                Some("current_exe"),
                Some(format!("{error:#}")),
                None,
            );
            return Err(error);
        }
    };
    match configured_daemon_autostart_command(&exe, data_root, trigger, None).spawn() {
        Ok(child) => Ok(DaemonAutostartRequest::Spawned(child)),
        Err(error) => {
            let _ = write_daemon_autostart_status(
                data_root,
                trigger,
                "failed",
                Some("spawn_failed"),
                Some(error.to_string()),
                None,
            );
            Err(error).context("spawn ctx daemon")
        }
    }
}

fn daemon_handoff_observation(
    data_root: &Path,
    expected_failure_pid: Option<u32>,
) -> DaemonHandoffObservation {
    let status = read_daemon_status(data_root);
    let lock_pid = super::paths_status::read_pid_lock_file(&daemon_lock_path(data_root));
    let lock_active = lock_pid.is_some_and(|pid| daemon_lock_is_owned_by(data_root, pid));
    daemon_handoff_observation_from(
        status.as_ref(),
        lock_pid,
        lock_active,
        expected_failure_pid,
        utc_now().timestamp_millis(),
    )
}

fn daemon_handoff_observation_from(
    status: Option<&Value>,
    lock_pid: Option<u32>,
    lock_active: bool,
    expected_failure_pid: Option<u32>,
    now_ms: i64,
) -> DaemonHandoffObservation {
    let Some(status) = status else {
        return DaemonHandoffObservation::Pending;
    };
    let status_name = status.get("status").and_then(Value::as_str);
    let status_pid = status
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok());
    let last_error = || {
        status
            .get("last_error")
            .and_then(Value::as_str)
            .unwrap_or("daemon startup failed")
            .to_owned()
    };
    let heartbeat_is_fresh = || {
        status
            .get("heartbeat_at_ms")
            .and_then(Value::as_i64)
            .is_some_and(|heartbeat| {
                heartbeat > 0
                    && now_ms.saturating_sub(heartbeat) <= DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS
                    && heartbeat.saturating_sub(now_ms)
                        <= DAEMON_SETUP_HANDOFF_MAX_FUTURE_HEARTBEAT_MS
            })
    };
    if status_name == Some("failed") {
        let belongs_to_request = expected_failure_pid
            .map(|expected| status_pid == Some(expected))
            .unwrap_or_else(|| lock_active && status_pid.is_some() && status_pid == lock_pid);
        if belongs_to_request && heartbeat_is_fresh() {
            return DaemonHandoffObservation::Failed(last_error());
        }
        return DaemonHandoffObservation::Pending;
    }
    if status_name != Some("running")
        || !lock_active
        || status_pid.is_none()
        || status_pid != lock_pid
    {
        return DaemonHandoffObservation::Pending;
    }
    if !heartbeat_is_fresh() {
        return DaemonHandoffObservation::Pending;
    }
    match status
        .get("config_reload")
        .and_then(|reload| reload.get("status"))
        .and_then(Value::as_str)
    {
        Some("failed" | "activation_failed") => {
            let error = status
                .get("config_reload")
                .and_then(|reload| reload.get("last_error"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(last_error);
            return DaemonHandoffObservation::Failed(error);
        }
        Some("pending") => return DaemonHandoffObservation::Pending,
        Some("applied") | None => {}
        Some(_) => return DaemonHandoffObservation::Pending,
    }
    let Some(heartbeat_at_ms) = status
        .get("heartbeat_at_ms")
        .and_then(Value::as_i64)
        .filter(|_| heartbeat_is_fresh())
    else {
        return DaemonHandoffObservation::Pending;
    };
    DaemonHandoffObservation::Running(DaemonHandoff {
        pid: status_pid.unwrap_or_default(),
        heartbeat_at_ms,
    })
}

fn wait_for_daemon_handoff_with(
    attempts: usize,
    mut observe: impl FnMut() -> DaemonHandoffObservation,
    mut child_failure: impl FnMut() -> Result<Option<String>>,
    mut pause: impl FnMut(),
) -> Result<DaemonHandoff> {
    for attempt in 0..attempts {
        match observe() {
            DaemonHandoffObservation::Running(handoff) => return Ok(handoff),
            DaemonHandoffObservation::Failed(error) => return Err(anyhow!(error)),
            DaemonHandoffObservation::Pending => {}
        }
        if let Some(error) = child_failure()? {
            return Err(anyhow!(error));
        }
        if attempt + 1 < attempts {
            pause();
        }
    }
    Err(anyhow!(
        "timed out waiting for running status, heartbeat, and process ownership"
    ))
}

fn daemon_autostart_command(
    exe: &Path,
    data_root: &Path,
    trigger: DaemonTriggerCommandArg,
    idle_exit: u64,
    loop_interval: u64,
    handoff_token: Option<&str>,
) -> Command {
    let mut command = Command::new(exe);
    command
        .arg("--data-root")
        .arg(data_root)
        .arg("daemon")
        .arg("run")
        .arg("--idle-exit-seconds")
        .arg(idle_exit.to_string())
        .arg("--loop-interval-seconds")
        .arg(loop_interval.to_string())
        .arg("--start-mode")
        .arg(DaemonStartModeArg::Auto.as_str())
        .arg("--trigger-command")
        .arg(trigger.as_str())
        .arg("--json")
        .env(DAEMON_BACKGROUND_CHILD_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(token) = handoff_token {
        command.env(DAEMON_UPGRADE_HANDOFF_TOKEN_ENV, token);
    }
    command
}

fn configured_daemon_autostart_command(
    exe: &Path,
    data_root: &Path,
    trigger: DaemonTriggerCommandArg,
    handoff_token: Option<&str>,
) -> Command {
    daemon_autostart_command(
        exe,
        data_root,
        trigger,
        daemon_autostart_u64_env(
            "CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS",
            DAEMON_AUTOSTART_IDLE_EXIT_SECONDS_DEFAULT,
            DAEMON_IDLE_EXIT_SECONDS_CAP,
        ),
        daemon_autostart_u64_env(
            "CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS",
            DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS_DEFAULT,
            3_600,
        ),
        handoff_token,
    )
}

fn daemon_restart_allowed(data_root: &Path) -> Result<bool> {
    Ok(daemon_autostart_allowed(
        data_root,
        &AppConfig::load(data_root)?,
    ))
}

fn daemon_autostart_allowed(data_root: &Path, config: &AppConfig) -> bool {
    config.daemon.enabled
        && database_path(data_root.to_path_buf()).exists()
        && !semantic_env_flag(DAEMON_AUTOSTART_OFF_ENV)
}

fn daemon_restart_trigger(data_root: &Path) -> Option<DaemonTriggerCommandArg> {
    if !daemon_lock_is_active(data_root) {
        return None;
    }
    let trigger = read_daemon_status(data_root).and_then(|status| {
        parse_daemon_trigger(status.get("trigger_command").and_then(Value::as_str))
    });
    trigger.or(Some(DaemonTriggerCommandArg::Search))
}

fn parse_daemon_trigger(value: Option<&str>) -> Option<DaemonTriggerCommandArg> {
    match value {
        Some("setup") => Some(DaemonTriggerCommandArg::Setup),
        Some("import") => Some(DaemonTriggerCommandArg::Import),
        Some("search") => Some(DaemonTriggerCommandArg::Search),
        _ => None,
    }
}

fn restart_acknowledged_installation_daemons(
    executable: &Path,
    attempt_id: &str,
    skip_root: Option<&Path>,
) -> Result<()> {
    for restart in read_installation_daemon_restarts(executable, attempt_id)? {
        if skip_root.is_some_and(|root| root == restart.data_root) {
            let _ = fs::remove_file(restart.registration_path);
            continue;
        }
        if !daemon_restart_allowed(&restart.data_root)? {
            remove_daemon_restart_requests(&restart.data_root);
            let _ = fs::remove_file(restart.registration_path);
            continue;
        }
        if daemon_lock_is_active(&restart.data_root) {
            wait_for_daemon_ready_ack(&restart.data_root)?;
            let _ = fs::remove_file(restart.registration_path);
            continue;
        }
        let mut child = daemon_autostart_command(
            executable,
            &restart.data_root,
            restart.trigger,
            restart.idle_exit_seconds,
            restart.loop_interval_seconds,
            None,
        )
        .spawn()
        .with_context(|| {
            format!(
                "restart ctx daemon for {} after installation upgrade",
                restart.data_root.display()
            )
        })?;
        wait_for_replacement_daemon(&restart.data_root, &mut child)?;
        let _ = fs::remove_file(restart.registration_path);
    }
    Ok(())
}

fn restart_acknowledged_legacy_installation_daemons(
    executable: &Path,
    attempt_id: &str,
    skip_root: Option<&Path>,
) -> Result<()> {
    for restart in read_installation_daemon_restarts(executable, attempt_id)? {
        if skip_root.is_some_and(|root| root == restart.data_root) {
            let _ = fs::remove_file(restart.registration_path);
            continue;
        }
        remove_daemon_restart_requests(&restart.data_root);
        let config = AppConfig::load(&restart.data_root)?;
        if !daemon_autostart_allowed(&restart.data_root, &config) {
            let _ = fs::remove_file(restart.registration_path);
            continue;
        }
        if daemon_lock_is_active(&restart.data_root) {
            let _ = fs::remove_file(restart.registration_path);
            continue;
        }
        clear_legacy_daemon_readiness(&restart.data_root)?;
        let mut child = daemon_autostart_command(
            executable,
            &restart.data_root,
            restart.trigger,
            restart.idle_exit_seconds,
            restart.loop_interval_seconds,
            None,
        )
        .spawn()
        .with_context(|| {
            format!(
                "restart legacy ctx daemon for {} after installation recovery",
                restart.data_root.display()
            )
        })?;
        wait_for_legacy_replacement_daemon(
            &restart.data_root,
            &mut child,
            restart.trigger,
            config.semantic_search_enabled() && super::semantic_query_service_supported(),
        )?;
        let _ = fs::remove_file(restart.registration_path);
    }
    Ok(())
}

pub(super) fn resume_completed_installation_daemons(data_root: &Path) -> Result<()> {
    if current_process_owns_daemon_upgrade_handoff(data_root) {
        return Ok(());
    }
    if crate::upgrade::installation_upgrade_is_active()? {
        return Ok(());
    }
    let Some(attempt_id) = crate::upgrade::terminal_installation_upgrade_attempt_id()? else {
        return Ok(());
    };
    let executable = crate::upgrade::installation_executable_path()?;
    restart_acknowledged_installation_daemons(&executable, &attempt_id, Some(data_root))
}

fn write_daemon_upgrade_handoff(
    data_root: &Path,
    handoff_id: &str,
    phase: &str,
    helper_pid: Option<u32>,
) -> Result<()> {
    write_private_json_file(
        &daemon_upgrade_handoff_path(data_root),
        &compact_json(json!({
            "schema_version": 1,
            "handoff_id": handoff_id,
            "phase": phase,
            "owner_pid": process::id(),
            "helper_pid": helper_pid,
            "updated_at_ms": utc_now().timestamp_millis(),
        })),
    )
}

pub(super) fn write_daemon_restart_request(
    data_root: &Path,
    trigger: DaemonTriggerCommandArg,
    request_id: &str,
) -> Result<PathBuf> {
    let path = daemon_upgrade_restart_request_root(data_root).join(format!("{request_id}.json"));
    write_private_json_file(
        &path,
        &compact_json(json!({
            "schema_version": 1,
            "request_id": request_id,
            "trigger_command": trigger.as_str(),
            "requester_pid": process::id(),
            "requested_at_ms": utc_now().timestamp_millis(),
        })),
    )?;
    Ok(path)
}

pub(super) fn read_daemon_restart_request(
    data_root: &Path,
) -> Option<(PathBuf, DaemonTriggerCommandArg)> {
    let mut paths = fs::read_dir(daemon_upgrade_restart_request_root(data_root))
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect::<Vec<_>>();
    paths.sort();
    paths.into_iter().find_map(|path| {
        let text = fs::read_to_string(&path).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        let trigger = parse_daemon_trigger(value.get("trigger_command").and_then(Value::as_str))?;
        Some((path, trigger))
    })
}

fn remove_daemon_restart_requests(data_root: &Path) {
    let root = daemon_upgrade_restart_request_root(data_root);
    if let Ok(entries) = fs::read_dir(&root) {
        for path in entries.filter_map(|entry| entry.ok().map(|entry| entry.path())) {
            let _ = fs::remove_file(path);
        }
    }
    let _ = fs::remove_dir(root);
}

pub(super) fn acknowledge_daemon_restart_requests(data_root: &Path) {
    remove_daemon_restart_requests(data_root);
}

fn wait_for_replacement_daemon(data_root: &Path, child: &mut Child) -> Result<()> {
    let deadline = Instant::now() + DAEMON_UPGRADE_RESTART_TIMEOUT;
    loop {
        if daemon_lock_is_owned_by(data_root, child.id())
            && read_daemon_restart_request(data_root).is_none()
        {
            return Ok(());
        }
        if child.try_wait()?.is_some() {
            return Err(anyhow!(
                "replacement ctx daemon exited before acquiring lifecycle ownership"
            ));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!(
                "timed out waiting for the replacement ctx daemon to start"
            ));
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    }
}

fn wait_for_legacy_replacement_daemon(
    data_root: &Path,
    child: &mut Child,
    trigger: DaemonTriggerCommandArg,
    semantic_ready_required: bool,
) -> Result<()> {
    let deadline = Instant::now() + DAEMON_UPGRADE_RESTART_TIMEOUT;
    let mut ready_on_previous_poll = false;
    loop {
        let ready = daemon_lock_is_owned_by(data_root, child.id())
            && legacy_daemon_status_is_ready(data_root, child.id(), trigger)
            && (!semantic_ready_required
                || legacy_daemon_query_service_is_ready(data_root, child.id()));
        if ready && ready_on_previous_poll {
            return Ok(());
        }
        ready_on_previous_poll = ready;
        if child.try_wait()?.is_some() {
            return Err(anyhow!(
                "legacy replacement ctx daemon exited before lifecycle readiness"
            ));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!(
                "timed out waiting for legacy replacement ctx daemon"
            ));
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    }
}

fn clear_legacy_daemon_readiness(data_root: &Path) -> Result<()> {
    for path in [
        daemon_status_path(data_root),
        daemon_query_endpoint_path(data_root),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("clear stale daemon readiness {}", path.display()));
            }
        }
    }
    Ok(())
}

fn legacy_daemon_status_is_ready(
    data_root: &Path,
    child_pid: u32,
    trigger: DaemonTriggerCommandArg,
) -> bool {
    read_daemon_status(data_root).is_some_and(|status| {
        status.get("status").and_then(Value::as_str) == Some("running")
            && status.get("pid").and_then(Value::as_u64) == Some(u64::from(child_pid))
            && status.get("start_mode").and_then(Value::as_str)
                == Some(DaemonStartModeArg::Auto.as_str())
            && status.get("trigger_command").and_then(Value::as_str) == Some(trigger.as_str())
    })
}

fn legacy_daemon_query_endpoint_is_ready(data_root: &Path, child_pid: u32) -> bool {
    fs::read_to_string(daemon_query_endpoint_path(data_root))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|endpoint| {
            endpoint.get("schema_version").and_then(Value::as_u64) == Some(1)
                && endpoint.get("pid").and_then(Value::as_u64) == Some(u64::from(child_pid))
                && endpoint
                    .get("token")
                    .and_then(Value::as_str)
                    .is_some_and(|token| token.len() >= 32)
        })
}

fn legacy_daemon_query_service_is_ready(data_root: &Path, child_pid: u32) -> bool {
    legacy_daemon_query_endpoint_is_ready(data_root, child_pid)
        && super::query_service::daemon_query_service_available(data_root)
}

fn wait_for_daemon_ready_ack(data_root: &Path) -> Result<()> {
    let deadline = Instant::now() + DAEMON_UPGRADE_RESTART_TIMEOUT;
    loop {
        if daemon_lock_is_active(data_root) && read_daemon_restart_request(data_root).is_none() {
            return Ok(());
        }
        if !daemon_lock_is_active(data_root) {
            return Err(anyhow!(
                "replacement ctx daemon exited before lifecycle readiness"
            ));
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out waiting for replacement ctx daemon readiness"
            ));
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    }
}

#[cfg(test)]
#[path = "daemon_autostart/tests.rs"]
mod telemetry_tests;

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{self, Child, Command, Stdio},
    time::{Duration as StdDuration, Instant, SystemTime},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::{database_path, utc_now};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{compact_json, config::AppConfig, DaemonStartModeArg, DaemonTriggerCommandArg};

use super::{
    health_search::{create_private_dir_all, secure_private_file_permissions, semantic_env_flag},
    paths_status::{
        daemon_lock_is_active, daemon_lock_is_owned_by, daemon_lock_is_stale, daemon_lock_path,
        daemon_root_path, daemon_status_path, open_or_create_pid_lock_file, process_state,
        read_daemon_status, write_daemon_status, write_private_json_file, ProcessState,
    },
    runtime_limits::{
        DAEMON_AUTOSTART_IDLE_EXIT_SECONDS_DEFAULT, DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS_DEFAULT,
        DAEMON_AUTOSTART_OFF_ENV, DAEMON_BACKGROUND_CHILD_ENV, DAEMON_IDLE_EXIT_SECONDS_CAP,
        DAEMON_QUERY_ENDPOINT_FILE,
    },
};
