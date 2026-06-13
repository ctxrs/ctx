use super::*;

pub(super) async fn ensure_sandbox_machine_download(manager: &HarnessRuntimeManager) -> Result<()> {
    if !sandbox_machine_required() {
        return Ok(());
    }
    ensure_managed_sandbox_cli_runtime(manager.data_root(), None, None).await?;
    let machine_image = if cfg!(target_os = "macos") {
        Some(ensure_managed_sandbox_machine_cache(manager.data_root(), None, None).await?)
    } else {
        None
    };
    let machine_name = sandbox_machine_name(manager.data_root());
    let machine_lock = sandbox_machine_singleflight_lock(&machine_name);
    let _machine_guard = match machine_lock.try_lock() {
        Ok(guard) => guard,
        Err(_) => return Ok(()),
    };
    seed_shared_sandbox_machine_cache_best_effort(manager.data_root(), None).await;
    if sandbox_machine_present(manager.data_root(), &machine_name).await? {
        persist_sandbox_machine_cache_to_shared_best_effort(manager.data_root(), None).await;
        return Ok(());
    }
    let init_outcome = run_sandbox_machine_init(
        manager.data_root(),
        &machine_name,
        machine_image.as_deref(),
        Some(container_machine_memory_mb(
            &ContainerExecutionSettings::default(),
        )),
        None,
    )
    .await?;
    let output = init_outcome.output;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let combined = format!("{stderr}\n{stdout}").trim().to_string();
    if init_outcome.continued_after_machine_present
        || output.status.success()
        || combined.to_ascii_lowercase().contains("already exists")
    {
        persist_sandbox_machine_cache_to_shared_best_effort(manager.data_root(), None).await;
        return Ok(());
    }
    anyhow::bail!("sandbox machine init failed: {combined}");
}

pub(super) async fn init_sandbox_machine_locked(
    manager: &HarnessRuntimeManager,
    machine_name: &str,
    desired_memory_mb: u32,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    let machine_image = if cfg!(target_os = "macos") {
        Some(ensure_managed_sandbox_machine_cache(manager.data_root(), observer, None).await?)
    } else {
        None
    };
    let init_outcome = run_sandbox_machine_init(
        manager.data_root(),
        machine_name,
        machine_image.as_deref(),
        Some(desired_memory_mb),
        observer,
    )
    .await?;
    let output = init_outcome.output;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let combined = format!("{stderr}\n{stdout}").trim().to_string();
    if init_outcome.continued_after_machine_present
        || output.status.success()
        || combined.to_ascii_lowercase().contains("already exists")
    {
        persist_sandbox_machine_cache_to_shared_best_effort(manager.data_root(), observer).await;
        return Ok(());
    }
    anyhow::bail!("sandbox machine init failed: {combined}");
}

pub(super) async fn stop_sandbox_machine_locked(
    manager: &HarnessRuntimeManager,
    machine_name: &str,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<bool> {
    let mut cmd = sandbox_container_command(manager.data_root())?;
    cmd.arg("machine").arg("stop").arg(machine_name);
    let output = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
    if output.status.success() {
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Info,
            "stopped local sandbox runtime",
        );
        return Ok(true);
    }
    let combined = command_output_message(&output);
    let combined_lc = combined.to_ascii_lowercase();
    if combined_lc.contains("already stopped")
        || combined_lc.contains("not running")
        || combined_lc.contains("no machine")
        || combined_lc.contains("does not exist")
    {
        return Ok(false);
    }
    anyhow::bail!("sandbox machine stop failed: {combined}");
}

pub(super) async fn remove_sandbox_machine_locked(
    manager: &HarnessRuntimeManager,
    machine_name: &str,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    let _ = stop_sandbox_machine_locked(manager, machine_name, observer).await;
    let mut cmd = sandbox_container_command(manager.data_root())?;
    cmd.arg("machine").arg("rm").arg("-f").arg(machine_name);
    let output = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
    if output.status.success() {
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Info,
            "removed local sandbox runtime for reconfiguration",
        );
        return Ok(());
    }
    let combined = command_output_message(&output);
    let combined_lc = combined.to_ascii_lowercase();
    if combined_lc.contains("does not exist") || combined_lc.contains("no machine") {
        return Ok(());
    }
    anyhow::bail!("sandbox machine rm -f failed: {combined}");
}
