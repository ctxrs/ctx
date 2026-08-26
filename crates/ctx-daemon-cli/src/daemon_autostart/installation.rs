use super::*;

pub(crate) struct InstallationDaemonLease {
    pub(super) lock: fs::File,
    pub(super) registration_path: PathBuf,
    pub(super) registration_id: String,
    pub(super) data_root: PathBuf,
    pub(super) trigger: DaemonTriggerCommandArg,
    pub(super) loop_interval_seconds: Option<u64>,
    pub(super) persistent: bool,
    pub(super) status: &'static str,
}

#[derive(Debug)]
pub(super) struct InstallationDaemonRestart {
    pub(super) registration_path: PathBuf,
    pub(super) data_root: PathBuf,
    pub(super) trigger: DaemonTriggerCommandArg,
    pub(super) loop_interval_seconds: Option<u64>,
}

pub(super) type InstallationDaemonQuiescence = ctx_daemon_runtime::InstallationQuiescence;

impl InstallationDaemonLease {
    pub(crate) fn acquire(
        data_root: &Path,
        trigger: DaemonTriggerCommandArg,
        loop_interval_seconds: Option<u64>,
        allow_active_upgrade: bool,
        persistent: bool,
    ) -> Result<Option<Self>> {
        let lock = open_installation_daemon_quiescence_lock()?;
        match fs2::FileExt::try_lock_shared(&lock) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => {
                return Err(error).context("acquire ctx installation daemon lease");
            }
        }
        if installation_daemon_admission_is_fenced(allow_active_upgrade)? {
            let _ = fs2::FileExt::unlock(&lock);
            return Ok(None);
        }
        let (_, registration_root) = ctx_upgrade_engine::installation_daemon_coordination_paths()?;
        ctx_history_platform::platform_security::create_private_directory_all(&registration_root)
            .with_context(|| {
            format!(
                "create private ctx installation daemon registrations {}",
                registration_root.display()
            )
        })?;
        let registration_id = Uuid::now_v7().to_string();
        let registration_path = registration_root.join(format!("{registration_id}.json"));
        let mut lease = Self {
            lock,
            registration_path,
            registration_id,
            data_root: data_root.to_path_buf(),
            trigger,
            loop_interval_seconds,
            persistent,
            status: "live",
        };
        lease.write_status("live", None)?;
        if installation_daemon_admission_is_fenced(allow_active_upgrade)? {
            lease.status = "removed";
            let _ = fs::remove_file(&lease.registration_path);
            return Ok(None);
        }
        Ok(Some(lease))
    }

    pub(crate) fn acknowledge(mut self, attempt_id: &str) -> Result<()> {
        self.status = "quiescing";
        self.write_status("quiescing", Some(attempt_id))?;
        write_daemon_restart_request(self.data_root.as_path(), self.trigger, attempt_id)?;
        self.write_status("acknowledged", Some(attempt_id))?;
        self.status = "acknowledged";
        Ok(())
    }

    pub(super) fn write_status(&self, status: &str, attempt_id: Option<&str>) -> Result<()> {
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
                "persistent": self.persistent,
                "loop_interval_explicit": self.loop_interval_seconds.is_some(),
                "loop_interval_seconds": self.loop_interval_seconds.unwrap_or(15 * 60),
                "updated_at_ms": utc_now().timestamp_millis(),
            })),
        )
    }
}

fn installation_daemon_admission_is_fenced(allow_active_upgrade: bool) -> Result<bool> {
    Ok(
        ctx_upgrade_engine::installation_hosted_uninstall_is_active()?
            || (!allow_active_upgrade && ctx_upgrade_engine::installation_upgrade_is_active()?),
    )
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
    let (path, _) = ctx_upgrade_engine::installation_daemon_coordination_paths()?;
    create_installation_daemon_coordination_parent(&path)?;
    open_installation_daemon_quiescence_lock_at(&path)
}

pub(super) fn try_acquire_installation_daemon_quiescence(
) -> Result<Option<InstallationDaemonQuiescence>> {
    let (path, _) = ctx_upgrade_engine::installation_daemon_coordination_paths()?;
    create_installation_daemon_coordination_parent(&path)?;
    ctx_daemon_runtime::try_acquire_installation_quiescence(&path)
}

fn create_installation_daemon_coordination_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        anyhow!(
            "ctx installation daemon coordination has no parent: {}",
            path.display()
        )
    })?;
    ctx_history_platform::platform_security::create_private_directory_all(parent).with_context(
        || {
            format!(
                "create private ctx installation daemon coordination {}",
                parent.display()
            )
        },
    )
}

