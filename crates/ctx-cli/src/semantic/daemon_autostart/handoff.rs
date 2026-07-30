use super::*;

mod termination;
pub(super) use termination::terminate_identity_verified_residual_daemon;

pub(in crate::semantic) fn terminate_current_executable_daemon(data_root: &Path) -> Result<()> {
    let executable = env::current_exe().context("resolve current ctx executable")?;
    terminate_identity_verified_residual_daemon(data_root, &executable)
}

fn daemon_upgrade_handoff_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_UPGRADE_HANDOFF_FILE)
}

fn daemon_upgrade_restart_request_root(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_UPGRADE_RESTART_REQUEST_DIR)
}

const DAEMON_UNINSTALL_ABORT_AFTER_DISABLE_ENV: &str =
    "CTX_DAEMON_UNINSTALL_ABORT_AFTER_DISABLE_FOR_TESTS";

pub(super) fn daemon_query_endpoint_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_QUERY_ENDPOINT_FILE)
}

pub(super) fn read_daemon_upgrade_handoff(data_root: &Path) -> Option<Value> {
    let text = fs::read_to_string(daemon_upgrade_handoff_path(data_root)).ok()?;
    serde_json::from_str(&text).ok()
}

pub(super) fn daemon_upgrade_handoff_is_active(data_root: &Path) -> bool {
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

pub(in crate::semantic) fn daemon_upgrade_handoff_blocks_current_process(data_root: &Path) -> bool {
    if !daemon_upgrade_handoff_is_active(data_root) {
        return false;
    }
    !current_process_owns_daemon_upgrade_handoff(data_root)
}

pub(in crate::semantic) fn current_process_owns_daemon_upgrade_handoff(data_root: &Path) -> bool {
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
    installation_executable: PathBuf,
    pub(super) restart_trigger: Option<DaemonTriggerCommandArg>,
    release_on_drop: bool,
}

impl DaemonUpgradeHandoff {
    pub(crate) fn wait_for_installation_quiescence(&self) -> Result<()> {
        wait_for_installation_daemon_quiescence_for(
            &self.installation_executable,
            &self.handoff_id,
        )?;
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
                DAEMON_IDLE_EXIT_SECONDS_CAP,
            )
            .unwrap_or(DAEMON_IDLE_EXIT_SECONDS_CAP),
            daemon_autostart_u64_env("CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS", 3_600)
                .unwrap_or(15 * 60),
        ))
    }

    /// Preserve daemon restart intent while schema-2 recovery re-executes the
    /// identity-validated current-format executable restored at the install
    /// path. The restored process consumes this request while fixing forward.
    pub(crate) fn release_for_current_format_reexec(mut self) -> Result<()> {
        if read_daemon_restart_request(&self.data_root).is_none() {
            if let Some(trigger) = self.restart_trigger {
                write_daemon_restart_request(&self.data_root, trigger, &self.handoff_id)?;
            }
        }
        self.release("aborted", None)?;
        self.release_on_drop = false;
        Ok(())
    }

    /// Release the upgrade fence and restart the current auto-daemon after a
    /// verified forward publication succeeds.
    pub(crate) fn resume_with(mut self, executable: &Path) -> Result<()> {
        let restart_trigger = self
            .restart_trigger
            .or_else(|| read_daemon_restart_request(&self.data_root).map(|(_, trigger)| trigger));
        if daemon_restart_allowed(&self.data_root)? {
            if let Some(trigger) = restart_trigger {
                let data_root = self.data_root.clone();
                let supervisor_resume =
                    super::super::daemon_supervisor::resume_daemon_supervisor_after_upgrade(
                        &data_root,
                        executable,
                        || self.complete_release(),
                    )?;
                match supervisor_resume {
                    super::super::daemon_supervisor::DaemonSupervisorUpgradeResume::Native => {
                        wait_for_daemon_ready_ack(&self.data_root)?;
                    }
                    super::super::daemon_supervisor::DaemonSupervisorUpgradeResume::Fallback => {
                        let mut command = configured_daemon_autostart_command(
                            executable,
                            &self.data_root,
                            trigger,
                            Some(&self.handoff_id),
                        );
                        let mut child = spawn_daemon_child(&mut command)
                            .context("restart ctx daemon after upgrade")?;
                        wait_for_replacement_daemon(&self.data_root, &mut child)?;
                    }
                }
            }
        }
        remove_daemon_restart_requests(&self.data_root);
        restart_acknowledged_installation_daemons(
            executable,
            &self.handoff_id,
            Some(&self.data_root),
        )?;
        if self.release_on_drop {
            self.complete_release()?;
        }
        Ok(())
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

    fn complete_release(&mut self) -> Result<()> {
        self.release("completed", None)?;
        self.release_on_drop = false;
        Ok(())
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
    let expected_executable = env::current_exe().context("resolve upgrading ctx executable")?;
    begin_daemon_upgrade_handoff_for_executable(
        data_root,
        upgrade_attempt_id,
        &expected_executable,
        true,
    )
}

pub(crate) fn begin_legacy_daemon_upgrade_handoff(
    data_root: &Path,
    upgrade_attempt_id: &str,
    expected_executable: &Path,
) -> Result<DaemonUpgradeHandoff> {
    begin_daemon_upgrade_handoff_for_executable(
        data_root,
        upgrade_attempt_id,
        expected_executable,
        false,
    )
}

fn begin_daemon_upgrade_handoff_for_executable(
    data_root: &Path,
    upgrade_attempt_id: &str,
    expected_executable: &Path,
    allow_cooperative_grace: bool,
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
    let _ = daemon_source_refresh_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "lifecycle_wakeup",
        })),
        DAEMON_HEALTH_TIMEOUT,
        DAEMON_HEALTH_RESPONSE_MAX_BYTES,
    );
    let handoff = DaemonUpgradeHandoff {
        data_root: data_root.to_path_buf(),
        handoff_id,
        installation_executable: expected_executable.to_path_buf(),
        restart_trigger,
        release_on_drop: true,
    };
    if !allow_cooperative_grace && daemon_lock_is_active(data_root) {
        terminate_identity_verified_residual_daemon(data_root, expected_executable)
            .context("stop identity-verified legacy ctx daemon before automatic upgrade")?;
    }
    let deadline = Instant::now() + DAEMON_UPGRADE_STOP_TIMEOUT;
    while daemon_lock_is_active(data_root) {
        if Instant::now() >= deadline {
            #[cfg(any(unix, windows))]
            {
                terminate_identity_verified_residual_daemon(data_root, expected_executable)
                    .context("stop residual ctx daemon before upgrade")?;
                break;
            }
            #[cfg(not(any(unix, windows)))]
            return Err(anyhow!(
                "timed out waiting for the ctx daemon to stop before upgrade"
            ));
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    }
    wait_for_daemon_lifecycle_release(data_root)?;
    write_daemon_upgrade_handoff(data_root, &handoff.handoff_id, "ready", None)?;
    handoff.wait_for_installation_quiescence()?;
    Ok(handoff)
}

