use super::*;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use super::unsupported::*;

pub(super) struct PlatformNativeSupervisor<'a> {
    host: &'a dyn DaemonApplicationHost,
    identity: Option<SupervisorIdentity>,
    data_root: PathBuf,
    daemon_environment: Option<&'a SupervisorEnvironmentSnapshot>,
    manager_environment: &'a SupervisorManagerEnvironment,
}

impl<'a> PlatformNativeSupervisor<'a> {
    pub(super) fn new(
        host: &'a dyn DaemonApplicationHost,
        data_root: &Path,
        daemon_environment: Option<&'a SupervisorEnvironmentSnapshot>,
        manager_environment: &'a SupervisorManagerEnvironment,
    ) -> Result<Self> {
        let identity = native_supervisor_identity(data_root, manager_environment)?;
        Ok(Self {
            host,
            identity,
            data_root: data_root.to_path_buf(),
            daemon_environment,
            manager_environment,
        })
    }

    fn identity(&self) -> Result<&SupervisorIdentity> {
        self.identity
            .as_ref()
            .ok_or_else(|| anyhow!("native supervisor identity is unavailable"))
    }

    fn spec(&self, executable: &Path) -> Result<SupervisorSpec> {
        environment::supervisor_artifact_spec(
            self.identity()?.clone(),
            executable,
            &self.data_root,
            self.launch_environment()?,
        )
    }

    pub(super) fn launch_environment(&self) -> Result<&SupervisorEnvironmentSnapshot> {
        self.daemon_environment.ok_or_else(|| {
            anyhow!(
                "native supervisor launch or verification requires a normalized daemon environment"
            )
        })
    }
}

