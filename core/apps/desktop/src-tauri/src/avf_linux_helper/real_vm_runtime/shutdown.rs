use super::*;

pub(in super::super) fn describe_saved_state_path_context(save_path: &Path) -> String {
    let mut details = vec![format!("save_path={}", save_path.display())];
    if let Some(parent) = save_path.parent() {
        details.push(format!("parent={}", parent.display()));
        details.push(format!("parent_exists={}", parent.exists()));
        match fs::metadata(parent) {
            Ok(metadata) => details.push(format!(
                "parent_readonly={}",
                metadata.permissions().readonly()
            )),
            Err(err) => details.push(format!("parent_metadata_error={err}")),
        }
    }
    details.join(", ")
}

#[cfg(target_os = "macos")]
pub(in super::super) fn persist_shared_vm_owner_error_state(
    state_path: &Path,
    state: &mut PersistedSharedVmState,
    note: String,
) -> Result<()> {
    state.state = AvfLinuxSharedVmLifecycleState::Error;
    state.simulated = false;
    state.updated_at = Some(now_timestamp_string());
    state.last_stopped_at = state.updated_at.clone();
    state.transition_status = None;
    state.relay_pid = None;
    state.guest_agent_pid = None;
    state.notes = vec![note];
    persist_state(state_path, state)
}

#[cfg(target_os = "macos")]
pub(in super::super) struct SharedVmShutdownOutcome {
    pub note: String,
    pub stop_outcome: AvfLinuxSharedVmStopOutcome,
    pub save_error: Option<String>,
    pub saved_state_written: bool,
}

#[cfg(target_os = "macos")]
pub(in super::super) fn shared_vm_start_outcome_log_label(
    outcome: AvfLinuxSharedVmStartOutcome,
) -> &'static str {
    match outcome {
        AvfLinuxSharedVmStartOutcome::AlreadyRunning => "already-running",
        AvfLinuxSharedVmStartOutcome::ColdBoot => "cold-boot",
        AvfLinuxSharedVmStartOutcome::Restored => "restore-hit",
        AvfLinuxSharedVmStartOutcome::ColdBootAfterRestoreFailure => {
            "cold-boot-after-restore-failure"
        }
    }
}

#[cfg(target_os = "macos")]
pub(in super::super) fn shutdown_real_shared_vm_for_exit(
    queue: &DispatchQueue,
    virtual_machine: &Retained<VZVirtualMachine>,
    data_root: &Path,
) -> SharedVmShutdownOutcome {
    let virtual_machine_ptr = &**virtual_machine as *const VZVirtualMachine;
    let mut notes = Vec::new();
    let mut saved_state_written = false;
    let mut save_error = None;
    let save_restore_supported = shared_vm_save_restore_supported();

    if save_restore_supported {
        match virtual_machine_state_on_queue(queue, virtual_machine_ptr) {
            Ok(initial_state) => {
                if matches!(
                    initial_state,
                    VZVirtualMachineState::Running | VZVirtualMachineState::Paused
                ) {
                    if initial_state == VZVirtualMachineState::Running {
                        match pause_virtual_machine_on_queue(queue, virtual_machine_ptr) {
                            Ok(()) => notes.push("paused workspace VM before save".to_string()),
                            Err(err) => {
                                let message = format!("pause before save failed: {err:#}");
                                save_error = Some(message.clone());
                                notes.push(message);
                            }
                        }
                    }

                    match virtual_machine_state_on_queue(queue, virtual_machine_ptr) {
                        Ok(VZVirtualMachineState::Paused) => {
                            let save_path = shared_vm_saved_state_path(data_root);
                            if let Some(parent) = save_path.parent() {
                                if let Err(err) = fs::create_dir_all(parent) {
                                    let message = format!(
                                        "failed to prepare saved-state directory {}: {err:#}",
                                        parent.display()
                                    );
                                    save_error = Some(message.clone());
                                    notes.push(message);
                                }
                            }
                            if let Err(err) = fs::remove_file(&save_path) {
                                if err.kind() != std::io::ErrorKind::NotFound {
                                    let message = format!(
                                        "failed to clear stale workspace VM save path {}: {err:#}; {}",
                                        save_path.display(),
                                        describe_saved_state_path_context(&save_path)
                                    );
                                    save_error = Some(message.clone());
                                    notes.push(message);
                                }
                            }
                            match save_virtual_machine_state_on_queue(
                                queue,
                                virtual_machine_ptr,
                                &save_path,
                            ) {
                                Ok(()) => {
                                    saved_state_written = true;
                                    notes.push(format!(
                                        "saved workspace VM state to {}",
                                        save_path.display()
                                    ));
                                }
                                Err(err) => {
                                    let message = format!(
                                        "saving workspace VM state to {} failed: {err:#}; {}",
                                        save_path.display(),
                                        describe_saved_state_path_context(&save_path)
                                    );
                                    save_error = Some(message.clone());
                                    notes.push(message);
                                }
                            }
                        }
                        Ok(other) => {
                            let message = format!(
                                "skipped save because workspace VM remained in state {other:?}"
                            );
                            save_error = Some(message.clone());
                            notes.push(message);
                        }
                        Err(err) => {
                            let message = format!(
                                "failed to re-check workspace VM state before save: {err:#}"
                            );
                            save_error = Some(message.clone());
                            notes.push(message);
                        }
                    }
                } else {
                    let message =
                        format!("skipped save because workspace VM was in state {initial_state:?}");
                    save_error = Some(message.clone());
                    notes.push(message);
                }
            }
            Err(err) => {
                let message =
                    format!("failed to query workspace VM state before shutdown: {err:#}");
                save_error = Some(message.clone());
                notes.push(message);
            }
        }
    } else {
        notes.push("save/restore unavailable on this host; stopping workspace VM cold".to_string());
    }

    let stop_outcome = if saved_state_written {
        AvfLinuxSharedVmStopOutcome::SavedStateWritten
    } else if !save_restore_supported {
        AvfLinuxSharedVmStopOutcome::ColdStopSaveUnsupported
    } else if save_error.is_some() {
        AvfLinuxSharedVmStopOutcome::ColdStopAfterSaveFailure
    } else {
        AvfLinuxSharedVmStopOutcome::ColdStop
    };

    if saved_state_written {
        notes.push("workspace VM owner exited after save without an additional stop".to_string());
        return SharedVmShutdownOutcome {
            note: notes.join("; "),
            stop_outcome,
            save_error,
            saved_state_written,
        };
    }

    if let Some(note) = discard_stale_saved_state_for_cold_stop(data_root) {
        if note.starts_with("failed to discard") && save_error.is_none() {
            save_error = Some(note.clone());
        }
        notes.push(note);
    }

    match virtual_machine_can_stop_on_queue(queue, virtual_machine_ptr) {
        Ok(true) => match stop_virtual_machine_on_queue(queue, virtual_machine_ptr) {
            Ok(()) => notes.push("stopped workspace VM owner cleanly".to_string()),
            Err(err) => notes.push(format!("workspace VM stop failed: {err:#}")),
        },
        Ok(false) => notes.push("workspace VM owner could not issue a clean stop".to_string()),
        Err(err) => notes.push(format!(
            "failed to check whether workspace VM could stop: {err:#}"
        )),
    }

    SharedVmShutdownOutcome {
        note: notes.join("; "),
        stop_outcome,
        save_error,
        saved_state_written,
    }
}
