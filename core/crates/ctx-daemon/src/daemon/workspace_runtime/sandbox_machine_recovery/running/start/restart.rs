use super::super::*;

pub(in crate::daemon::workspace_runtime::sandbox_machine_recovery::running) async fn restart_sandbox_machine_once(
    data_root: &Path,
    machine_name: &str,
    observer: Option<&dyn HarnessSetupObserver>,
    last_err: &mut String,
) -> Result<bool> {
    observe_log(
        observer,
        HarnessSetupPhase::MachineStartOrInit,
        HarnessSetupLogLevel::Warn,
        "sandbox machine remained unreachable after start; restarting once",
    );
    let stop_out = {
        let mut stop = sandbox_container_command(data_root)?;
        stop.arg("machine").arg("stop").arg(machine_name);
        command_output_with_timeout(stop, SANDBOX_MACHINE_START_TIMEOUT).await?
    };
    if !stop_out.status.success() {
        let combined = command_output_message(&stop_out);
        if !combined.is_empty() {
            *last_err = combined.clone();
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Warn,
                &format!("sandbox machine stop returned non-zero during recovery: {combined}"),
            );
        }
    }

    let restart_out = {
        let mut start = sandbox_container_command(data_root)?;
        start.arg("machine").arg("start").arg(machine_name);
        command_output_with_timeout(start, SANDBOX_MACHINE_START_TIMEOUT).await?
    };
    if !restart_out.status.success() {
        let combined = command_output_message(&restart_out);
        if !combined.is_empty() {
            *last_err = combined.clone();
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Warn,
                &format!(
                    "sandbox machine start returned non-zero during restart recovery: {combined}"
                ),
            );
        } else {
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Warn,
                &format!(
                    "sandbox machine start returned non-zero during restart recovery: {}",
                    restart_out.status
                ),
            );
        }
    }

    observe_phase(
        observer,
        HarnessSetupPhase::MachineStartOrInit,
        "waiting for local sandbox runtime readiness",
    );
    wait_for_sandbox_machine_ready(
        data_root,
        observer,
        "local sandbox runtime recovered after restart",
        last_err,
    )
    .await
}
