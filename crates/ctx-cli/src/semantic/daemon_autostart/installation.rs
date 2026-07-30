use super::*;

pub(in crate::semantic) struct InstallationDaemonLease {
    pub(super) lock: fs::File,
    pub(super) registration_path: PathBuf,
    pub(super) registration_id: String,
    pub(super) data_root: PathBuf,
    pub(super) trigger: DaemonTriggerCommandArg,
    pub(super) idle_exit_seconds: Option<u64>,
    pub(super) loop_interval_seconds: Option<u64>,
    pub(super) status: &'static str,
}

#[derive(Debug)]
pub(super) struct InstallationDaemonRestart {
    pub(super) registration_path: PathBuf,
    pub(super) data_root: PathBuf,
    pub(super) trigger: DaemonTriggerCommandArg,
    pub(super) idle_exit_seconds: Option<u64>,
    pub(super) loop_interval_seconds: Option<u64>,
}

pub(super) struct InstallationDaemonQuiescence {
    lock: fs::File,
}

impl InstallationDaemonLease {
    pub(in crate::semantic) fn acquire(
        data_root: &Path,
        trigger: DaemonTriggerCommandArg,
        idle_exit_seconds: Option<u64>,
        loop_interval_seconds: Option<u64>,
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

    pub(in crate::semantic) fn acknowledge(mut self, attempt_id: &str) -> Result<()> {
        self.status = "quiescing";
        self.write_status("quiescing", Some(attempt_id))?;
        write_daemon_restart_request(self.data_root.as_path(), self.trigger, attempt_id)?;
        self.write_status("acknowledged", Some(attempt_id))?;
        self.status = "acknowledged";
        Ok(())
    }

    pub(super) fn write_status(&self, status: &str, attempt_id: Option<&str>) -> Result<()> {
        // Explicit booleans carry the persistent contract. Numeric values are
        // retained only as bounded receipt fields for current forward upgrade
        // recovery and are never interpreted as implicit production exits.
        let persistent = self.idle_exit_seconds.is_none();
        let loop_interval_explicit = self.loop_interval_seconds.is_some();
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
                "persistent": persistent,
                "loop_interval_explicit": loop_interval_explicit,
                "idle_exit_seconds": self.idle_exit_seconds
                    .unwrap_or(DAEMON_IDLE_EXIT_SECONDS_CAP),
                "loop_interval_seconds": self.loop_interval_seconds
                    .unwrap_or(15 * 60),
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

pub(super) fn try_acquire_installation_daemon_quiescence(
) -> Result<Option<InstallationDaemonQuiescence>> {
    let lock = open_installation_daemon_quiescence_lock()?;
    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => Ok(Some(InstallationDaemonQuiescence { lock })),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error).context("acquire ctx installation daemon quiescence"),
    }
}

impl Drop for InstallationDaemonQuiescence {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.lock);
    }
}

pub(super) fn open_installation_daemon_quiescence_lock_at(path: &Path) -> Result<fs::File> {
    let (file, _) = open_or_create_pid_lock_file(path)
        .with_context(|| format!("open ctx installation daemon lock {}", path.display()))?;
    secure_private_file_permissions(path)?;
    Ok(file)
}

pub(super) fn wait_for_installation_daemon_quiescence_for(
    executable: &Path,
    attempt_id: &str,
) -> Result<()> {
    let (lock_path, registration_root) =
        crate::upgrade::installation_daemon_coordination_paths_for(executable);
    wait_for_installation_daemon_quiescence_at(
        &lock_path,
        &registration_root,
        attempt_id,
        DAEMON_INSTALLATION_QUIESCE_TIMEOUT,
    )
}

pub(super) fn wait_for_installation_daemon_quiescence_at(
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

pub(super) fn read_installation_daemon_restarts(
    executable: &Path,
    attempt_id: &str,
) -> Result<Vec<InstallationDaemonRestart>> {
    let (_, root) = crate::upgrade::installation_daemon_coordination_paths_for(executable);
    read_installation_daemon_restarts_from(&root, attempt_id, false)
}

pub(super) fn read_installation_daemon_restarts_from(
    root: &Path,
    attempt_id: &str,
    fail_on_live: bool,
) -> Result<Vec<InstallationDaemonRestart>> {
    let registrations = read_installation_daemon_registrations_from(root)?;
    let mut restarts = Vec::new();
    for (path, value) in registrations {
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
        let persistent = value
            .get("persistent")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let idle_exit_seconds = if persistent {
            None
        } else {
            Some(
                value["idle_exit_seconds"]
                    .as_u64()
                    .filter(|value| *value > 0 && *value <= DAEMON_IDLE_EXIT_SECONDS_CAP)
                    .ok_or_else(|| {
                        anyhow!("ctx daemon acknowledgement has an invalid idle timeout")
                    })?,
            )
        };
        let loop_interval_explicit = value
            .get("loop_interval_explicit")
            .and_then(Value::as_bool)
            .unwrap_or(!persistent);
        let loop_interval_seconds = match value["loop_interval_seconds"].as_u64() {
            Some(value) if value > 0 && value <= 3_600 && loop_interval_explicit => Some(value),
            Some(value) if value > 0 && value <= 3_600 && persistent => None,
            _ => {
                return Err(anyhow!(
                    "ctx daemon acknowledgement has an invalid loop interval"
                ))
            }
        };
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

fn read_installation_daemon_registrations_from(root: &Path) -> Result<Vec<(PathBuf, Value)>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read ctx daemon acknowledgements {}", root.display()));
        }
    };
    let mut registrations = Vec::new();
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
        registrations.push((path, value));
    }
    registrations.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(registrations)
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

pub(super) fn remove_installation_daemon_coordination() -> Result<()> {
    let (_, root) = crate::upgrade::installation_daemon_coordination_paths()?;
    for (path, _) in read_installation_daemon_registrations_from(&root)? {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("remove ctx daemon coordination {}", path.display()))
            }
        }
    }
    let _ = fs::remove_dir(&root);
    Ok(())
}

pub(super) fn registered_installation_daemon_roots() -> Result<Vec<PathBuf>> {
    let (_, root) = crate::upgrade::installation_daemon_coordination_paths()?;
    registered_installation_daemon_roots_from(&root)
}

pub(super) fn registered_installation_daemon_roots_from(root: &Path) -> Result<Vec<PathBuf>> {
    let mut roots = read_installation_daemon_registrations_from(root)?
        .into_iter()
        .map(|(_, value)| PathBuf::from(value["data_root"].as_str().unwrap_or_default()))
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    Ok(roots)
}