/// Hosted uninstallers call this command before deleting the installed
/// executable. Each phase is idempotent so an interrupted uninstaller can
/// invoke it again safely.
pub(crate) fn prepare_daemon_uninstall(data_root: &Path) -> Result<Value> {
    let expected_executable =
        env::current_exe().context("resolve installed ctx executable before uninstall")?;
    let canonical_root =
        ctx_history_core::managed_data_root().context("resolve canonical ctx data root")?;
    let mut roots = BTreeSet::from([data_root.to_path_buf(), canonical_root.clone()]);
    let mut disabled_roots = BTreeSet::new();
    discover_and_disable_installation_roots(&mut roots, &mut disabled_roots)?;
    if cfg!(debug_assertions) && env::var_os(DAEMON_UNINSTALL_ABORT_AFTER_DISABLE_ENV).is_some() {
        process::exit(89);
    }

    super::super::daemon_supervisor::disable_daemon_supervisor(&canonical_root)
        .context("remove canonical ctx daemon supervisor before uninstall")?;

    let installation_deadline = Instant::now() + DAEMON_INSTALLATION_QUIESCE_TIMEOUT;
    let installation_quiescence = loop {
        discover_and_disable_installation_roots(&mut roots, &mut disabled_roots)?;
        quiesce_daemon_roots(&roots, &expected_executable)?;
        if let Some(quiescence) = super::installation::try_acquire_installation_daemon_quiescence()?
        {
            break quiescence;
        }
        if Instant::now() >= installation_deadline {
            return Err(anyhow!(
                "timed out waiting for installation-wide ctx daemon quiescence; keep the ctx binary and retry `ctx daemon disable --prepare-uninstall`"
            ));
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    };

    discover_and_disable_installation_roots(&mut roots, &mut disabled_roots)?;
    for root in &roots {
        if daemon_lock_is_active(root) {
            return Err(anyhow!(
                "ctx daemon lifecycle ownership appeared after installation quiescence for {}; keep the ctx binary and retry `ctx daemon disable --prepare-uninstall`",
                root.display()
            ));
        }
    }
    super::installation::remove_installation_daemon_coordination()
        .context("remove installation-wide ctx daemon coordination before uninstall")?;
    for root in &roots {
        remove_daemon_lifecycle_coordination(root)?;
    }
    drop(installation_quiescence);
    let quiesced_roots = roots.into_iter().collect::<Vec<_>>();
    let quiesced_root_count = quiesced_roots.len();
    Ok(compact_json(json!({
        "schema_version": 1,
        "command": "daemon_prepare_uninstall",
        "ok": true,
        "scope": "installation",
        "requested_data_root": data_root,
        "canonical_data_root": canonical_root,
        "quiesced_roots": quiesced_roots,
        "quiesced_root_count": quiesced_root_count,
        "installation_quiescent": true,
        "daemon_enabled": false,
        "daemon_running": false,
        "owner_lock_released": true,
        "endpoint_released": true,
        "supervisor_removed": true,
        "coordination_state_removed": true,
        "binary_retained": true,
        "retry_safe": true,
        "local_only": true,
    })))
}

fn discover_and_disable_installation_roots(
    roots: &mut BTreeSet<PathBuf>,
    disabled_roots: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    roots.extend(super::installation::registered_installation_daemon_roots()?);
    for root in roots.iter() {
        if disabled_roots.insert(root.clone()) {
            crate::config::set_daemon_enabled(root, false).with_context(|| {
                format!(
                    "durably disable ctx daemon root {} before uninstall",
                    root.display()
                )
            })?;
        }
    }
    Ok(())
}

fn quiesce_daemon_roots(roots: &BTreeSet<PathBuf>, expected_executable: &Path) -> Result<()> {
    for root in roots {
        request_disabled_daemon_shutdown(root);
    }
    let cooperative_deadline = Instant::now() + DAEMON_UPGRADE_STOP_TIMEOUT;
    while roots.iter().any(|root| daemon_lock_is_active(root))
        && Instant::now() < cooperative_deadline
    {
        for root in roots {
            if daemon_lock_is_active(root) {
                request_disabled_daemon_shutdown(root);
            }
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    }
    for root in roots {
        if daemon_lock_is_active(root) {
            terminate_identity_verified_residual_daemon(root, expected_executable).with_context(
                || {
                    format!(
                        "stop identity-verified residual ctx daemon for {} before uninstall",
                        root.display()
                    )
                },
            )?;
        }
        wait_for_daemon_lifecycle_release(root)?;
    }
    Ok(())
}

fn request_disabled_daemon_shutdown(data_root: &Path) {
    let _ = daemon_source_refresh_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "shutdown",
        })),
        DAEMON_HEALTH_TIMEOUT,
        DAEMON_HEALTH_RESPONSE_MAX_BYTES,
    );
}

