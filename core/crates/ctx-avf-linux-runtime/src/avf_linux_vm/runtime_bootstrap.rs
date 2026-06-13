use super::*;

const EXISTING_SHARED_VM_START_WAIT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(630);
const EXISTING_SHARED_VM_START_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(250);

pub fn runtime_available() -> bool {
    probe_helper().map(|probe| probe.supported).unwrap_or(false)
}

pub fn runtime_state(data_root: &Path) -> Result<(bool, bool)> {
    let helper_ready = probe_helper().map(|probe| probe.supported).unwrap_or(false);
    let runtime_ready = runtime_ready(data_root)?;
    if !helper_ready || !runtime_ready {
        return Ok((false, runtime_ready));
    }
    Ok((true, runtime_ready))
}

pub async fn ensure_workspace_vm_ready_with_observer(
    data_root: &Path,
    workspace_id: WorkspaceId,
    _settings: &ContainerExecutionSettings,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<AvfLinuxSharedVmState> {
    observe_phase(
        observer,
        HarnessSetupPhase::MachineCheck,
        "checking AVF Linux helper availability",
    );
    let probe = probe_helper()?;
    if !probe.supported {
        bail!("AVF Linux helper reported that this host is unsupported");
    }
    observe_log(
        observer,
        HarnessSetupPhase::MachineCheck,
        HarnessSetupLogLevel::Info,
        &format!(
            "using AVF Linux helper {} on {}/{}",
            probe.helper_version, probe.host_os, probe.host_arch
        ),
    );
    for note in &probe.notes {
        observe_log(
            observer,
            HarnessSetupPhase::MachineCheck,
            HarnessSetupLogLevel::Info,
            note,
        );
    }

    let runtime = ensure_managed_avf_linux_guest_runtime(data_root, observer, None).await?;
    observe_log(
        observer,
        HarnessSetupPhase::ArtifactDownload,
        HarnessSetupLogLevel::Info,
        &format!(
            "AVF Linux guest runtime {} is ready (rootfs={}, kernel={}, initrd={}, guest_agent={}, egress_proxy={}, container_stack={})",
            runtime.version,
            runtime.rootfs_image.display(),
            runtime.kernel_path.display(),
            runtime.initrd_path.display(),
            runtime
                .guest_agent_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "none".to_string()),
            runtime
                .egress_proxy_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "none".to_string()),
            runtime.container_stack_path.display()
        ),
    );
    observe_phase(
        observer,
        HarnessSetupPhase::MachineCheck,
        "checking AVF Linux workspace VM state",
    );

    let vm_data_root = workspace_vm_data_root(data_root, workspace_id);
    let layout = prepare_runtime_layout(&vm_data_root)?;
    let state = workspace_vm_state(data_root, workspace_id)?;
    observe_log(
        observer,
        HarnessSetupPhase::MachineCheck,
        HarnessSetupLogLevel::Info,
        &format!(
            "AVF Linux workspace VM layout is ready for workspace {} at {} (logs={}, state={:?})",
            workspace_id.0,
            layout.vm_root.display(),
            layout.logs_root.display(),
            state.state
        ),
    );
    if shared_vm_is_launch_ready(&state) {
        observe_log(
            observer,
            HarnessSetupPhase::MachineCheck,
            HarnessSetupLogLevel::Info,
            &format!(
                "AVF Linux workspace VM is already running for workspace {}",
                workspace_id.0
            ),
        );
        return Ok(state);
    }
    if shared_vm_start_in_progress(&state) {
        observe_log(
            observer,
            HarnessSetupPhase::MachineCheck,
            HarnessSetupLogLevel::Info,
            &format!(
                "AVF Linux workspace VM for workspace {} already has an in-flight startup (state={:?}, transition_status={:?}); waiting for it to finish",
                workspace_id.0, state.state, state.transition_status
            ),
        );
        observe_phase(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            "waiting for AVF Linux workspace VM startup to finish",
        );
        return wait_for_existing_workspace_vm_launch_ready_with_observer(
            data_root,
            workspace_id,
            observer,
        )
        .await;
    }

    observe_phase(
        observer,
        HarnessSetupPhase::MachineStartOrInit,
        "starting AVF Linux workspace VM",
    );
    let started = start_workspace_vm(data_root, workspace_id, &runtime)?;
    observe_log(
        observer,
        HarnessSetupPhase::MachineStartOrInit,
        HarnessSetupLogLevel::Info,
        &format!(
            "AVF Linux workspace VM start completed for workspace {} with state {:?} (simulated={})",
            workspace_id.0, started.state, started.simulated
        ),
    );
    for note in &started.notes {
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Info,
            note,
        );
    }
    if !matches!(started.state, AvfLinuxSharedVmLifecycleState::Running) {
        bail!(
            "AVF Linux workspace VM for workspace {} did not reach a running state after start (state={:?})",
            workspace_id.0,
            started.state,
        );
    }
    if shared_vm_is_launch_ready(&started) {
        return Ok(started);
    }

    observe_log(
        observer,
        HarnessSetupPhase::MachineStartOrInit,
        HarnessSetupLogLevel::Info,
        &format!(
            "AVF Linux workspace VM start for workspace {} returned running but not yet launch-ready (transition_status={:?}); waiting for readiness to finish",
            workspace_id.0, started.transition_status
        ),
    );
    wait_for_existing_workspace_vm_launch_ready_with_observer(data_root, workspace_id, observer)
        .await
}