impl NativeSupervisorBackend<SupervisorEnvironmentSnapshot> for PlatformNativeSupervisor<'_> {
    fn probe_manager(&self, _data_root: &Path) -> Result<SupervisorManagerOperability> {
        #[cfg(target_os = "linux")]
        return ctx_daemon_runtime::probe_systemd_user_manager(self.manager_environment);
        #[cfg(target_os = "macos")]
        return ctx_daemon_runtime::probe_launchd_gui_user_domain(self.manager_environment);
        #[cfg(windows)]
        return ctx_daemon_runtime::probe_windows_task_scheduler(self.manager_environment);
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        Ok(SupervisorManagerOperability::Unavailable {
            reason: native_supervisor_limitation().to_owned(),
        })
    }

    fn prepare_mutation(&self, data_root: &Path, executable: &Path) -> Result<()> {
        lifecycle::handoff_mismatched_daemon_owner(self.host, data_root, executable)
            .context("replace daemon ownership held by a different ctx binary image")
    }

    fn artifact_path(&self, data_root: &Path) -> Result<Option<PathBuf>> {
        let _ = data_root;
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        return Ok(None);
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        Ok(self
            .identity
            .as_ref()
            .map(|identity| identity.artifact_path().to_path_buf()))
    }

    fn install(
        &self,
        data_root: &Path,
        executable: &Path,
        environment: &SupervisorEnvironmentSnapshot,
    ) -> Result<PathBuf> {
        let daemon_environment = self.launch_environment()?;
        debug_assert_eq!(
            environment.values, daemon_environment.values,
            "installation must use the normalized daemon environment"
        );
        let spec = self.spec(executable)?;
        install_native_supervisor(
            self.host,
            data_root,
            executable,
            daemon_environment,
            self.manager_environment,
            &spec,
        )
    }

    fn disable(&self, data_root: &Path) -> Result<Option<PathBuf>> {
        disable_native_supervisor(data_root, self.manager_environment, self.identity()?)
    }

    fn verify_registration(&self, data_root: &Path, executable: &Path) -> Result<()> {
        let spec = self.spec(executable)?;
        verify_native_supervisor_registration(
            data_root,
            executable,
            self.launch_environment()?,
            self.manager_environment,
            &spec,
        )
    }

    fn verify_live_owner(&self, data_root: &Path, executable: &Path) -> Result<u32> {
        let spec = self.spec(executable)?;
        verify_native_supervisor(
            data_root,
            executable,
            self.launch_environment()?,
            self.manager_environment,
            &spec,
        )
    }

    fn prepare_start(&self, data_root: &Path, executable: &Path) -> Result<Option<u32>> {
        // Close the manager-startup window in which the daemon lock exists
        // before manager-specific ownership provenance becomes visible.
        if let Ok(owner_pid) = self.verify_live_owner(data_root, executable) {
            return Ok(Some(owner_pid));
        }
        migrate_existing_daemon_to_supervisor(self.host, data_root)?;
        Ok(None)
    }

    fn start(&self, data_root: &Path) -> Result<()> {
        start_native_supervisor(data_root, self.manager_environment, self.identity()?)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn migrate_existing_daemon_to_supervisor(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
) -> Result<()> {
    if !daemon_lock_is_active(data_root) {
        return Ok(());
    }
    let owner_pid =
        ctx_daemon_runtime::read_pid_lock_json(&ctx_daemon_runtime::daemon_lock_path(data_root))
            .as_ref()
            .and_then(ctx_daemon_runtime::pid_from_lock_json)
            .ok_or_else(|| {
                anyhow!("running daemon has no stable owner PID for supervisor handoff")
            })?;
    let response = host.request_lifecycle_wakeup(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "supervisor_handoff",
        })),
        Duration::from_millis(500),
        16 * 1024,
    )?;
    if response
        .as_ref()
        .and_then(|value| value.get("ok"))
        .and_then(Value::as_bool)
        != Some(true)
        || response
            .as_ref()
            .and_then(|value| value.get("pid"))
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            != Some(owner_pid)
    {
        return Err(anyhow!(
            "running daemon did not accept native-supervisor handoff"
        ));
    }
    let deadline = Instant::now() + SUPERVISOR_HANDOFF_TIMEOUT;
    while daemon_lock_is_active(data_root) {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out waiting for daemon native-supervisor handoff"
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn migrate_existing_daemon_to_supervisor(
    _host: &dyn DaemonApplicationHost,
    data_root: &Path,
) -> Result<()> {
    if daemon_lock_is_active(data_root) {
        return Err(anyhow!(
            "native supervisor handoff is unavailable on this platform"
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn native_supervisor_identity(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Option<SupervisorIdentity>> {
    let name = environment::SYSTEMD_UNIT_NAME;
    let artifact_path = ctx_daemon_runtime::linux_systemd_unit_path(manager_environment, name)?;
    environment::supervisor_identity(name, artifact_path).map(Some)
}

#[cfg(target_os = "macos")]
fn native_supervisor_identity(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Option<SupervisorIdentity>> {
    let name = environment::LAUNCH_AGENT_LABEL;
    let artifact_path = ctx_daemon_runtime::launch_agent_path(manager_environment, name)?;
    environment::supervisor_identity(name, artifact_path).map(Some)
}

#[cfg(windows)]
fn native_supervisor_identity(
    data_root: &Path,
    _manager_environment: &SupervisorManagerEnvironment,
) -> Result<Option<SupervisorIdentity>> {
    let sid = ctx_daemon_runtime::current_windows_user_sid()?;
    environment::windows_supervisor_identity(data_root, &sid).map(Some)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn native_supervisor_identity(
    _data_root: &Path,
    _manager_environment: &SupervisorManagerEnvironment,
) -> Result<Option<SupervisorIdentity>> {
    SupervisorIdentity::new(native_supervisor_kind(), PathBuf::new()).map(Some)
}

#[cfg(target_os = "linux")]
fn install_native_supervisor(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    _executable: &Path,
    _environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<PathBuf> {
    ctx_daemon_runtime::install_systemd_supervisor(
        data_root,
        spec,
        manager_environment,
        &|data_root| migrate_existing_daemon_to_supervisor(host, data_root),
    )
}

#[cfg(target_os = "linux")]
fn disable_native_supervisor(
    data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
    identity: &SupervisorIdentity,
) -> Result<Option<PathBuf>> {
    let artifact = ctx_daemon_runtime::disable_systemd_supervisor(identity, manager_environment)?;
    ctx_daemon_runtime::remove_supervisor_environment(data_root)?;
    Ok(artifact)
}

#[cfg(target_os = "linux")]
fn verify_native_supervisor_registration(
    _data_root: &Path,
    _executable: &Path,
    _daemon_environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<()> {
    ctx_daemon_runtime::verify_systemd_registration(spec, manager_environment)
}

#[cfg(target_os = "linux")]
fn verify_native_supervisor(
    data_root: &Path,
    executable: &Path,
    _daemon_environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<u32> {
    let manager_pid = ctx_daemon_runtime::systemd_live_owner_pid(spec, manager_environment)?;
    ctx_daemon_runtime::verify_daemon_owner_identity(data_root, executable, Some(manager_pid))
}

#[cfg(target_os = "linux")]
fn start_native_supervisor(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
    identity: &SupervisorIdentity,
) -> Result<()> {
    ctx_daemon_runtime::start_systemd_supervisor(identity, manager_environment)
}

#[cfg(target_os = "macos")]
fn install_native_supervisor(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    _executable: &Path,
    _environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<PathBuf> {
    ctx_daemon_runtime::install_launch_agent(data_root, spec, manager_environment, &|data_root| {
        migrate_existing_daemon_to_supervisor(host, data_root)
    })
}

#[cfg(target_os = "macos")]
fn disable_native_supervisor(
    data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
    identity: &SupervisorIdentity,
) -> Result<Option<PathBuf>> {
    let artifact = ctx_daemon_runtime::disable_launch_agent(identity, manager_environment)?;
    ctx_daemon_runtime::remove_supervisor_environment(data_root)?;
    Ok(artifact)
}

#[cfg(target_os = "macos")]
fn verify_native_supervisor_registration(
    _data_root: &Path,
    _executable: &Path,
    _daemon_environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<()> {
    ctx_daemon_runtime::verify_launch_agent_registration(spec, manager_environment)
}

#[cfg(target_os = "macos")]
fn verify_native_supervisor(
    data_root: &Path,
    executable: &Path,
    _daemon_environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<u32> {
    let manager_pid = ctx_daemon_runtime::launch_agent_live_owner_pid(spec, manager_environment)?;
    ctx_daemon_runtime::verify_daemon_owner_identity(data_root, executable, Some(manager_pid))
}

#[cfg(target_os = "macos")]
fn start_native_supervisor(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
    identity: &SupervisorIdentity,
) -> Result<()> {
    ctx_daemon_runtime::start_launch_agent(identity, manager_environment)
}

#[cfg(windows)]
fn install_native_supervisor(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    _executable: &Path,
    _environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<PathBuf> {
    ctx_daemon_runtime::install_windows_supervisor(
        data_root,
        spec,
        manager_environment,
        &|data_root| migrate_existing_daemon_to_supervisor(host, data_root),
    )
}

#[cfg(windows)]
fn disable_native_supervisor(
    data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
    identity: &SupervisorIdentity,
) -> Result<Option<PathBuf>> {
    let artifact = ctx_daemon_runtime::disable_windows_supervisor(identity, manager_environment)?;
    ctx_daemon_runtime::remove_supervisor_environment(data_root)?;
    Ok(artifact)
}

#[cfg(windows)]
fn verify_native_supervisor_registration(
    _data_root: &Path,
    _executable: &Path,
    _daemon_environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<()> {
    ctx_daemon_runtime::verify_windows_supervisor_registration(spec, manager_environment)
}

#[cfg(windows)]
fn verify_native_supervisor(
    data_root: &Path,
    executable: &Path,
    _daemon_environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<u32> {
    ctx_daemon_runtime::verify_windows_supervisor(data_root, executable, spec, manager_environment)
}

#[cfg(windows)]
fn start_native_supervisor(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
    identity: &SupervisorIdentity,
) -> Result<()> {
    ctx_daemon_runtime::start_windows_supervisor(identity, manager_environment)
}
