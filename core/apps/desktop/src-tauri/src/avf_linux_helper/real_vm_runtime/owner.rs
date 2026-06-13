#[cfg(target_os = "macos")]
use super::guest_control::service_real_shared_vm_control_clients;
use super::processes::spawn_shared_vm_memory_watchdog;
#[cfg(target_os = "macos")]
use super::shutdown::{
    persist_shared_vm_owner_error_state, shared_vm_start_outcome_log_label,
    shutdown_real_shared_vm_for_exit,
};
use super::*;

#[cfg(target_os = "macos")]
pub(in super::super) fn run_shared_vm(data_root: &Path) -> Result<()> {
    let state_path = shared_vm_state_path(data_root);
    let mut state = load_state(&state_path)?
        .ok_or_else(|| anyhow::anyhow!("shared VM state is missing at {}", state_path.display()))?;
    let rootfs_image = state
        .rootfs_image
        .clone()
        .ok_or_else(|| anyhow::anyhow!("shared VM state is missing rootfs_image"))?;
    let kernel_path = state
        .kernel_path
        .clone()
        .ok_or_else(|| anyhow::anyhow!("shared VM state is missing kernel_path"))?;
    let initrd_path = state
        .initrd_path
        .clone()
        .ok_or_else(|| anyhow::anyhow!("shared VM state is missing initrd_path"))?;
    let runtime_root = state
        .runtime_root
        .clone()
        .ok_or_else(|| anyhow::anyhow!("shared VM state is missing runtime_root"))?;
    let data_disk_image = shared_vm_data_disk_path(data_root);
    if !data_disk_image.is_file() {
        bail!(
            "shared AVF Linux data disk is missing at {}",
            data_disk_image.display()
        );
    }

    let listener = bind_shared_vm_control_listener(data_root)?;
    listener
        .set_nonblocking(true)
        .context("setting shared VM control listener nonblocking")?;
    let saved_state_path = shared_vm_saved_state_path(data_root);
    let preserving_seed_for_restore =
        shared_vm_save_restore_supported() && saved_state_path.is_file();
    append_shared_vm_log_line(
        data_root,
        &format!(
            "starting real AVF Linux VM (rootfs={}, data_disk={}, kernel={}, initrd={})",
            rootfs_image.display(),
            data_disk_image.display(),
            kernel_path.display(),
            initrd_path.display()
        ),
    )?;
    let seed_image =
        stage_shared_vm_cloud_init_seed(data_root, &runtime_root, preserving_seed_for_restore)?;
    if let Some(seed_image) = seed_image.as_ref() {
        let action = if preserving_seed_for_restore {
            "reusing"
        } else {
            "staged"
        };
        append_shared_vm_log_line(
            data_root,
            &format!(
                "{action} AVF cloud-init seed image at {}",
                seed_image.display()
            ),
        )?;
    }
    let kernel_cmdline = load_shared_vm_kernel_cmdline(&runtime_root)?;

    let queue = DispatchQueue::new(
        "rs.ctx.desktop.avf-linux.shared-vm",
        DispatchQueueAttr::SERIAL,
    );
    let build_virtual_machine = || {
        build_real_avf_linux_virtual_machine(
            data_root,
            &rootfs_image,
            &data_disk_image,
            &kernel_path,
            &initrd_path,
            seed_image.as_deref(),
            &kernel_cmdline,
            &queue,
        )
    };
    let mut virtual_machine = build_virtual_machine()?;
    let mut virtual_machine_ptr = &*virtual_machine as *const VZVirtualMachine;
    let mut start_outcome = AvfLinuxSharedVmStartOutcome::ColdBoot;
    let mut restore_error = None;
    if shared_vm_save_restore_supported() && saved_state_path.is_file() {
        append_shared_vm_log_line(
            data_root,
            &format!(
                "attempting to restore saved workspace VM state from {}",
                saved_state_path.display()
            ),
        )?;
        match restore_virtual_machine_state_on_queue(&queue, virtual_machine_ptr, &saved_state_path)
            .and_then(|_| resume_virtual_machine_on_queue(&queue, virtual_machine_ptr))
        {
            Ok(()) => {
                start_outcome = AvfLinuxSharedVmStartOutcome::Restored;
                append_shared_vm_log_line(
                    data_root,
                    &format!(
                        "restored workspace VM state from {} and resumed the guest",
                        saved_state_path.display()
                    ),
                )?;
            }
            Err(err) => {
                let message = format!(
                    "restoring saved workspace VM state from {} failed: {err:#}; {}",
                    saved_state_path.display(),
                    describe_saved_state_path_context(&saved_state_path)
                );
                start_outcome = AvfLinuxSharedVmStartOutcome::ColdBootAfterRestoreFailure;
                restore_error = Some(message.clone());
                append_shared_vm_log_line(
                    data_root,
                    &format!("{message}; continuing with a cold boot"),
                )?;
                let _ = fs::remove_file(&saved_state_path);
                virtual_machine = build_virtual_machine()?;
                virtual_machine_ptr = &*virtual_machine as *const VZVirtualMachine;
            }
        }
    }

    if start_outcome != AvfLinuxSharedVmStartOutcome::Restored {
        let virtual_machine_addr = virtual_machine_ptr as usize;
        let can_start =
            exec_on_dispatch_queue(&queue, "shared AVF Linux VM canStart", move || unsafe {
                let virtual_machine_ptr = virtual_machine_addr as *const VZVirtualMachine;
                (&*virtual_machine_ptr).canStart()
            })?;
        if !can_start {
            bail!("shared AVF Linux VM cannot be started from its current state");
        }
        start_virtual_machine_on_queue(&queue, virtual_machine_ptr)?;
        append_shared_vm_log_line(
            data_root,
            &format!(
                "real AVF Linux VM started successfully; forwarding host control socket {} to guest vsock port {}",
                shared_vm_control_socket_path(data_root).display(),
                SHARED_VM_GUEST_CONTROL_VSOCK_PORT
            ),
        )?;
    } else {
        append_shared_vm_log_line(
            data_root,
            &format!(
                "real AVF Linux VM restored successfully; forwarding host control socket {} to guest vsock port {}",
                shared_vm_control_socket_path(data_root).display(),
                SHARED_VM_GUEST_CONTROL_VSOCK_PORT
            ),
        )?;
    }
    append_shared_vm_log_line(
        data_root,
        &format!(
            "shared AVF Linux VM owner startup path: {}",
            shared_vm_start_outcome_log_label(start_outcome)
        ),
    )?;
    state.state = AvfLinuxSharedVmLifecycleState::Running;
    state.simulated = false;
    state.updated_at = Some(now_timestamp_string());
    state.last_started_at = state.updated_at.clone();
    state.transition_status = Some(AvfLinuxSharedVmTransitionStatus::Scaffolded);
    state.last_start_outcome = Some(start_outcome);
    state.last_restore_error = restore_error;
    state.relay_pid = Some(std::process::id());
    state.guest_agent_pid = None;
    persist_state(&state_path, &state)?;

    let min_cpu = unsafe { VZVirtualMachineConfiguration::minimumAllowedCPUCount() };
    let max_cpu = unsafe { VZVirtualMachineConfiguration::maximumAllowedCPUCount() };
    let min_memory = unsafe { VZVirtualMachineConfiguration::minimumAllowedMemorySize() };
    let max_memory = unsafe { VZVirtualMachineConfiguration::maximumAllowedMemorySize() };
    let sizing = resolved_avf_vm_sizing_for_host(min_cpu, max_cpu, min_memory, max_memory)?;
    let memory_floor_bytes =
        align_down_to_mebibyte(SHARED_VM_MIN_DEFAULT_MEMORY_BYTES.max(min_memory))
            .min(sizing.memory_size_bytes);
    let _watchdog_pid = spawn_shared_vm_memory_watchdog(data_root, std::process::id())
        .context("spawning shared AVF Linux VM memory watchdog")?;
    let mut resource_state =
        SharedVmResourceState::new(sizing.memory_size_bytes, memory_floor_bytes);
    loop {
        service_real_shared_vm_control_clients(&queue, &virtual_machine, &listener, data_root)?;
        if let Some(note) = shared_vm_memory_pressure_stop_requested_note(data_root)? {
            let shutdown = shutdown_real_shared_vm_for_exit(&queue, &virtual_machine, data_root);
            clear_shared_vm_memory_pressure_stop_request(data_root);
            state.last_stop_outcome = Some(shutdown.stop_outcome);
            state.last_save_error = shutdown.save_error.clone();
            state.last_saved_at = shutdown.saved_state_written.then(now_timestamp_string);
            let combined_note = format!("{note}; {}", shutdown.note);
            append_shared_vm_log_line(data_root, &combined_note)?;
            persist_shared_vm_owner_error_state(&state_path, &mut state, combined_note.clone())?;
            bail!("{combined_note}");
        }
        if shared_vm_shutdown_requested(data_root) {
            let shutdown = shutdown_real_shared_vm_for_exit(&queue, &virtual_machine, data_root);
            clear_shared_vm_shutdown_request(data_root);
            state.state = AvfLinuxSharedVmLifecycleState::Stopped;
            state.simulated = false;
            state.updated_at = Some(now_timestamp_string());
            state.last_saved_at = if shutdown.saved_state_written {
                state.updated_at.clone()
            } else {
                None
            };
            state.last_stopped_at = state.updated_at.clone();
            state.transition_status = Some(AvfLinuxSharedVmTransitionStatus::Stopped);
            state.last_stop_outcome = Some(shutdown.stop_outcome);
            state.last_save_error = shutdown.save_error;
            state.relay_pid = None;
            state.guest_agent_pid = None;
            state.notes = vec![shutdown.note];
            persist_state(&state_path, &state)?;
            append_shared_vm_log_line(
                data_root,
                "shared AVF Linux VM owner honored a shutdown request and exited cleanly",
            )?;
            return Ok(());
        }
        let vm_state = virtual_machine_state_on_queue(&queue, virtual_machine_ptr)?;
        if vm_state == VZVirtualMachineState::Running {
            if let Err(err) = maybe_grow_shared_vm_data_disk(
                &queue,
                &virtual_machine,
                data_root,
                &mut resource_state,
            ) {
                let shutdown =
                    shutdown_real_shared_vm_for_exit(&queue, &virtual_machine, data_root);
                state.last_stop_outcome = Some(shutdown.stop_outcome);
                state.last_save_error = shutdown.save_error.clone();
                state.last_saved_at = shutdown.saved_state_written.then(now_timestamp_string);
                let note = format!(
                    "workspace VM owner stopped because AVF data-disk maintenance failed: {err:#}; {}",
                    shutdown.note
                );
                append_shared_vm_log_line(data_root, &note)?;
                persist_shared_vm_owner_error_state(&state_path, &mut state, note)?;
                return Err(err).context("maintaining AVF data-disk capacity");
            }
            if let Err(err) = maybe_adjust_shared_vm_memory(
                &queue,
                &virtual_machine,
                data_root,
                &mut resource_state,
            ) {
                let shutdown =
                    shutdown_real_shared_vm_for_exit(&queue, &virtual_machine, data_root);
                state.last_stop_outcome = Some(shutdown.stop_outcome);
                state.last_save_error = shutdown.save_error.clone();
                state.last_saved_at = shutdown.saved_state_written.then(now_timestamp_string);
                let note = format!(
                    "workspace VM owner stopped because AVF memory maintenance failed: {err:#}; {}",
                    shutdown.note
                );
                append_shared_vm_log_line(data_root, &note)?;
                persist_shared_vm_owner_error_state(&state_path, &mut state, note)?;
                return Err(err).context("maintaining AVF memory pressure controls");
            }
        }
        if matches!(
            vm_state,
            VZVirtualMachineState::Running
                | VZVirtualMachineState::Starting
                | VZVirtualMachineState::Resuming
                | VZVirtualMachineState::Paused
                | VZVirtualMachineState::Pausing
                | VZVirtualMachineState::Saving
                | VZVirtualMachineState::Restoring
        ) {
            std::thread::sleep(SHARED_VM_CONTROL_POLL_INTERVAL);
            continue;
        }
        if vm_state == VZVirtualMachineState::Error {
            let note = "shared AVF Linux VM entered the Virtualization error state".to_string();
            append_shared_vm_log_line(data_root, &note)?;
            persist_shared_vm_owner_error_state(&state_path, &mut state, note.clone())?;
            bail!("{note}");
        }
        append_shared_vm_log_line(
            data_root,
            &format!("shared AVF Linux VM exited control loop with state {vm_state:?}"),
        )?;
        state.state = AvfLinuxSharedVmLifecycleState::Stopped;
        state.simulated = false;
        state.updated_at = Some(now_timestamp_string());
        state.last_saved_at = None;
        state.last_stopped_at = state.updated_at.clone();
        state.transition_status = Some(AvfLinuxSharedVmTransitionStatus::Stopped);
        state.last_stop_outcome = Some(AvfLinuxSharedVmStopOutcome::ColdStop);
        state.last_save_error = None;
        state.relay_pid = None;
        state.guest_agent_pid = None;
        let mut notes = vec![format!(
            "workspace VM owner exited its control loop with state {vm_state:?}"
        )];
        if let Some(note) = discard_stale_saved_state_for_cold_stop(data_root) {
            if note.starts_with("failed to discard") {
                state.last_save_error = Some(note.clone());
            }
            notes.push(note);
        }
        state.notes = notes;
        persist_state(&state_path, &state)?;
        return Ok(());
    }
}