pub async fn ensure_shared_vm_ready_with_observer(
    data_root: &Path,
    settings: &ContainerExecutionSettings,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<AvfLinuxSharedVmState> {
    ensure_workspace_vm_ready_with_observer(
        data_root,
        WorkspaceId(uuid::Uuid::nil()),
        settings,
        observer,
    )
    .await
}

async fn wait_for_existing_workspace_vm_launch_ready_with_observer(
    data_root: &Path,
    workspace_id: WorkspaceId,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<AvfLinuxSharedVmState> {
    let deadline = tokio::time::Instant::now() + EXISTING_SHARED_VM_START_WAIT_TIMEOUT;
    loop {
        let state = workspace_vm_state(data_root, workspace_id)?;
        if shared_vm_is_launch_ready(&state) {
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Info,
                &format!(
                    "AVF Linux workspace VM startup finished for workspace {} and is now launch-ready",
                    workspace_id.0
                ),
            );
            return Ok(state);
        }

        if !shared_vm_start_in_progress(&state) {
            bail!(
                "AVF Linux workspace VM startup ended before becoming launch-ready for workspace {} (state={:?}, transition_status={:?}, notes={})",
                workspace_id.0,
                state.state,
                state.transition_status,
                state.notes.join(" | ")
            );
        }

        if tokio::time::Instant::now() >= deadline {
            bail!(
                "timed out waiting for in-flight AVF Linux workspace VM startup to become launch-ready for workspace {} (state={:?}, transition_status={:?})",
                workspace_id.0,
                state.state,
                state.transition_status
            );
        }

        tokio::time::sleep(EXISTING_SHARED_VM_START_POLL_INTERVAL).await;
    }
}

pub async fn ensure_guest_worktree_from_host_copy(
    data_root: &Path,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
    host_workspace_root: &Path,
    base_commit_sha: &str,
    branch_name: &str,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<AvfLinuxGuestWorktree> {
    observe_phase(
        observer,
        HarnessSetupPhase::ContainerStartOrCreate,
        "preparing AVF Linux guest worktree",
    );
    let prepared = prepare_guest_worktree(
        data_root,
        workspace_id,
        worktree_id,
        host_workspace_root,
        base_commit_sha,
        branch_name,
    )?;
    observe_log(
        observer,
        HarnessSetupPhase::ContainerStartOrCreate,
        HarnessSetupLogLevel::Info,
        &format!(
            "AVF Linux guest worktree {} is {} at {} (shadow={})",
            worktree_id.0,
            match prepared.status {
                AvfLinuxGuestWorktreeStatus::Prepared => "prepared",
                AvfLinuxGuestWorktreeStatus::AlreadyPresent => "already present",
            },
            prepared.guest_root.display(),
            prepared.host_shadow_root.display()
        ),
    );
    for note in &prepared.notes {
        observe_log(
            observer,
            HarnessSetupPhase::ContainerStartOrCreate,
            HarnessSetupLogLevel::Info,
            note,
        );
    }
    Ok(prepared)
}

pub async fn prefetch_runtime_with_observer(
    data_root: &Path,
    _settings: &ContainerExecutionSettings,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    observe_phase(
        observer,
        HarnessSetupPhase::MachineCheck,
        "checking AVF Linux helper availability",
    );
    let probe = probe_helper()?;
    if !probe.supported {
        bail!("AVF Linux helper reported that this host is unsupported");
    }
    observe_log(
        observer,
        HarnessSetupPhase::MachineCheck,
        HarnessSetupLogLevel::Info,
        &format!(
            "using AVF Linux helper {} on {}/{}",
            probe.helper_version, probe.host_os, probe.host_arch
        ),
    );
    for note in &probe.notes {
        observe_log(
            observer,
            HarnessSetupPhase::MachineCheck,
            HarnessSetupLogLevel::Info,
            note,
        );
    }
    let runtime = ensure_managed_avf_linux_guest_runtime(data_root, observer, None).await?;
    observe_log(
        observer,
        HarnessSetupPhase::ArtifactDownload,
        HarnessSetupLogLevel::Info,
        &format!(
            "AVF Linux guest runtime {} is ready (rootfs={}, kernel={}, initrd={}, guest_agent={}, egress_proxy={}, container_stack={})",
            runtime.version,
            runtime.rootfs_image.display(),
            runtime.kernel_path.display(),
            runtime.initrd_path.display(),
            runtime
                .guest_agent_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "none".to_string()),
            runtime
                .egress_proxy_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "none".to_string()),
            runtime.container_stack_path.display()
        ),
    );
    observe_log(
        observer,
        HarnessSetupPhase::MachineCheck,
        HarnessSetupLogLevel::Info,
        &format!(
            "AVF Linux runtime is ready; workspace VM instances will materialize lazily from {}",
            runtime.runtime_root.display(),
        ),
    );
    Ok(())
}
