use super::*;

pub(super) fn request_daemon_autostart(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    config: &DaemonConfigSnapshot,
    trigger: DaemonTrigger,
    profile: DaemonLaunchProfile,
) -> Result<DaemonAutostartRequest> {
    request_daemon_autostart_with(host, data_root, config, trigger, profile, &mut || Ok(()))
}

pub(super) fn request_daemon_autostart_with(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    config: &DaemonConfigSnapshot,
    trigger: DaemonTrigger,
    profile: DaemonLaunchProfile,
    checkpoint: &mut dyn FnMut() -> Result<()>,
) -> Result<DaemonAutostartRequest> {
    checkpoint()?;
    if hosted_uninstall_fences_daemon_autostart(host) {
        return Ok(DaemonAutostartRequest::Suppressed(
            "hosted_uninstall_active",
        ));
    }
    if profile == DaemonLaunchProfile::Persistent && config.enabled {
        if let Some(deferral) = host.defer_restart_for_upgrade_handoff(data_root, trigger)? {
            return Ok(DaemonAutostartRequest::Deferred(deferral));
        }
    }
    // Suppression disables spawning, not reuse. Test harnesses and managed
    // callers can intentionally provide an already-owned daemon while
    // forbidding any additional detached process.
    if (config.enabled || profile == DaemonLaunchProfile::FiniteCoreWorker)
        && daemon_lock_is_active(data_root)
    {
        let executable = daemon_autostart_exe()?;
        if daemon_lock_matches_executable(data_root, &executable)? {
            return Ok(DaemonAutostartRequest::Existing(
                wait_for_daemon_owner_identity_with_cancellation(data_root, checkpoint)?
                    .ok_or_else(|| {
                        anyhow!("active ctx daemon lock has no stable owner identity")
                    })?,
            ));
        }
        if profile == DaemonLaunchProfile::FiniteCoreWorker {
            // A finite foreground joiner has observational authority only over
            // any existing owner, even when it was started by another binary.
            // Binary handoff and replacement remain persistent lifecycle work.
            return Ok(DaemonAutostartRequest::Existing(
                wait_for_daemon_owner_identity_with_cancellation(data_root, checkpoint)?
                    .ok_or_else(|| {
                        anyhow!("active ctx daemon lock has no stable owner identity")
                    })?,
            ));
        }
        if daemon_autostart_suppression_reason().is_some() {
            return Err(binary_identity_handoff_error());
        }
        handoff_mismatched_daemon_owner_with_cancellation(
            host,
            data_root,
            &executable,
            checkpoint,
        )?;
        if daemon_lock_is_active(data_root) {
            return Ok(DaemonAutostartRequest::Existing(
                wait_for_daemon_owner_identity_with_cancellation(data_root, checkpoint)?
                    .ok_or_else(|| {
                        anyhow!("active replacement ctx daemon has no stable owner identity")
                    })?,
            ));
        }
    }
    if let Some(reason) = daemon_autostart_suppression_reason() {
        return Ok(DaemonAutostartRequest::Suppressed(reason));
    }
    if profile == DaemonLaunchProfile::Persistent && !daemon_autostart_allowed(data_root, config) {
        return Ok(DaemonAutostartRequest::Suppressed("not_allowed"));
    }
    let automatic_recovery_allowed = profile == DaemonLaunchProfile::Persistent
        && config.enabled
        && config.mode == DaemonMode::Full
        && host
            .automatic_upgrade_recovery_allowed(data_root)
            .unwrap_or(false);
    if host.installation_upgrade_active().unwrap_or(false) && !automatic_recovery_allowed {
        return Ok(DaemonAutostartRequest::Suppressed(
            "installation_upgrade_active",
        ));
    }
    let lock_path = daemon_lock_path(data_root);
    if lock_path.exists()
        && !daemon_lock_is_stale(&lock_path)
        && profile == DaemonLaunchProfile::Persistent
    {
        let executable = daemon_autostart_exe()?;
        handoff_mismatched_daemon_owner_with_cancellation(
            host,
            data_root,
            &executable,
            checkpoint,
        )?;
        if daemon_lock_is_active(data_root) {
            return Ok(DaemonAutostartRequest::Existing(
                wait_for_daemon_owner_identity_with_cancellation(data_root, checkpoint)?
                    .ok_or_else(|| {
                        anyhow!("active replacement ctx daemon has no stable owner identity")
                    })?,
            ));
        }
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
    let launch = match profile {
        DaemonLaunchProfile::Persistent => {
            configured_daemon_autostart_command(&exe, data_root, trigger, None)
        }
        DaemonLaunchProfile::FiniteCoreWorker => {
            configured_finite_core_worker_command(&exe, data_root, trigger)
        }
    };
    let launch = launch?;
    match spawn_daemon_profile(host, launch, profile, checkpoint) {
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

pub(super) fn spawn_daemon_profile(
    host: &dyn DaemonApplicationHost,
    launch: NormalizedLaunch,
    profile: DaemonLaunchProfile,
    checkpoint: &mut dyn FnMut() -> Result<()>,
) -> Result<Child> {
    // Last cancellation boundary before process creation. If cancellation was
    // observed during lifecycle/config preparation, no child is spawned.
    checkpoint()?;
    match profile {
        DaemonLaunchProfile::Persistent => spawn_daemon_child(host, launch),
        DaemonLaunchProfile::FiniteCoreWorker => {
            launch::spawn_attached_finite_core_worker(host, launch)
        }
    }
    .map_err(Into::into)
}