#[cfg(not(target_os = "macos"))]
pub(in super::super) fn run_shared_vm(_data_root: &Path) -> Result<()> {
    bail!("real shared AVF Linux VM ownership requires macOS")
}

#[cfg(target_os = "macos")]
pub(in super::super) fn run_shared_vm_memory_watchdog(
    data_root: &Path,
    owner_pid: u32,
) -> Result<()> {
    let host_port = unsafe { mach_host_self() };
    let mut consecutive_emergency_samples = 0_u32;
    let mut logged_host_memory_error = false;

    loop {
        if !shared_vm_server_process_alive(owner_pid) || shared_vm_shutdown_requested(data_root) {
            return Ok(());
        }

        match host_available_memory_bytes(host_port) {
            Ok(available_host_bytes) => {
                logged_host_memory_error = false;
                match resolve_shared_vm_memory_watchdog_sample_action(
                    consecutive_emergency_samples,
                    available_host_bytes,
                ) {
                    SharedVmMemoryWatchdogSampleAction::NoAction {
                        next_consecutive_emergency_samples,
                    } => {
                        consecutive_emergency_samples = next_consecutive_emergency_samples;
                    }
                    SharedVmMemoryWatchdogSampleAction::RequestStop {
                        next_consecutive_emergency_samples: _next_consecutive_emergency_samples,
                        available_host_bytes,
                    } => {
                        let note = format!(
                            "host memory pressure emergency watchdog triggered: available host memory fell to {:.2} GiB, below the {:.2} GiB emergency floor",
                            available_host_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                            SHARED_VM_HOST_MEMORY_EMERGENCY_BYTES as f64
                                / (1024.0 * 1024.0 * 1024.0),
                        );
                        append_shared_vm_log_line(data_root, &note)?;
                        request_shared_vm_memory_pressure_stop(data_root, &note)?;
                        let exit_action = if wait_for_process_exit(
                            owner_pid,
                            SHARED_VM_MEMORY_WATCHDOG_EXIT_GRACE,
                        ) {
                            SharedVmMemoryWatchdogExitAction::OwnerExitedAfterRequest
                        } else {
                            append_shared_vm_log_line(
                                data_root,
                                "shared AVF Linux VM owner did not exit after the emergency memory stop request; sending SIGTERM",
                            )?;
                            stop_shared_vm_server(owner_pid);
                            resolve_shared_vm_memory_watchdog_exit_action(
                                false,
                                wait_for_process_exit(
                                    owner_pid,
                                    SHARED_VM_MEMORY_WATCHDOG_EXIT_GRACE,
                                ),
                            )
                        };

                        match exit_action {
                            SharedVmMemoryWatchdogExitAction::OwnerExitedAfterRequest
                            | SharedVmMemoryWatchdogExitAction::OwnerExitedAfterSigterm => {
                                return Ok(());
                            }
                            SharedVmMemoryWatchdogExitAction::EscalateToSigkill => unsafe {
                                libc::kill(owner_pid as i32, libc::SIGKILL);
                            },
                        }
                        append_shared_vm_log_line(
                            data_root,
                            "shared AVF Linux VM memory watchdog escalated to SIGKILL after the owner failed to exit under emergency host pressure",
                        )?;
                        return Ok(());
                    }
                }
            }
            Err(err) => {
                consecutive_emergency_samples = 0;
                if !logged_host_memory_error {
                    append_shared_vm_log_line(
                        data_root,
                        &format!(
                            "shared AVF Linux VM memory watchdog could not read host memory stats: {err:#}"
                        ),
                    )?;
                    logged_host_memory_error = true;
                }
            }
        }

        std::thread::sleep(SHARED_VM_MEMORY_WATCHDOG_POLL_INTERVAL);
    }
}

#[cfg(not(target_os = "macos"))]
pub(in super::super) fn run_shared_vm_memory_watchdog(
    _data_root: &Path,
    _owner_pid: u32,
) -> Result<()> {
    bail!("shared AVF Linux VM memory watchdog requires macOS")
}
