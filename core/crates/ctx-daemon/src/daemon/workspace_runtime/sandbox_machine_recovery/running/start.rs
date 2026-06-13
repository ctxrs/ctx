use super::*;

mod restart;

pub(super) use restart::restart_sandbox_machine_once;

pub(super) struct StartAttempt {
    pub(super) wait_after_start: bool,
    pub(super) force_recreate: bool,
}

pub(super) async fn start_or_initialize_sandbox_machine(
    data_root: &Path,
    machine_name: &str,
    observer: Option<&dyn HarnessSetupObserver>,
    desired_memory_mb: &mut Option<u32>,
    last_err: &mut String,
) -> Result<StartAttempt> {
    let start_out = {
        let mut start = sandbox_container_command(data_root)?;
        start.arg("machine").arg("start").arg(machine_name);
        command_output_with_timeout(start, SANDBOX_MACHINE_START_TIMEOUT).await?
    };
    if start_out.status.success() {
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Info,
            "local sandbox runtime start command completed; waiting for readiness",
        );
        return Ok(StartAttempt {
            wait_after_start: true,
            force_recreate: false,
        });
    }

    let combined = command_output_message(&start_out);
    let combined_lc = combined.to_ascii_lowercase();
    if looks_like_missing_machine_error(&combined_lc) {
        let desired_memory = match desired_memory_mb {
            Some(memory_mb) => *memory_mb,
            None => {
                let memory_mb = configured_sandbox_machine_memory_mb(data_root, observer).await;
                *desired_memory_mb = Some(memory_mb);
                memory_mb
            }
        };
        observe_phase(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            "materializing local sandbox runtime from managed cache",
        );
        initialize_sandbox_machine(
            data_root,
            machine_name,
            Some(desired_memory),
            observer,
            last_err,
        )
        .await?;
        observe_phase(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            "waiting for local sandbox runtime readiness",
        );
        return Ok(StartAttempt {
            wait_after_start: true,
            force_recreate: false,
        });
    }

    if looks_like_recoverable_machine_start_error(&combined_lc) {
        if !combined.is_empty() {
            *last_err = combined.clone();
        }
        return Ok(recoverable_start_attempt(observer, &combined, &combined_lc));
    }

    if combined.is_empty() {
        anyhow::bail!(
            "sandbox machine start failed with non-zero exit {}",
            start_out.status
        );
    }
    anyhow::bail!("sandbox machine start failed: {combined}");
}

fn recoverable_start_attempt(
    observer: Option<&dyn HarnessSetupObserver>,
    combined: &str,
    combined_lc: &str,
) -> StartAttempt {
    if looks_like_running_but_unreachable_machine_start_error(combined_lc) {
        let message = if combined.is_empty() {
            "sandbox machine start reported an already-running machine while sandbox CLI remained unreachable; restarting once"
                .to_string()
        } else {
            format!(
                "sandbox machine start reported an already-running machine while sandbox CLI remained unreachable; restarting once: {combined}"
            )
        };
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Warn,
            &message,
        );
        return StartAttempt {
            wait_after_start: false,
            force_recreate: true,
        };
    }

    let message = if combined.is_empty() {
        "sandbox machine start returned a recoverable error; waiting for readiness".to_string()
    } else {
        format!(
            "sandbox machine start returned recoverable error; waiting for readiness: {combined}"
        )
    };
    observe_log(
        observer,
        HarnessSetupPhase::MachineStartOrInit,
        HarnessSetupLogLevel::Warn,
        &message,
    );
    StartAttempt {
        wait_after_start: true,
        force_recreate: false,
    }
}
