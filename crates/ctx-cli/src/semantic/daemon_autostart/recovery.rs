use super::*;

pub(super) fn restart_acknowledged_installation_daemons(
    executable: &Path,
    attempt_id: &str,
    skip_root: Option<&Path>,
) -> Result<()> {
    restart_acknowledged_installation_daemons_with(
        executable,
        attempt_id,
        skip_root,
        spawn_daemon_child,
    )
}

pub(super) fn restart_acknowledged_installation_daemons_with(
    executable: &Path,
    attempt_id: &str,
    skip_root: Option<&Path>,
    mut spawn: impl FnMut(&mut Command) -> io::Result<Child>,
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
        let mut command = daemon_autostart_command(
            executable,
            &restart.data_root,
            restart.trigger,
            restart.idle_exit_seconds,
            restart.loop_interval_seconds,
            None,
        );
        let mut child = spawn(&mut command).with_context(|| {
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
