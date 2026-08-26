//! CLI-only setup sequencing over neutral daemon lifecycle operations.

use super::*;

fn application_config(config: &AppConfig<'_>) -> ctx_daemon_application::DaemonConfigSnapshot {
    ctx_daemon_application::DaemonConfigSnapshot {
        enabled: config.daemon.enabled,
        mode: super::super::daemon_supervisor::daemon_mode(config.daemon.mode),
        semantic_enabled: config.semantic_search_enabled(),
    }
}

pub(super) fn finite_application_config(
    config: &AppConfig<'_>,
) -> ctx_daemon_application::DaemonConfigSnapshot {
    let mut effective = application_config(config);
    effective.enabled = false;
    effective
}

fn application_trigger(trigger: DaemonTriggerCommandArg) -> ctx_daemon_application::DaemonTrigger {
    super::super::daemon_supervisor::daemon_trigger(trigger)
}

#[cfg(test)]
pub(super) fn daemon_autostart_allowed(data_root: &Path, config: &AppConfig<'_>) -> bool {
    ctx_daemon_application::daemon_autostart_allowed(data_root, &application_config(config))
}

pub fn daemon_autostart_suppression_reason() -> Option<&'static str> {
    ctx_daemon_application::daemon_autostart_suppression_reason()
}

pub fn maybe_autostart_daemon(
    data_root: &Path,
    config: &AppConfig<'_>,
    trigger: DaemonTriggerCommandArg,
) {
    if !config.daemon.enabled {
        return;
    }
    super::super::daemon_supervisor::with_daemon_application(|application| {
        if application.daemon_start_is_fenced() {
            return;
        }
        if application
            .active_daemon_matches_current_executable(data_root)
            .unwrap_or(false)
        {
            return;
        }
        if daemon_autostart_suppression_reason().is_none() {
            match super::super::daemon_supervisor::ensure_daemon_supervisor(application, data_root)
            {
                Ok(_) => {}
                Err(_) => return,
            }
        }
        let _ = application.request_daemon_start(
            data_root,
            &application_config(config),
            application_trigger(trigger),
        );
    });
}

pub fn start_finite_core_worker_and_wait(
    data_root: &Path,
    config: &AppConfig<'_>,
    trigger: DaemonTriggerCommandArg,
) -> Result<DaemonHandoff> {
    super::super::daemon_supervisor::with_daemon_application(|application| {
        let effective = finite_application_config(config);
        let handoff = application
            .start_finite_core_worker_and_wait(data_root, &effective, application_trigger(trigger))
            .map_err(|error| match error {
                ctx_daemon_application::DaemonStartError::Suppressed(reason) => anyhow!(
                    "ctx finite Core worker start was suppressed ({reason}); retry after it clears"
                ),
                ctx_daemon_application::DaemonStartError::BinaryIdentity(error) => error,
                ctx_daemon_application::DaemonStartError::Start(error) => {
                    anyhow!("ctx finite Core worker did not start: {error:#}")
                }
                ctx_daemon_application::DaemonStartError::Ready(error) => {
                    anyhow!("ctx finite Core worker did not become ready: {error}")
                }
            })?;
        Ok(DaemonHandoff {
            pid: handoff.pid,
            heartbeat_at_ms: handoff.heartbeat_at_ms,
        })
    })
}

pub fn autostart_daemon_and_wait(
    data_root: &Path,
    config: &AppConfig<'_>,
    trigger: DaemonTriggerCommandArg,
) -> Result<DaemonHandoff> {
    Ok(autostart_daemon_for_setup_and_wait(data_root, config, trigger)?.handoff)
}

