use super::*;

#[cfg(target_os = "macos")]
pub(crate) fn exec_on_dispatch_queue<T, F>(queue: &DispatchQueue, label: &str, work: F) -> Result<T>
where
    T: Send + 'static,
    F: Send + FnOnce() -> T + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    queue.exec_async(move || {
        let _ = sender.send(work());
    });
    match receiver.recv_timeout(GUEST_EXEC_CONNECT_TIMEOUT) {
        Ok(value) => Ok(value),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            bail!("{label} timed out waiting for dispatch queue execution")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            bail!("{label} dispatch queue disconnected unexpectedly")
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn start_virtual_machine_on_queue(
    queue: &DispatchQueue,
    virtual_machine: *const VZVirtualMachine,
) -> Result<()> {
    let virtual_machine_addr = virtual_machine as usize;
    run_vm_completion_on_queue(
        queue,
        "shared AVF Linux VM start",
        VM_LIFECYCLE_COMPLETION_TIMEOUT,
        move |completion| unsafe {
            let virtual_machine = virtual_machine_addr as *const VZVirtualMachine;
            (&*virtual_machine).startWithCompletionHandler(completion);
        },
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn run_vm_completion_on_queue<F>(
    queue: &DispatchQueue,
    label: &str,
    timeout: Duration,
    invoke: F,
) -> Result<()>
where
    F: Send + FnOnce(&RcBlock<dyn Fn(*mut NSError)>) + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    queue.exec_async(move || {
        let completion = RcBlock::new(move |error: *mut NSError| {
            let result = if error.is_null() {
                Ok(())
            } else {
                let error = unsafe { &*error };
                Err(anyhow::anyhow!(format_nserror(error)))
            };
            let _ = sender.send(result);
        });
        invoke(&completion);
    });
    match receiver.recv_timeout(timeout) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(err).context(label.to_string()),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            bail!("{label} timed out waiting for completion")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            bail!("{label} completion handler disconnected unexpectedly")
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn pause_virtual_machine_on_queue(
    queue: &DispatchQueue,
    virtual_machine: *const VZVirtualMachine,
) -> Result<()> {
    let virtual_machine_addr = virtual_machine as usize;
    run_vm_completion_on_queue(
        queue,
        "shared AVF Linux VM pause",
        VM_LIFECYCLE_COMPLETION_TIMEOUT,
        move |completion| {
            let virtual_machine = unsafe { &*(virtual_machine_addr as *const VZVirtualMachine) };
            unsafe {
                virtual_machine.pauseWithCompletionHandler(completion);
            }
        },
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn resume_virtual_machine_on_queue(
    queue: &DispatchQueue,
    virtual_machine: *const VZVirtualMachine,
) -> Result<()> {
    let virtual_machine_addr = virtual_machine as usize;
    run_vm_completion_on_queue(
        queue,
        "shared AVF Linux VM resume",
        VM_LIFECYCLE_COMPLETION_TIMEOUT,
        move |completion| {
            let virtual_machine = unsafe { &*(virtual_machine_addr as *const VZVirtualMachine) };
            unsafe {
                virtual_machine.resumeWithCompletionHandler(completion);
            }
        },
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn stop_virtual_machine_on_queue(
    queue: &DispatchQueue,
    virtual_machine: *const VZVirtualMachine,
) -> Result<()> {
    let virtual_machine_addr = virtual_machine as usize;
    run_vm_completion_on_queue(
        queue,
        "shared AVF Linux VM stop",
        VM_LIFECYCLE_COMPLETION_TIMEOUT,
        move |completion| {
            let virtual_machine = unsafe { &*(virtual_machine_addr as *const VZVirtualMachine) };
            unsafe {
                virtual_machine.stopWithCompletionHandler(completion);
            }
        },
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn virtual_machine_state_on_queue(
    queue: &DispatchQueue,
    virtual_machine: *const VZVirtualMachine,
) -> Result<VZVirtualMachineState> {
    let virtual_machine_addr = virtual_machine as usize;
    exec_on_dispatch_queue(
        queue,
        "shared AVF Linux VM state dispatch",
        move || unsafe {
            let virtual_machine = &*(virtual_machine_addr as *const VZVirtualMachine);
            virtual_machine.state()
        },
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn virtual_machine_can_stop_on_queue(
    queue: &DispatchQueue,
    virtual_machine: *const VZVirtualMachine,
) -> Result<bool> {
    let virtual_machine_addr = virtual_machine as usize;
    exec_on_dispatch_queue(
        queue,
        "shared AVF Linux VM canStop dispatch",
        move || unsafe {
            let virtual_machine = &*(virtual_machine_addr as *const VZVirtualMachine);
            virtual_machine.canStop()
        },
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) fn save_virtual_machine_state_on_queue(
    queue: &DispatchQueue,
    virtual_machine: *const VZVirtualMachine,
    save_path: &Path,
) -> Result<()> {
    let virtual_machine_addr = virtual_machine as usize;
    let save_path = save_path.to_path_buf();
    run_vm_completion_on_queue(
        queue,
        "shared AVF Linux VM save",
        VM_SAVE_RESTORE_COMPLETION_TIMEOUT,
        move |completion| {
            let virtual_machine = unsafe { &*(virtual_machine_addr as *const VZVirtualMachine) };
            let save_url = file_url_for_path(&save_path);
            unsafe {
                virtual_machine.saveMachineStateToURL_completionHandler(&save_url, completion);
            }
        },
    )
}

#[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
pub(crate) fn save_virtual_machine_state_on_queue(
    _queue: &DispatchQueue,
    _virtual_machine: *const VZVirtualMachine,
    _save_path: &Path,
) -> Result<()> {
    bail!("AVF Linux VM save/restore requires an Apple silicon macOS host");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) fn restore_virtual_machine_state_on_queue(
    queue: &DispatchQueue,
    virtual_machine: *const VZVirtualMachine,
    save_path: &Path,
) -> Result<()> {
    let virtual_machine_addr = virtual_machine as usize;
    let save_path = save_path.to_path_buf();
    run_vm_completion_on_queue(
        queue,
        "shared AVF Linux VM restore",
        VM_SAVE_RESTORE_COMPLETION_TIMEOUT,
        move |completion| {
            let virtual_machine = unsafe { &*(virtual_machine_addr as *const VZVirtualMachine) };
            let save_url = file_url_for_path(&save_path);
            unsafe {
                virtual_machine.restoreMachineStateFromURL_completionHandler(&save_url, completion);
            }
        },
    )
}

#[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
pub(crate) fn restore_virtual_machine_state_on_queue(
    _queue: &DispatchQueue,
    _virtual_machine: *const VZVirtualMachine,
    _save_path: &Path,
) -> Result<()> {
    bail!("AVF Linux VM save/restore requires an Apple silicon macOS host");
}
