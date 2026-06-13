use super::*;

pub(super) async fn recreate_sandbox_machine_if_present_or_forced(
    data_root: &Path,
    machine_name: &str,
    observer: Option<&dyn HarnessSetupObserver>,
    desired_memory_mb: Option<u32>,
    force_recreate: bool,
    last_err: &mut String,
) -> Result<bool> {
    let machine_present = sandbox_machine_present(data_root, machine_name)
        .await
        .unwrap_or(false);
    if !machine_present && !force_recreate {
        return Ok(false);
    }

    observe_log(
        observer,
        HarnessSetupPhase::MachineStartOrInit,
        HarnessSetupLogLevel::Warn,
        if force_recreate {
            "sandbox machine reported an already-running but unreachable state; recreating machine"
        } else {
            "sandbox machine still unreachable after restart; recreating machine"
        },
    );
    cleanup_ctx_managed_sandbox_helper_processes(data_root, machine_name, observer);
    clear_stale_sandbox_machine_temp_state(data_root, machine_name, observer);

    if machine_present {
        let mut rm = sandbox_container_command(data_root)?;
        rm.arg("machine").arg("rm").arg("-f").arg(machine_name);
        let rm_out = command_output_with_timeout(rm, SANDBOX_MACHINE_START_TIMEOUT).await?;
        if !rm_out.status.success() {
            let combined = command_output_message(&rm_out);
            if !combined.is_empty() {
                *last_err = format!("sandbox machine rm -f failed: {combined}");
            }
        }
    }

    let desired_memory = match desired_memory_mb {
        Some(memory_mb) => memory_mb,
        None => configured_sandbox_machine_memory_mb(data_root, observer).await,
    };

    if let Err(err) = initialize_sandbox_machine(
        data_root,
        machine_name,
        Some(desired_memory),
        observer,
        last_err,
    )
    .await
    {
        *last_err = format!("sandbox machine init failed after recreate: {err:#}");
        return Ok(false);
    }

    observe_log(
        observer,
        HarnessSetupPhase::MachineStartOrInit,
        HarnessSetupLogLevel::Info,
        "recreated sandbox machine; waiting for readiness",
    );
    wait_for_sandbox_machine_ready(
        data_root,
        observer,
        "sandbox machine recovered after recreation",
        last_err,
    )
    .await
}
