use super::*;

pub(super) fn daemon_should_schedule_auto_upgrade(
    daemon_enabled: bool,
    daemon_mode: DaemonMode,
) -> bool {
    daemon_enabled && daemon_mode == DaemonMode::Full
}

#[cfg(test)]
pub(super) fn fail_daemon_before_ready_for_test(data_root: &Path) -> Result<()> {
    if data_root
        .join(".fail-daemon-before-ready-for-test")
        .exists()
    {
        return Err(anyhow!("injected daemon failure before readiness"));
    }
    Ok(())
}

pub(super) fn daemon_services_can_begin_idle_shutdown(
    query_service: Option<&DaemonQueryService>,
    observed_query_generation: u64,
    refresh_service: Option<&DaemonQueryService>,
    observed_refresh_generation: u64,
) -> bool {
    let refresh_activity = refresh_service.map(|service| service.activity.as_ref());
    if !daemon_can_begin_idle_shutdown(refresh_activity, observed_refresh_generation) {
        return false;
    }
    if daemon_can_begin_idle_shutdown(
        query_service.map(|service| service.activity.as_ref()),
        observed_query_generation,
    ) {
        return true;
    }
    if let Some(activity) = refresh_activity {
        activity.resume_accepting();
    }
    false
}

pub(super) fn ensure_daemon_ipc_services_healthy(
    query_service: Option<&DaemonQueryService>,
    refresh_service: Option<&DaemonQueryService>,
) -> Result<()> {
    for service in [refresh_service, query_service].into_iter().flatten() {
        if service.listener_finished() {
            return Err(anyhow!(
                "daemon {} IPC listener exited unexpectedly",
                service.service.as_str()
            ));
        }
    }
    Ok(())
}

pub(super) fn daemon_should_attempt_finite_idle_shutdown(
    idle_exit: Option<StdDuration>,
    idle_since: Option<Instant>,
    _retry_due: bool,
    _source_refresh_pending: bool,
) -> bool {
    idle_exit.is_some_and(|limit| idle_since.is_some_and(|idle| idle.elapsed() >= limit))
}

pub(super) fn installation_lifecycle_blocks_current_process(data_root: &Path) -> bool {
    crate::upgrade::installation_hosted_uninstall_is_active().unwrap_or(true)
        || (!super::current_process_owns_daemon_upgrade_handoff(data_root)
            && crate::upgrade::installation_upgrade_is_active().unwrap_or(false))
}
