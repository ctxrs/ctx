use super::*;

mod recreate;
mod start;

use self::recreate::recreate_sandbox_machine_if_present_or_forced;
use self::start::{
    restart_sandbox_machine_once, start_or_initialize_sandbox_machine, StartAttempt,
};

#[cfg_attr(test, allow(dead_code))]
pub(in crate::daemon::workspace_runtime) async fn ensure_sandbox_machine_running_with_observer(
    data_root: &Path,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    let machine_name = sandbox_machine_name(data_root);
    let machine_lock = sandbox_machine_singleflight_lock(&machine_name);
    let machine_guard = match machine_lock.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Info,
                "waiting for concurrent local sandbox runtime init/start operation",
            );
            machine_lock.lock().await
        }
    };
    if !sandbox_machine_required() {
        observe_log(
            observer,
            HarnessSetupPhase::MachineCheck,
            HarnessSetupLogLevel::Info,
            "sandbox machine not required on this platform",
        );
        return Ok(());
    }
    let mut last_err = {
        let mut cmd = sandbox_container_command(data_root)?;
        cmd.arg("info");
        match command_output_with_timeout(cmd, SANDBOX_INFO_TIMEOUT).await {
            Ok(out) if out.status.success() => {
                observe_log(
                    observer,
                    HarnessSetupPhase::MachineCheck,
                    HarnessSetupLogLevel::Info,
                    "local sandbox runtime is already reachable",
                );
                persist_sandbox_machine_cache_to_shared_best_effort(data_root, observer).await;
                return Ok(());
            }
            Ok(out) => String::from_utf8_lossy(&out.stderr).trim().to_string(),
            Err(err) => err.to_string(),
        }
    };

    seed_shared_sandbox_machine_cache_best_effort(data_root, observer).await;

    observe_phase(
        observer,
        HarnessSetupPhase::MachineStartOrInit,
        "starting or initializing local sandbox runtime",
    );
    clear_stale_sandbox_machine_temp_state(data_root, &machine_name, observer);
    let mut desired_memory_mb = None;

    let StartAttempt {
        wait_after_start,
        force_recreate,
    } = start_or_initialize_sandbox_machine(
        data_root,
        &machine_name,
        observer,
        &mut desired_memory_mb,
        &mut last_err,
    )
    .await?;

    if wait_after_start {
        observe_phase(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            "waiting for local sandbox runtime readiness",
        );
        if wait_for_sandbox_machine_ready(
            data_root,
            observer,
            "local sandbox runtime is ready",
            &mut last_err,
        )
        .await?
        {
            return Ok(());
        }
    }

    if !force_recreate
        && restart_sandbox_machine_once(data_root, &machine_name, observer, &mut last_err).await?
    {
        return Ok(());
    }

    if recreate_sandbox_machine_if_present_or_forced(
        data_root,
        &machine_name,
        observer,
        desired_memory_mb,
        force_recreate,
        &mut last_err,
    )
    .await?
    {
        return Ok(());
    }

    drop(machine_guard);
    if last_err.trim().is_empty() {
        anyhow::bail!("sandbox machine remained unreachable after bounded recovery");
    }
    anyhow::bail!(
        "sandbox machine remained unreachable after bounded recovery: {}",
        last_err.trim()
    );
}
