use super::*;

fn daemon_upgrade_handoff_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_UPGRADE_HANDOFF_FILE)
}

fn daemon_upgrade_restart_request_root(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_UPGRADE_RESTART_REQUEST_DIR)
}

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
    pub(super) restart_trigger: Option<DaemonTriggerCommandArg>,
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
                            && super::super::semantic_query_service_supported(),
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