pub(super) fn open_installation_daemon_quiescence_lock_at(path: &Path) -> Result<fs::File> {
    ctx_daemon_runtime::open_installation_quiescence_lock(path)
}

pub(super) fn wait_for_installation_daemon_quiescence_for(
    executable: &Path,
    attempt_id: &str,
) -> Result<()> {
    let (lock_path, registration_root) =
        ctx_upgrade_engine::installation_daemon_coordination_paths_for(executable)?;
    create_installation_daemon_coordination_parent(&lock_path)?;
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
    ctx_daemon_runtime::wait_for_installation_quiescence(
        lock_path,
        registration_root,
        attempt_id,
        timeout,
        DAEMON_UPGRADE_POLL_INTERVAL,
        3_600,
    )
}

pub(super) fn read_installation_daemon_restarts(
    executable: &Path,
    attempt_id: &str,
) -> Result<Vec<InstallationDaemonRestart>> {
    let (_, root) = ctx_upgrade_engine::installation_daemon_coordination_paths_for(executable)?;
    read_installation_daemon_restarts_from(&root, attempt_id, false)
}

pub(super) fn read_installation_daemon_restarts_from(
    root: &Path,
    attempt_id: &str,
    fail_on_live: bool,
) -> Result<Vec<InstallationDaemonRestart>> {
    ctx_daemon_runtime::read_installation_restart_records(root, attempt_id, fail_on_live, 3_600)?
        .into_iter()
        .map(|record| {
            Ok(InstallationDaemonRestart {
                registration_path: record.registration_path,
                data_root: record.data_root,
                trigger: parse_daemon_trigger(Some(&record.opaque_trigger))
                    .ok_or_else(|| anyhow!("ctx daemon acknowledgement has an invalid trigger"))?,
                loop_interval_seconds: record.loop_interval_seconds,
            })
        })
        .collect()
}

fn read_installation_daemon_registrations_from(root: &Path) -> Result<Vec<(PathBuf, Value)>> {
    ctx_daemon_runtime::read_installation_registrations(root)
}

pub(super) fn remove_installation_daemon_coordination() -> Result<()> {
    let (_, root) = ctx_upgrade_engine::installation_daemon_coordination_paths()?;
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
    let (_, root) = ctx_upgrade_engine::installation_daemon_coordination_paths()?;
    registered_installation_daemon_roots_from(&root)
}

pub(super) fn registered_installation_daemon_roots_from(root: &Path) -> Result<Vec<PathBuf>> {
    ctx_daemon_runtime::registered_installation_roots(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_registration_writes_only_explicit_persistent_restart_policy() {
        let temp = tempfile::tempdir().unwrap();
        let registration_path = temp.path().join("registration.json");
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(temp.path().join("lock"))
            .unwrap();
        let lease = InstallationDaemonLease {
            lock,
            registration_path: registration_path.clone(),
            registration_id: "registration".to_owned(),
            data_root: temp.path().join("data"),
            trigger: DaemonTriggerCommandArg::Search,
            loop_interval_seconds: Some(23),
            persistent: true,
            status: "live",
        };

        lease.write_status("live", None).unwrap();

        let value: Value = serde_json::from_slice(&fs::read(registration_path).unwrap()).unwrap();
        assert_eq!(value["persistent"], Value::Bool(true));
        assert_eq!(value["loop_interval_explicit"], Value::Bool(true));
        assert_eq!(value["loop_interval_seconds"], Value::from(23));
        assert!(value.get("idle_exit_seconds").is_none());
    }

    #[test]
    fn finite_registration_is_discoverable_without_persistent_restart_policy() {
        let temp = tempfile::tempdir().unwrap();
        let registration_path = temp.path().join("registration.json");
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(temp.path().join("lock"))
            .unwrap();
        let lease = InstallationDaemonLease {
            lock,
            registration_path: registration_path.clone(),
            registration_id: "finite-registration".to_owned(),
            data_root: temp.path().join("data"),
            trigger: DaemonTriggerCommandArg::Search,
            loop_interval_seconds: None,
            persistent: false,
            status: "live",
        };

        lease.write_status("live", None).unwrap();

        let value: Value = serde_json::from_slice(&fs::read(registration_path).unwrap()).unwrap();
        assert_eq!(value["persistent"], Value::Bool(false));
        assert_eq!(value["data_root"], json!(temp.path().join("data")));
        assert_eq!(value["loop_interval_explicit"], Value::Bool(false));
    }
}
