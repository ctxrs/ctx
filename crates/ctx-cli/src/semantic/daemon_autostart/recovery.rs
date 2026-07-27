use super::*;

pub(super) fn restart_acknowledged_installation_daemons(
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

pub(super) fn restart_acknowledged_legacy_installation_daemons(
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
            config.semantic_search_enabled() && super::super::semantic_query_service_supported(),
        )?;
        let _ = fs::remove_file(restart.registration_path);
    }
    Ok(())
}

pub(in crate::semantic) fn resume_completed_installation_daemons(data_root: &Path) -> Result<()> {
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

pub(super) fn wait_for_replacement_daemon(data_root: &Path, child: &mut Child) -> Result<()> {
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

pub(super) fn wait_for_legacy_replacement_daemon(
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

pub(super) fn clear_legacy_daemon_readiness(data_root: &Path) -> Result<()> {
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

pub(super) fn legacy_daemon_status_is_ready(
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

pub(super) fn legacy_daemon_query_endpoint_is_ready(data_root: &Path, child_pid: u32) -> bool {
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

pub(super) fn legacy_daemon_query_service_is_ready(data_root: &Path, child_pid: u32) -> bool {
    legacy_daemon_query_endpoint_is_ready(data_root, child_pid)
        && super::super::query_service::daemon_query_service_available(data_root)
}

pub(super) fn wait_for_daemon_ready_ack(data_root: &Path) -> Result<()> {
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