fn wait_for_daemon_lifecycle_release(data_root: &Path) -> Result<()> {
    let deadline = Instant::now() + DAEMON_UPGRADE_RESTART_TIMEOUT;
    while daemon_lock_is_active(data_root) {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "ctx daemon retained lifecycle ownership after verified termination"
            ));
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    }
    Ok(())
}

fn remove_daemon_lifecycle_coordination(data_root: &Path) -> Result<()> {
    remove_daemon_restart_requests(data_root);
    let root = daemon_root_path(data_root);
    for path in [
        daemon_upgrade_handoff_path(data_root),
        daemon_query_endpoint_path(data_root),
        root.join("source-refresh-endpoint.json"),
        root.join("query.sock"),
        root.join("source-refresh.sock"),
        root.join("supervisor.json"),
        daemon_lock_path(data_root),
        pid_lock_guard_path(&daemon_lock_path(data_root)),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("remove daemon coordination {}", path.display()))
            }
        }
    }
    Ok(())
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
            installation_executable: env::current_exe()
                .context("resolve upgrading ctx executable")?,
            restart_trigger: Some(restart_trigger),
            release_on_drop: true,
        });
    }
    write_daemon_restart_request(data_root, restart_trigger, upgrade_attempt_id)?;
    write_daemon_upgrade_handoff(data_root, upgrade_attempt_id, "ready", None)?;
    Ok(DaemonUpgradeHandoff {
        data_root: data_root.to_path_buf(),
        handoff_id: upgrade_attempt_id.to_owned(),
        installation_executable: env::current_exe().context("resolve upgrading ctx executable")?,
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
            let supervisor_resume =
                super::super::daemon_supervisor::resume_daemon_supervisor_after_upgrade(
                    data_root,
                    executable,
                    || write_daemon_upgrade_handoff(data_root, handoff_id, "completed", None),
                )?;
            match supervisor_resume {
                super::super::daemon_supervisor::DaemonSupervisorUpgradeResume::Native => {
                    wait_for_daemon_ready_ack(data_root)?;
                }
                super::super::daemon_supervisor::DaemonSupervisorUpgradeResume::Fallback => {
                    let mut command =
                        if let Some((_trigger, idle_exit, loop_interval)) = captured_restart {
                            daemon_autostart_command(
                                executable,
                                data_root,
                                trigger,
                                (idle_exit != DAEMON_IDLE_EXIT_SECONDS_CAP).then_some(idle_exit),
                                Some(loop_interval),
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
                    let mut child = spawn_daemon_child(&mut command)
                        .context("restart ctx daemon after replacement")?;
                    wait_for_replacement_daemon(data_root, &mut child)?;
                }
            }
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

pub(crate) fn replacement_helper_owns_daemon_handoff(
    data_root: &Path,
    handoff_id: &str,
    helper_pid: u32,
) -> bool {
    read_daemon_upgrade_handoff(data_root).is_some_and(|value| {
        value.get("handoff_id").and_then(Value::as_str) == Some(handoff_id)
            && value.get("phase").and_then(Value::as_str) == Some("scheduled")
            && value
                .get("helper_pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
                == Some(helper_pid)
    })
}

pub(super) fn write_daemon_upgrade_handoff(
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

pub(in crate::semantic) fn write_daemon_restart_request(
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

pub(in crate::semantic) fn read_daemon_restart_request(
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

pub(super) fn remove_daemon_restart_requests(data_root: &Path) {
    let root = daemon_upgrade_restart_request_root(data_root);
    if let Ok(entries) = fs::read_dir(&root) {
        for path in entries.filter_map(|entry| entry.ok().map(|entry| entry.path())) {
            let _ = fs::remove_file(path);
        }
    }
    let _ = fs::remove_dir(root);
}

pub(in crate::semantic) fn acknowledge_daemon_restart_requests(data_root: &Path) {
    remove_daemon_restart_requests(data_root);
}
