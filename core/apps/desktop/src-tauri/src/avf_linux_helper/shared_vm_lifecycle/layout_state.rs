use super::*;

#[derive(Debug)]
pub(crate) struct SharedVmStartLockGuard {
    path: PathBuf,
}

impl Drop for SharedVmStartLockGuard {
    fn drop(&mut self) {
        if self.path.exists() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(crate) fn parse_shared_vm_start_lock_pid(raw: &str) -> Option<u32> {
    raw.trim().parse::<u32>().ok().filter(|pid| *pid > 0)
}

fn try_acquire_shared_vm_start_lock(data_root: &Path) -> Result<Option<SharedVmStartLockGuard>> {
    let lock_path = shared_vm_start_lock_path(data_root);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(mut file) => {
            writeln!(file, "{}", std::process::id())
                .with_context(|| format!("writing {}", lock_path.display()))?;
            Ok(Some(SharedVmStartLockGuard { path: lock_path }))
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let holder_pid = fs::read_to_string(&lock_path)
                .ok()
                .and_then(|raw| parse_shared_vm_start_lock_pid(&raw));
            let stale_holder = match holder_pid {
                Some(pid) => !shared_vm_server_process_alive(pid),
                None => true,
            };
            if stale_holder {
                fs::remove_file(&lock_path)
                    .with_context(|| format!("removing stale {}", lock_path.display()))?;
            }
            Ok(None)
        }
        Err(err) => Err(err).with_context(|| format!("opening {}", lock_path.display())),
    }
}

pub(crate) fn acquire_shared_vm_start_lock(
    data_root: &Path,
    timeout: Duration,
) -> Result<SharedVmStartLockGuard> {
    let lock_path = shared_vm_start_lock_path(data_root);
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(guard) = try_acquire_shared_vm_start_lock(data_root)? {
            return Ok(guard);
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "timed out waiting for shared VM start lock {}",
                lock_path.display()
            );
        }
        std::thread::sleep(SHARED_VM_START_LOCK_POLL_INTERVAL);
    }
}

pub(crate) fn prepare_runtime_layout(data_root: &Path) -> Result<AvfLinuxRuntimeLayout> {
    let vm_root = shared_vm_root(data_root);
    let logs_root = shared_vm_logs_root(data_root);
    let state_path = shared_vm_state_path(data_root);
    let existed = vm_root.exists() && logs_root.exists() && state_path.exists();

    fs::create_dir_all(&vm_root).with_context(|| format!("creating {}", vm_root.display()))?;
    fs::create_dir_all(&logs_root).with_context(|| format!("creating {}", logs_root.display()))?;

    if !state_path.exists() {
        let state = PersistedSharedVmState {
            state: AvfLinuxSharedVmLifecycleState::Stopped,
            guest_identity: supported_guest_identity(),
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
            transition_status: None,
            last_start_outcome: None,
            last_stop_outcome: None,
            last_restore_error: None,
            last_save_error: None,
            relay_pid: None,
            guest_agent_pid: None,
            simulated: true,
            notes: vec![
                "shared VM lifecycle scaffold is present, but actual AVF guest boot is not implemented yet".to_string(),
            ],
        };
        persist_state(&state_path, &state)?;
    }

    Ok(AvfLinuxRuntimeLayout {
        protocol_version: HELPER_PROTOCOL_VERSION,
        protocol_schema: HELPER_PROTOCOL_SCHEMA,
        vm_root,
        logs_root,
        state_path,
        layout_status: if existed {
            AvfLinuxRuntimeLayoutStatus::AlreadyPresent
        } else {
            AvfLinuxRuntimeLayoutStatus::Prepared
        },
        notes: vec![
            "shared AVF Linux VM layout is scaffolded under the daemon data root".to_string(),
        ],
    })
}

pub(crate) fn shared_vm_state(data_root: &Path) -> Result<AvfLinuxSharedVmStateResponse> {
    let vm_root = shared_vm_root(data_root);
    let logs_root = shared_vm_logs_root(data_root);
    let state_path = shared_vm_state_path(data_root);
    let log_path = shared_vm_log_path(data_root);
    let mut persisted = load_state(&state_path)?;
    if let Some(state) = persisted.as_mut() {
        let missing_owner = state
            .relay_pid
            .is_some_and(|pid| !shared_vm_server_process_alive(pid));
        let missing_simulated_guest = state.simulated
            && state
                .guest_agent_pid
                .is_some_and(|pid| !shared_vm_server_process_alive(pid));
        if matches!(state.state, AvfLinuxSharedVmLifecycleState::Running)
            && (missing_owner || missing_simulated_guest)
        {
            if let Some(pid) = state.relay_pid.take() {
                stop_shared_vm_server(pid);
            }
            if let Some(pid) = state.guest_agent_pid.take() {
                stop_shared_vm_server(pid);
            }
            let memory_pressure_note = shared_vm_memory_pressure_stop_requested_note(data_root)?;
            state.state = if memory_pressure_note.is_some() {
                AvfLinuxSharedVmLifecycleState::Error
            } else {
                AvfLinuxSharedVmLifecycleState::Stopped
            };
            state.updated_at = Some(now_timestamp_string());
            state.last_stopped_at = state.updated_at.clone();
            state.last_saved_at = None;
            state.last_stop_outcome = Some(AvfLinuxSharedVmStopOutcome::ColdStop);
            state.last_save_error = None;
            state.transition_status = if memory_pressure_note.is_some() {
                None
            } else {
                Some(AvfLinuxSharedVmTransitionStatus::Stopped)
            };
            state.notes = vec![if let Some(note) = memory_pressure_note {
                clear_shared_vm_memory_pressure_stop_request(data_root);
                format!(
                    "shared VM owner exited after an emergency host-memory stop request: {note}"
                )
            } else if state.simulated {
                "shared VM relay or simulated guest-agent process was not alive; marking the scaffolded VM stopped"
                    .to_string()
            } else {
                "shared VM owner process was not alive; marking the AVF VM stopped".to_string()
            }];
            persist_state(&state_path, state)?;
        }
    }
    Ok(map_state_response(
        persisted.as_ref(),
        data_root,
        vm_root,
        logs_root,
        state_path,
        log_path,
    ))
}

pub(crate) fn discard_stale_saved_state_for_cold_stop(data_root: &Path) -> Option<String> {
    let saved_state_path = shared_vm_saved_state_path(data_root);
    match fs::remove_file(&saved_state_path) {
        Ok(()) => Some(format!(
            "discarded stale workspace VM saved state at {} because this stop did not produce a fresh save",
            saved_state_path.display()
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => Some(format!(
            "failed to discard stale workspace VM saved state at {}: {err:#}; {}",
            saved_state_path.display(),
            describe_saved_state_path_context(&saved_state_path)
        )),
    }
}

pub(super) fn clear_shared_vm_transient_artifacts(data_root: &Path) {
    for path in [
        shared_vm_control_socket_path(data_root),
        shared_vm_guest_agent_socket_path(data_root),
        shared_vm_guest_control_ready_path(data_root),
        shared_vm_guest_control_failed_path(data_root),
    ] {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }
}
