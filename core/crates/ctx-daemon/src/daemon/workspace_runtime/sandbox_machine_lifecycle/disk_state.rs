use super::*;

use self::engine::ensure_engine_ready_for_disk_state_inspection;
pub(super) use self::engine::has_running_workspace_containers_for_stopped_machine_reconfiguration;

mod engine;

pub(super) async fn has_running_workspace_containers(
    manager: &HarnessRuntimeManager,
) -> Result<bool> {
    let mut cmd = sandbox_container_command(manager.data_root())?;
    cmd.arg("ps").arg("--format").arg("{{.Names}}");
    let output = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
    if !output.status.success() {
        anyhow::bail!("sandbox CLI ps failed: {}", command_output_message(&output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .map(str::trim)
        .any(|name| name.starts_with("ctx-harness-")))
}

pub(super) async fn should_defer_disk_isolated_machine_reconfiguration(
    manager: &HarnessRuntimeManager,
    settings: &ContainerExecutionSettings,
    machine_name: &str,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<bool> {
    if !matches!(settings.mount_mode, ContainerMountMode::DiskIsolated) {
        return Ok(false);
    }
    disk_isolated_workspace_volumes_exist(manager, machine_name, observer).await
}

pub(super) async fn disk_isolated_workspace_volumes_exist(
    manager: &HarnessRuntimeManager,
    machine_name: &str,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<bool> {
    if !ensure_engine_ready_for_disk_state_inspection(manager, machine_name, observer).await? {
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Warn,
            "unable to verify disk-isolated workspace volumes before memory reconfiguration; leaving local sandbox runtime unchanged",
        );
        return Ok(true);
    }

    let mut cmd = sandbox_container_command(manager.data_root())?;
    cmd.arg("volume").arg("ls").arg("--format").arg("{{.Name}}");
    let output = match command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await {
        Ok(output) => output,
        Err(err) => {
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Warn,
                &format!(
                    "failed to inspect disk-isolated workspace volumes before memory reconfiguration: {err}"
                ),
            );
            return Ok(true);
        }
    };
    if !output.status.success() {
        let detail = command_output_message(&output);
        let suffix = if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        };
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Warn,
            &format!(
                "unable to inspect disk-isolated workspace volumes before memory reconfiguration{suffix}"
            ),
        );
        return Ok(true);
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .any(|name| name.starts_with("ctx-ws-")))
}
