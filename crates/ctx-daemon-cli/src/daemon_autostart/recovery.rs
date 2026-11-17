use super::*;

pub(super) fn restart_acknowledged_installation_daemons(
    executable: &Path,
    attempt_id: &str,
    skip_root: Option<&Path>,
) -> Result<()> {
    let (_, registrations) =
        ctx_upgrade_engine::installation_daemon_coordination_paths_for(executable)?;
    restart_acknowledged_installation_daemons_with(
        &registrations,
        executable,
        attempt_id,
        skip_root,
        spawn_daemon_child,
    )
}

pub(super) fn restart_acknowledged_installation_daemons_with(
    registrations: &Path,
    executable: &Path,
    attempt_id: &str,
    skip_root: Option<&Path>,
    mut spawn: impl FnMut(NormalizedLaunch) -> io::Result<Child>,
) -> Result<()> {
    for restart in read_installation_daemon_restarts_from(registrations, attempt_id, false)? {
        if skip_root.is_some_and(|root| root == restart.data_root) {
            let _ = fs::remove_file(restart.registration_path);
            continue;
        }
        if !daemon_restart_allowed(&restart.data_root)? {
            let _transition =
                ctx_daemon_runtime::DaemonLifecycleTransitionLock::acquire(&restart.data_root)?;
            if !daemon_restart_allowed(&restart.data_root)? {
                remove_daemon_restart_requests(&restart.data_root);
                let _ = fs::remove_file(restart.registration_path);
                continue;
            }
        }
        if daemon_lock_is_active(&restart.data_root) {
            wait_for_daemon_ready_ack(&restart.data_root)?;
            let _ = fs::remove_file(restart.registration_path);
            continue;
        }
        let launch = daemon_autostart_command(
            executable,
            &restart.data_root,
            restart.trigger,
            restart.loop_interval_seconds,
            None,
        )?;
        let mut child = spawn(launch).with_context(|| {
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

pub(crate) fn resume_completed_installation_daemons(data_root: &Path) -> Result<()> {
    if current_process_owns_daemon_upgrade_handoff(data_root) {
        return Ok(());
    }
    if ctx_upgrade_engine::installation_upgrade_is_active()? {
        return Ok(());
    }
    let Some(attempt_id) = ctx_upgrade_engine::terminal_installation_upgrade_attempt_id()? else {
        return Ok(());
    };
    let executable = ctx_upgrade_engine::installation_executable_path()?;
    restart_acknowledged_installation_daemons(&executable, &attempt_id, Some(data_root))
}

pub(super) fn wait_for_replacement_daemon(data_root: &Path, child: &mut Child) -> Result<()> {
    ctx_daemon_runtime::wait_for_replacement_child(
        child,
        DAEMON_UPGRADE_RESTART_TIMEOUT,
        DAEMON_UPGRADE_POLL_INTERVAL,
        |pid| {
            daemon_lock_is_owned_by(data_root, pid)
                && read_daemon_restart_request(data_root).is_none()
        },
    )
    .map_err(replacement_child_wait_error)
}

fn replacement_child_wait_error(
    error: ctx_daemon_runtime::ReplacementChildWaitError,
) -> anyhow::Error {
    match error {
        ctx_daemon_runtime::ReplacementChildWaitError::ChildStatus(error) => error.into(),
        ctx_daemon_runtime::ReplacementChildWaitError::ExitedBeforeOwnership => {
            anyhow!("replacement ctx daemon exited before acquiring lifecycle ownership")
        }
        ctx_daemon_runtime::ReplacementChildWaitError::TimedOut => {
            anyhow!("timed out waiting for the replacement ctx daemon to start")
        }
    }
}

pub(super) fn wait_for_daemon_ready_ack(data_root: &Path) -> Result<()> {
    ctx_daemon_runtime::wait_for_daemon_ready(
        DAEMON_UPGRADE_RESTART_TIMEOUT,
        DAEMON_UPGRADE_POLL_INTERVAL,
        || daemon_lock_is_active(data_root),
        || read_daemon_restart_request(data_root).is_some(),
    )
    .map_err(daemon_readiness_wait_error)
}

fn daemon_readiness_wait_error(
    error: ctx_daemon_runtime::DaemonReadinessWaitError,
) -> anyhow::Error {
    match error {
        ctx_daemon_runtime::DaemonReadinessWaitError::ExitedBeforeReadiness => {
            anyhow!("replacement ctx daemon exited before lifecycle readiness")
        }
        ctx_daemon_runtime::DaemonReadinessWaitError::TimedOut => {
            anyhow!("timed out waiting for replacement ctx daemon readiness")
        }
    }
}
