use super::*;

#[path = "shared_vm_lifecycle/layout_state.rs"]
mod layout_state;
#[path = "shared_vm_lifecycle/process_control.rs"]
mod process_control;

pub(super) use layout_state::*;
pub(super) use process_control::*;

pub(super) fn start_shared_vm(
    data_root: &Path,
    runtime_root: &Path,
    rootfs_image: &Path,
    kernel_path: &Path,
    initrd_path: &Path,
    runtime_version: String,
) -> Result<AvfLinuxSharedVmStateResponse> {
    let start_requested_at = std::time::Instant::now();
    for path in [runtime_root, rootfs_image, kernel_path, initrd_path] {
        if !path.exists() {
            bail!(
                "required AVF Linux runtime path is missing: {}",
                path.display()
            );
        }
    }
    let _ = prepare_runtime_layout(data_root)?;
    clear_shared_vm_shutdown_request(data_root);
    clear_shared_vm_memory_pressure_stop_request(data_root);
    let _start_lock_guard = acquire_shared_vm_start_lock(
        data_root,
        cold_boot_real_guest_exec_ready_timeout() + Duration::from_secs(30),
    )?;
    let state_path = shared_vm_state_path(data_root);
    let mut state = load_state(&state_path)?.unwrap_or_else(default_stopped_state);
    let saved_state_path = shared_vm_saved_state_path(data_root);
    let requested_runtime_shape_digest = shared_vm_runtime_shape_digest(
        runtime_root,
        rootfs_image,
        kernel_path,
        initrd_path,
        runtime_version.as_str(),
    );
    let requested_writable_surface_contract_digest =
        shared_vm_writable_surface_contract_digest(data_root)?;
    let runtime_shape_changed = match state.runtime_shape_digest.as_deref() {
        Some(previous_digest) => previous_digest != requested_runtime_shape_digest.as_str(),
        None => {
            state.runtime_version.as_deref() != Some(runtime_version.as_str())
                || state.runtime_root.as_ref().map(PathBuf::as_path) != Some(runtime_root)
                || state.initrd_path.as_ref().map(PathBuf::as_path) != Some(initrd_path)
        }
    };
    let writable_surface_contract_changed = state.writable_surface_contract_digest.as_deref()
        != Some(requested_writable_surface_contract_digest.as_str());
    let boot_contract_changed = runtime_shape_changed || writable_surface_contract_changed;
    let mut runtime_restart_note = None;
    let mut owner_alive = state.relay_pid.is_some_and(shared_vm_server_process_alive);
    let mut guest_alive = state
        .guest_agent_pid
        .is_some_and(shared_vm_server_process_alive);
    let launch_ready_for_reuse = matches!(
        state.transition_status,
        Some(AvfLinuxSharedVmTransitionStatus::Ready)
    );
    let mut already_running = if state.simulated {
        owner_alive && guest_alive
    } else {
        owner_alive && shared_vm_owner_guest_probe_ready(data_root)
    };
    already_running &= launch_ready_for_reuse;
    if already_running && boot_contract_changed {
        let restart_note = format!(
            "requested shared VM {} differs from the running shared VM; forcing a stop before restart",
            shared_vm_restart_reason_label(
                runtime_shape_changed,
                writable_surface_contract_changed
            )
        );
        append_shared_vm_log_line(data_root, &restart_note)?;
        let stopped = stop_shared_vm(data_root)
            .context("stopping already-running shared VM after boot contract change")?;
        if !matches!(stopped.state, AvfLinuxSharedVmLifecycleState::Stopped) {
            bail!(
                "expected shared VM to stop before boot contract restart, found {:?}",
                stopped.state
            );
        }
        reset_writable_shared_vm_runtime_state(data_root)
            .context("resetting writable shared VM runtime state after boot contract change")?;
        runtime_restart_note = Some(restart_note);
        state = load_state(&state_path)?.unwrap_or_else(default_stopped_state);
        owner_alive = state.relay_pid.is_some_and(shared_vm_server_process_alive);
        guest_alive = state
            .guest_agent_pid
            .is_some_and(shared_vm_server_process_alive);
        let launch_ready_for_reuse = matches!(
            state.transition_status,
            Some(AvfLinuxSharedVmTransitionStatus::Ready)
        );
        already_running = if state.simulated {
            owner_alive && guest_alive
        } else {
            owner_alive && shared_vm_owner_guest_probe_ready(data_root)
        };
        already_running &= launch_ready_for_reuse;
    }
    let mut stale_saved_state_note = None;
    if boot_contract_changed && saved_state_path.exists() {
        fs::remove_file(&saved_state_path).with_context(|| {
            format!(
                "removing stale saved AVF Linux VM state {}",
                saved_state_path.display()
            )
        })?;
        stale_saved_state_note = Some(format!(
            "discarded saved workspace VM state at {} because the staged {} changed",
            saved_state_path.display(),
            shared_vm_restart_reason_label(
                runtime_shape_changed,
                writable_surface_contract_changed
            )
        ));
    }
    if boot_contract_changed {
        let staged_rootfs_path = shared_vm_rootfs_path(data_root);
        if staged_rootfs_path.exists() {
            fs::remove_file(&staged_rootfs_path).with_context(|| {
                format!(
                    "removing stale writable AVF Linux rootfs {}",
                    staged_rootfs_path.display()
                )
            })?;
        }
    }

    let kernel_cmdline = load_shared_vm_kernel_cmdline(runtime_root)?;
    let (boot_kernel_path, kernel_materialization_note) =
        materialize_bootable_kernel_image(data_root, kernel_path)?;
    let (staged_rootfs_image, rootfs_materialization_note) =
        materialize_writable_rootfs_image(data_root, rootfs_image)?;
    let (data_disk_image, data_disk_materialization_note) = materialize_data_disk_image(data_root)?;

    let native_validation_note = if cfg!(test) {
        None
    } else {
        Some(
            validate_real_avf_linux_vm_configuration(
                data_root,
                &staged_rootfs_image,
                &data_disk_image,
                &boot_kernel_path,
                initrd_path,
                &kernel_cmdline,
            )
            .unwrap_or_else(|err| {
                format!("native AVF configuration validation did not complete: {err:#}")
            }),
        )
    };
    let (real_vm_supported, real_vm_support_note) = if cfg!(test) {
        (
            false,
            "test mode keeps the shared VM on the simulated relay path".to_string(),
        )
    } else {
        shared_vm_runtime_supports_real_guest_exec(runtime_root)
    };
    if already_running {
        state.state = AvfLinuxSharedVmLifecycleState::Running;
        state.runtime_root = Some(runtime_root.to_path_buf());
        state.rootfs_image = Some(staged_rootfs_image.clone());
        state.kernel_path = Some(boot_kernel_path.clone());
        state.initrd_path = Some(initrd_path.to_path_buf());
        state.runtime_version = Some(runtime_version);
        state.runtime_shape_digest = Some(requested_runtime_shape_digest.clone());
        state.writable_surface_contract_digest =
            Some(requested_writable_surface_contract_digest.clone());
        state.updated_at = Some(now_timestamp_string());
        state.last_started_at = state.updated_at.clone();
        if boot_contract_changed {
            state.last_saved_at = None;
        }
        state.transition_status = Some(AvfLinuxSharedVmTransitionStatus::Ready);
        state.last_start_outcome = Some(AvfLinuxSharedVmStartOutcome::AlreadyRunning);
        state.last_restore_error = None;
        state.notes = vec![if state.simulated {
            "shared VM relay and guest-agent processes were already alive; reusing the simulated shared VM"
                    .to_string()
        } else {
            "shared VM owner process was already alive; reusing the real AVF VM".to_string()
        }];
        if let Some(note) = kernel_materialization_note.clone() {
            state.notes.push(note);
        }
        if let Some(note) = rootfs_materialization_note.clone() {
            state.notes.push(note);
        }
        if let Some(note) = data_disk_materialization_note.clone() {
            state.notes.push(note);
        }
        state.notes.push(real_vm_support_note.clone());
        if let Some(note) = native_validation_note.clone() {
            state.notes.push(note);
        }
        if let Some(note) = stale_saved_state_note.clone() {
            state.notes.push(note);
        }
        if let Some(note) = runtime_restart_note.clone() {
            state.notes.push(note);
        }
        state.notes.push(format!(
            "shared VM start reused an already-running {} path in {}",
            if state.simulated {
                "simulated"
            } else {
                "real AVF"
            },
            format_duration_ms(start_requested_at.elapsed())
        ));
        let _ = append_shared_vm_log_line(
            data_root,
            &format!(
                "shared VM start reused an already-running {} path in {}",
                if state.simulated {
                    "simulated"
                } else {
                    "real AVF"
                },
                format_duration_ms(start_requested_at.elapsed())
            ),
        );
        persist_state(&state_path, &state)?;
        return shared_vm_state(data_root);
    }
    if let Some(pid) = state.relay_pid.take() {
        stop_shared_vm_server(pid);
    }
    if let Some(pid) = state.guest_agent_pid.take() {
        stop_shared_vm_server(pid);
    }
    clear_shared_vm_transient_artifacts(data_root);
    state.runtime_root = Some(runtime_root.to_path_buf());
    state.rootfs_image = Some(staged_rootfs_image.clone());
    state.kernel_path = Some(boot_kernel_path.clone());
    state.initrd_path = Some(initrd_path.to_path_buf());
    state.runtime_version = Some(runtime_version.clone());
    state.runtime_shape_digest = Some(requested_runtime_shape_digest.clone());
    state.writable_surface_contract_digest =
        Some(requested_writable_surface_contract_digest.clone());
    state.state = AvfLinuxSharedVmLifecycleState::Starting;
    state.updated_at = Some(now_timestamp_string());
    state.last_started_at = None;
    if boot_contract_changed {
        state.last_saved_at = None;
    }
    state.transition_status = Some(AvfLinuxSharedVmTransitionStatus::Scaffolded);
    state.last_start_outcome = None;
    state.last_restore_error = None;
    state.relay_pid = None;
    state.guest_agent_pid = None;
    state.notes =
        vec!["persisting AVF runtime paths before starting the shared VM owner".to_string()];
    persist_state(&state_path, &state)?;

    let saved_state_exists = saved_state_path.exists();
    let restore_eligible_for_start = !cfg!(test)
        && real_vm_supported
        && shared_vm_save_restore_supported()
        && saved_state_exists;
    let readiness_timeout = real_guest_exec_ready_timeout_for_start(
        rootfs_materialization_note.as_deref(),
        restore_eligible_for_start,
    );
    let timeout_reason = if rootfs_materialization_note.is_some() {
        "writable rootfs was materialized for this start"
    } else if restore_eligible_for_start {
        "saved state is eligible for restore on the real AVF path"
    } else if saved_state_exists {
        "saved state exists but this start cannot restore it on the selected path"
    } else {
        "no saved state exists yet"
    };
    let restore_unavailable_error = (saved_state_exists && !restore_eligible_for_start).then(|| {
        "saved workspace VM state was present, but this start could not use it and proceeded with a cold boot"
            .to_string()
    });
    append_shared_vm_log_line(
        data_root,
        &format!(
            "shared VM start selected {} path with readiness timeout {} because {}",
            if restore_eligible_for_start {
                "restore-candidate"
            } else {
                "cold-boot"
            },
            format_duration_ms(readiness_timeout),
            timeout_reason
        ),
    )?;
    let (relay_pid, guest_agent_pid, simulated, start_outcome, restore_error, mut notes) = if cfg!(
        test
    ) {
        (
            None,
            None,
            true,
            AvfLinuxSharedVmStartOutcome::ColdBoot,
            restore_unavailable_error.clone(),
            vec!["shared VM start was requested in test mode; state is simulated until actual AVF guest boot is implemented".to_string()],
        )
    } else if real_vm_supported {
        let relay_pid = spawn_real_shared_vm_owner(data_root, readiness_timeout)?;
        let owner_state = load_state(&state_path)?.ok_or_else(|| {
            anyhow::anyhow!(
                "shared VM owner reached launch-ready but state disappeared at {}",
                state_path.display()
            )
        })?;
        let start_outcome = owner_state.last_start_outcome.ok_or_else(|| {
            anyhow::anyhow!(
                "shared VM owner reached launch-ready without reporting last_start_outcome in {}",
                state_path.display()
            )
        })?;
        (
            Some(relay_pid),
            None,
            false,
            start_outcome,
            owner_state.last_restore_error.clone(),
            vec![
                "shared VM owner process is running and owns a real AVF Linux VM lifecycle"
                    .to_string(),
                real_vm_support_note.clone(),
            ],
        )
    } else {
        let guest_agent_pid = spawn_guest_agent_server(data_root)?;
        let relay_pid = match spawn_shared_vm_server(data_root) {
            Ok(pid) => pid,
            Err(err) => {
                stop_shared_vm_server(guest_agent_pid);
                return Err(err);
            }
        };
        (
            Some(relay_pid),
            Some(guest_agent_pid),
            true,
            AvfLinuxSharedVmStartOutcome::ColdBoot,
            restore_unavailable_error.clone(),
            vec![
                "shared VM relay and guest-agent processes are running; state remains simulated until actual AVF guest boot is implemented".to_string(),
                real_vm_support_note.clone(),
            ],
        )
    };
    state.state = AvfLinuxSharedVmLifecycleState::Running;
    state.runtime_root = Some(runtime_root.to_path_buf());
    state.rootfs_image = Some(staged_rootfs_image);
    state.kernel_path = Some(boot_kernel_path);
    state.initrd_path = Some(initrd_path.to_path_buf());
    state.runtime_version = Some(runtime_version);
    state.runtime_shape_digest = Some(requested_runtime_shape_digest);
    state.writable_surface_contract_digest = Some(requested_writable_surface_contract_digest);
    state.updated_at = Some(now_timestamp_string());
    state.last_started_at = state.updated_at.clone();
    state.transition_status = Some(AvfLinuxSharedVmTransitionStatus::Ready);
    state.last_start_outcome = Some(start_outcome);
    state.last_restore_error = restore_error;
    state.relay_pid = relay_pid;
    state.guest_agent_pid = guest_agent_pid;
    state.simulated = simulated;
    if let Some(note) = kernel_materialization_note {
        notes.push(note);
    }
    if let Some(note) = rootfs_materialization_note {
        notes.push(note);
    }
    if let Some(note) = data_disk_materialization_note {
        notes.push(note);
    }
    if let Some(note) = native_validation_note {
        notes.push(note);
    }
    if let Some(note) = stale_saved_state_note {
        notes.push(note);
    }
    if let Some(note) = runtime_restart_note {
        notes.push(note);
    }
    if let Some(note) = restore_unavailable_error {
        notes.push(note);
    }
    notes.push(format!(
        "shared VM start reached launch-ready in {} via {} path",
        format_duration_ms(start_requested_at.elapsed()),
        if simulated { "simulated" } else { "real AVF" }
    ));
    state.notes = notes;
    persist_state(&state_path, &state)?;
    shared_vm_state(data_root)
}

