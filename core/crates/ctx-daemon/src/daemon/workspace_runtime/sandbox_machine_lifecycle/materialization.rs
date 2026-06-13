use super::*;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(super) async fn ensure_sandbox_machine_materialized(
    manager: &HarnessRuntimeManager,
    settings: &ContainerExecutionSettings,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    if !sandbox_machine_required() {
        return Ok(());
    }
    let desired_memory_mb = container_machine_memory_mb(settings);
    let machine_name = sandbox_machine_name(manager.data_root());
    let machine_lock = sandbox_machine_singleflight_lock(&machine_name);
    let _machine_guard = machine_lock.lock().await;
    seed_shared_sandbox_machine_cache_best_effort(manager.data_root(), observer).await;

    let present = sandbox_machine_present(manager.data_root(), &machine_name).await?;
    if present {
        let actual_memory_mb = manager
            .inspect_sandbox_machine_memory_mb(&machine_name)
            .await?;
        if actual_memory_mb == Some(desired_memory_mb) {
            return Ok(());
        }
        if manager
            .should_defer_disk_isolated_machine_reconfiguration(settings, &machine_name, observer)
            .await?
        {
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Warn,
                "deferring local sandbox runtime memory reconfiguration because disk-isolated workspace volumes would be destroyed by machine recreation",
            );
            return Ok(());
        }
        if manager
            .has_running_workspace_containers_for_stopped_machine_reconfiguration(observer)
            .await?
        {
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Warn,
                "deferring local sandbox runtime memory reconfiguration until active workspace containers stop",
            );
            return Ok(());
        }
        let detail = actual_memory_mb
            .map(|value| format!("{value} MiB"))
            .unwrap_or_else(|| "unknown".to_string());
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Info,
            &format!(
                "reconfiguring local sandbox runtime memory from {detail} to {desired_memory_mb} MiB"
            ),
        );
        manager
            .remove_sandbox_machine_locked(&machine_name, observer)
            .await?;
    }

    manager
        .init_sandbox_machine_locked(&machine_name, desired_memory_mb, observer)
        .await
}

pub(super) async fn reconcile_running_sandbox_machine_memory(
    manager: &HarnessRuntimeManager,
    settings: &ContainerExecutionSettings,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    if !sandbox_machine_required() {
        return Ok(());
    }
    let desired_memory_mb = container_machine_memory_mb(settings);
    let machine_name = sandbox_machine_name(manager.data_root());
    let machine_lock = sandbox_machine_singleflight_lock(&machine_name);
    let _machine_guard = machine_lock.lock().await;

    if !sandbox_machine_present(manager.data_root(), &machine_name).await? {
        observe_log(
            observer,
            HarnessSetupPhase::MachineCheck,
            HarnessSetupLogLevel::Warn,
            "local sandbox runtime is reachable but machine state could not be inspected; leaving memory profile unchanged",
        );
        return Ok(());
    }

    let actual_memory_mb = manager
        .inspect_sandbox_machine_memory_mb(&machine_name)
        .await?;
    if actual_memory_mb == Some(desired_memory_mb) {
        return Ok(());
    }
    if manager
        .should_defer_disk_isolated_machine_reconfiguration(settings, &machine_name, observer)
        .await?
    {
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Warn,
            "deferring local sandbox runtime memory reconfiguration because disk-isolated workspace volumes would be destroyed by machine recreation",
        );
        return Ok(());
    }
    if manager
        .has_running_workspace_containers_for_stopped_machine_reconfiguration(observer)
        .await?
    {
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Warn,
            "deferring local sandbox runtime memory reconfiguration until active workspace containers stop",
        );
        return Ok(());
    }
    let detail = actual_memory_mb
        .map(|value| format!("{value} MiB"))
        .unwrap_or_else(|| "unknown".to_string());
    observe_log(
        observer,
        HarnessSetupPhase::MachineStartOrInit,
        HarnessSetupLogLevel::Info,
        &format!(
            "reconfiguring local sandbox runtime memory from {detail} to {desired_memory_mb} MiB"
        ),
    );
    manager
        .remove_sandbox_machine_locked(&machine_name, observer)
        .await?;
    manager
        .init_sandbox_machine_locked(&machine_name, desired_memory_mb, observer)
        .await
}