pub fn autostart_daemon_for_setup_and_wait(
    data_root: &Path,
    config: &AppConfig<'_>,
    trigger: DaemonTriggerCommandArg,
) -> Result<DaemonSetupHandoff> {
    super::super::daemon_supervisor::with_daemon_application(|application| {
        if application.daemon_start_is_fenced() {
            return Err(anyhow!(
                "ctx daemon start was suppressed (hosted_uninstall_active); retry after it clears or run `ctx setup --no-daemon`"
            ));
        }
        if daemon_autostart_suppression_reason().is_none() {
            super::super::daemon_supervisor::ensure_daemon_supervisor(application, data_root)
                .context("establish ctx daemon supervision")?;
        }
        let handoff = application
            .start_daemon_and_wait(
                data_root,
                &application_config(config),
                application_trigger(trigger),
            )
            .map_err(|error| match error {
                ctx_daemon_application::DaemonStartError::Suppressed(reason) => anyhow!(
                    "ctx daemon start was suppressed ({reason}); retry after it clears or run `ctx setup --no-daemon`"
                ),
                ctx_daemon_application::DaemonStartError::BinaryIdentity(error) => error,
                ctx_daemon_application::DaemonStartError::Start(error) => anyhow!(
                    "ctx daemon did not start: {error:#}. Run `ctx status --format json`, then `ctx daemon run` for details"
                ),
                ctx_daemon_application::DaemonStartError::Ready(error) => anyhow!(
                    "ctx daemon did not become ready: {error}. Run `ctx status --format json`, then `ctx daemon run` for details"
                ),
            })?;
        Ok(DaemonSetupHandoff {
            handoff: DaemonHandoff {
                pid: handoff.pid,
                heartbeat_at_ms: handoff.heartbeat_at_ms,
            },
        })
    })
}

pub fn observe_daemon_for_setup_and_wait(
    data_root: &Path,
    config: &AppConfig<'_>,
) -> Result<DaemonSetupHandoff> {
    super::super::daemon_supervisor::with_daemon_application(|application| {
        let handoff = application
            .observe_daemon_and_wait(data_root, &application_config(config))
            .context("observe ready ctx daemon at setup output boundary")?;
        Ok(DaemonSetupHandoff {
            handoff: DaemonHandoff {
                pid: handoff.pid,
                heartbeat_at_ms: handoff.heartbeat_at_ms,
            },
        })
    })
}

#[cfg(test)]
pub(super) fn handoff_mismatched_daemon_owner(data_root: &Path, executable: &Path) -> Result<()> {
    super::super::daemon_supervisor::with_daemon_application(|application| {
        application.handoff_mismatched_daemon_owner(data_root, executable)
    })
}

pub(super) fn daemon_autostart_command(
    exe: &Path,
    root: &Path,
    trigger: DaemonTriggerCommandArg,
    interval: Option<u64>,
    token: Option<&str>,
) -> io::Result<NormalizedLaunch> {
    ctx_daemon_application::daemon_autostart_command(
        exe,
        root,
        application_trigger(trigger),
        interval,
        token,
    )
}
#[cfg(test)]
pub(super) fn configured_daemon_autostart_command(
    exe: &Path,
    root: &Path,
    trigger: DaemonTriggerCommandArg,
    token: Option<&str>,
) -> io::Result<NormalizedLaunch> {
    ctx_daemon_application::configured_daemon_autostart_command(
        exe,
        root,
        application_trigger(trigger),
        token,
    )
}
pub(super) fn spawn_daemon_child(launch: NormalizedLaunch) -> io::Result<Child> {
    super::super::daemon_supervisor::with_daemon_application(|application| {
        application.spawn_daemon_child(launch)
    })
}
pub(super) fn spawn_daemon_child_for_upgrade_handoff(
    launch: NormalizedLaunch,
    executable: &Path,
) -> io::Result<Child> {
    super::super::daemon_supervisor::with_daemon_application(|application| {
        application.spawn_daemon_child_for_upgrade_handoff(launch, executable)
    })
}
pub(super) fn daemon_restart_allowed(root: &Path) -> Result<bool> {
    super::super::daemon_supervisor::with_daemon_application(|application| {
        application.daemon_restart_allowed(root)
    })
}
pub(super) fn daemon_restart_trigger(root: &Path) -> Option<DaemonTriggerCommandArg> {
    ctx_daemon_application::daemon_restart_trigger(root)
        .map(super::super::daemon_supervisor::daemon_trigger_arg)
}
pub(super) fn parse_daemon_trigger(value: Option<&str>) -> Option<DaemonTriggerCommandArg> {
    ctx_daemon_application::parse_persisted_trigger(value)
        .map(super::super::daemon_supervisor::daemon_trigger_arg)
}