pub(super) fn stop_shared_vm(data_root: &Path) -> Result<AvfLinuxSharedVmStateResponse> {
    let state_path = shared_vm_state_path(data_root);
    let Some(mut state) = load_state(&state_path)? else {
        return Ok(AvfLinuxSharedVmStateResponse {
            protocol_version: HELPER_PROTOCOL_VERSION,
            protocol_schema: HELPER_PROTOCOL_SCHEMA,
            state: AvfLinuxSharedVmLifecycleState::Missing,
            vm_root: shared_vm_root(data_root),
            logs_root: shared_vm_logs_root(data_root),
            state_path,
            log_path: Some(shared_vm_log_path(data_root)),
            saved_state_path: Some(shared_vm_saved_state_path(data_root)),
            saved_state_exists: shared_vm_saved_state_path(data_root).exists(),
            runtime_root: None,
            rootfs_image: None,
            kernel_path: None,
            initrd_path: None,
            runtime_version: None,
            runtime_shape_digest: None,
            writable_surface_contract_digest: None,
            updated_at: Some(now_timestamp_string()),
            last_started_at: None,
            last_saved_at: None,
            last_stopped_at: None,
            transition_status: Some(AvfLinuxSharedVmTransitionStatus::Missing),
            last_start_outcome: None,
            last_stop_outcome: None,
            last_restore_error: None,
            last_save_error: None,
            relay_pid: None,
            guest_agent_pid: None,
            simulated: true,
            notes: vec!["shared VM layout is missing; nothing to stop".to_string()],
        });
    };
    if !state.simulated {
        if let Some(owner_pid) = state.relay_pid {
            request_shared_vm_shutdown(data_root)?;
            if wait_for_process_exit(owner_pid, SHARED_VM_SHUTDOWN_WAIT_TIMEOUT) {
                clear_shared_vm_shutdown_request(data_root);
                clear_shared_vm_transient_artifacts(data_root);
                let response = shared_vm_state(data_root)?;
                if matches!(response.state, AvfLinuxSharedVmLifecycleState::Stopped) {
                    return Ok(response);
                }
            }
        }
    }
    if let Some(pid) = state.relay_pid.take() {
        stop_shared_vm_server(pid);
    }
    if let Some(pid) = state.guest_agent_pid.take() {
        stop_shared_vm_server(pid);
    }
    clear_shared_vm_shutdown_request(data_root);
    clear_shared_vm_memory_pressure_stop_request(data_root);
    clear_shared_vm_transient_artifacts(data_root);
    state.state = AvfLinuxSharedVmLifecycleState::Stopped;
    state.updated_at = Some(now_timestamp_string());
    state.last_saved_at = None;
    state.last_stopped_at = state.updated_at.clone();
    state.transition_status = Some(AvfLinuxSharedVmTransitionStatus::Stopped);
    state.last_stop_outcome = Some(AvfLinuxSharedVmStopOutcome::ColdStop);
    let mut notes = vec![if state.simulated {
        "shared VM lifecycle scaffold is stopped".to_string()
    } else {
        "real shared AVF Linux VM is stopped".to_string()
    }];
    if let Some(note) = discard_stale_saved_state_for_cold_stop(data_root) {
        if note.starts_with("failed to discard") {
            state.last_save_error = Some(note.clone());
        } else {
            state.last_save_error = None;
        }
        notes.push(note);
    } else {
        state.last_save_error = None;
    }
    state.notes = notes;
    persist_state(&state_path, &state)?;
    shared_vm_state(data_root)
}

pub(super) fn shared_vm_runtime_shape_digest(
    runtime_root: &Path,
    rootfs_image: &Path,
    kernel_path: &Path,
    initrd_path: &Path,
    runtime_version: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(runtime_version.as_bytes());
    hasher.update(b"\0");
    hasher.update(runtime_root.display().to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(rootfs_image.display().to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(kernel_path.display().to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(initrd_path.display().to_string().as_bytes());
    hex::encode(hasher.finalize())
}

fn shared_vm_restart_reason_label(
    runtime_shape_changed: bool,
    writable_surface_contract_changed: bool,
) -> &'static str {
    match (runtime_shape_changed, writable_surface_contract_changed) {
        (true, true) => "runtime and writable-surface contract",
        (true, false) => "runtime",
        (false, true) => "writable-surface contract",
        (false, false) => "boot contract",
    }
}

pub(super) fn append_shared_vm_log_line(data_root: &Path, line: &str) -> Result<()> {
    use std::io::Write as _;

    let path = shared_vm_log_path(data_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("writing {}", path.display()))
}
