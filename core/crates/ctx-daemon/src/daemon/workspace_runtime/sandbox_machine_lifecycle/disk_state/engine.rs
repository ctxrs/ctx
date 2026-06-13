use std::time::Instant;

use super::super::*;

pub(super) async fn ensure_engine_ready_for_disk_state_inspection(
    manager: &HarnessRuntimeManager,
    machine_name: &str,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<bool> {
    if sandbox_engine_ready(manager.data_root()).await? {
        return Ok(true);
    }

    observe_log(
        observer,
        HarnessSetupPhase::MachineStartOrInit,
        HarnessSetupLogLevel::Info,
        "starting local sandbox runtime to inspect disk-isolated workspace volumes before memory reconfiguration",
    );
    let mut start = sandbox_container_command(manager.data_root())?;
    start.arg("machine").arg("start").arg(machine_name);
    let output = command_output_with_timeout(start, SANDBOX_MACHINE_START_TIMEOUT).await?;
    if !output.status.success() {
        return Ok(false);
    }

    let deadline = Instant::now() + sandbox_machine_ready_timeout();
    loop {
        if sandbox_engine_ready(manager.data_root()).await? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(sandbox_machine_ready_poll_interval()).await;
    }
}

pub(in crate::daemon::workspace_runtime::sandbox_machine_lifecycle) async fn has_running_workspace_containers_for_stopped_machine_reconfiguration(
    manager: &HarnessRuntimeManager,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<bool> {
    match super::has_running_workspace_containers(manager).await {
        Ok(has_running) => Ok(has_running),
        Err(err) => {
            if sandbox_engine_ready(manager.data_root())
                .await
                .unwrap_or(false)
            {
                return Err(err);
            }
            let machine_name = sandbox_machine_name(manager.data_root());
            match manager.inspect_sandbox_machine_state(&machine_name).await? {
                Some(state) if state.contains("running") || state.contains("starting") => {
                    observe_log(
                        observer,
                        HarnessSetupPhase::MachineStartOrInit,
                        HarnessSetupLogLevel::Warn,
                        "local sandbox runtime appears to be running but unreachable; deferring memory reconfiguration until workload probes recover",
                    );
                    tracing::debug!(
                        "treating workspace container probe failure as busy because the local sandbox runtime still reports a running state: {err:#}"
                    );
                    return Ok(true);
                }
                Some(_) => {}
                None => {
                    observe_log(
                        observer,
                        HarnessSetupPhase::MachineStartOrInit,
                        HarnessSetupLogLevel::Warn,
                        "local sandbox runtime state is unknown while workload probes are unreachable; deferring memory reconfiguration until the runtime can be inspected safely",
                    );
                    tracing::debug!(
                        "treating workspace container probe failure as busy because the local sandbox runtime state is unknown: {err:#}"
                    );
                    return Ok(true);
                }
            }
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Info,
                "local sandbox runtime is not reachable; continuing memory reconfiguration without workload probe",
            );
            tracing::debug!(
                "treating workspace container probe failure as idle because the sandbox runtime is not reachable: {err:#}"
            );
            Ok(false)
        }
    }
}
