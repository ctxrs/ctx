use super::super::*;
#[cfg(target_os = "macos")]
use super::memory_policy::shared_vm_host_pressure_state_name;
use super::memory_policy::{
    align_down_to_mebibyte, resolve_shared_vm_memory_balloon_action,
    shared_vm_memory_controller_decision_reason, shared_vm_memory_controller_reason_codes,
    shared_vm_memory_pressure_state_after, shared_vm_memory_pressure_state_before,
    SharedVmMemoryBalloonAction, MEBIBYTE_BYTES,
};
use super::parse_single_u64_output;
#[cfg(target_os = "macos")]
use super::state::SharedVmResourceState;
#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2_foundation::NSArray;
#[cfg(target_os = "macos")]
use objc2_virtualization::{VZMemoryBalloonDevice, VZVirtioTraditionalMemoryBalloonDevice};

#[cfg(all(target_os = "macos", unix))]
fn guest_memory_available_bytes(
    queue: &DispatchQueue,
    virtual_machine: &Retained<VZVirtualMachine>,
) -> Result<u64> {
    let result = run_owner_guest_exec_capture(
        queue,
        virtual_machine,
        Path::new("/"),
        "/bin/sh",
        &[
            "-lc".to_string(),
            "awk '/MemAvailable:/ { print $2 * 1024 }' /proc/meminfo".to_string(),
        ],
        Some("root"),
        HashMap::new(),
    )?;
    ensure_guest_exec_success(
        "reading guest MemAvailable bytes",
        GuestExecCaptureResult {
            exit_code: result.exit_code,
            stdout: result.stdout.clone(),
            stderr: result.stderr.clone(),
        },
    )?;
    parse_single_u64_output(&result.stdout, "guest MemAvailable bytes")
}

#[cfg(all(target_os = "macos", unix))]
fn compact_guest_memory_best_effort(
    queue: &DispatchQueue,
    virtual_machine: &Retained<VZVirtualMachine>,
) {
    let _ = run_owner_guest_exec_capture(
        queue,
        virtual_machine,
        Path::new("/"),
        "/bin/sh",
        &[
            "-lc".to_string(),
            "echo 1 > /proc/sys/vm/compact_memory 2>/dev/null || true".to_string(),
        ],
        Some("root"),
        HashMap::new(),
    );
}

#[cfg(target_os = "macos")]
pub(in super::super) fn host_available_memory_bytes(host_port: libc::mach_port_t) -> Result<u64> {
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size <= 0 {
        bail!("sysconf(_SC_PAGESIZE) returned an invalid page size");
    }

    let mut stats = std::mem::MaybeUninit::<libc::vm_statistics64_data_t>::uninit();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    let status = unsafe {
        libc::host_statistics64(
            host_port,
            libc::HOST_VM_INFO64,
            stats.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if status != 0 {
        bail!("host_statistics64(HOST_VM_INFO64) failed with kern_return_t {status}");
    }
    let stats = unsafe { stats.assume_init() };
    let available_pages = u64::from(stats.free_count)
        .saturating_add(u64::from(stats.inactive_count))
        .saturating_add(u64::from(stats.speculative_count));
    Ok(available_pages.saturating_mul(page_size as u64))
}

#[cfg(target_os = "macos")]
fn shared_vm_memory_target_bytes_on_queue(
    queue: &DispatchQueue,
    virtual_machine: *const VZVirtualMachine,
) -> Result<u64> {
    let virtual_machine_addr = virtual_machine as usize;
    exec_on_dispatch_queue(
        queue,
        "shared AVF Linux VM memory target dispatch",
        move || -> Result<u64> {
            let virtual_machine = unsafe { &*(virtual_machine_addr as *const VZVirtualMachine) };
            let balloon_devices: Retained<NSArray<VZMemoryBalloonDevice>> =
                unsafe { virtual_machine.memoryBalloonDevices() };
            let Some(balloon_device) = balloon_devices.iter().next() else {
                bail!("shared AVF Linux VM has no memory balloon devices configured");
            };
            let balloon_device = unsafe {
                &*((&*balloon_device) as *const _ as *const VZVirtioTraditionalMemoryBalloonDevice)
            };
            Ok(unsafe { balloon_device.targetVirtualMachineMemorySize() })
        },
    )?
}

#[cfg(target_os = "macos")]
fn request_shared_vm_memory_target_bytes_on_queue(
    queue: &DispatchQueue,
    virtual_machine: *const VZVirtualMachine,
    target_bytes: u64,
) -> Result<()> {
    let virtual_machine_addr = virtual_machine as usize;
    exec_on_dispatch_queue(
        queue,
        "shared AVF Linux VM memory target request dispatch",
        move || -> Result<()> {
            let virtual_machine = unsafe { &*(virtual_machine_addr as *const VZVirtualMachine) };
            let balloon_devices: Retained<NSArray<VZMemoryBalloonDevice>> =
                unsafe { virtual_machine.memoryBalloonDevices() };
            let Some(balloon_device) = balloon_devices.iter().next() else {
                bail!("shared AVF Linux VM has no memory balloon devices configured");
            };
            let balloon_device = unsafe {
                &*((&*balloon_device) as *const _ as *const VZVirtioTraditionalMemoryBalloonDevice)
            };
            unsafe {
                balloon_device.setTargetVirtualMachineMemorySize(target_bytes);
            }
            Ok(())
        },
    )?
}

#[cfg(target_os = "macos")]
fn append_shared_vm_memory_controller_decision_event(
    data_root: &Path,
    resource_state: &mut SharedVmResourceState,
    guest_probe_ready: bool,
    current_target_bytes: u64,
    host_available_bytes: u64,
    guest_available_bytes: Option<u64>,
    action: &SharedVmMemoryBalloonAction,
) {
    let ceiling_bytes =
        align_down_to_mebibyte(resource_state.memory.ceiling_bytes.max(MEBIBYTE_BYTES));
    let floor_bytes = align_down_to_mebibyte(resource_state.memory.floor_bytes.max(MEBIBYTE_BYTES))
        .min(ceiling_bytes);
    let current_target_bytes =
        align_down_to_mebibyte(current_target_bytes).clamp(floor_bytes, ceiling_bytes);
    let reason = shared_vm_memory_controller_decision_reason(
        action,
        guest_probe_ready,
        current_target_bytes,
        ceiling_bytes,
        floor_bytes,
        guest_available_bytes,
        host_available_bytes,
    );
    let (action_name, new_target_bytes, aggressive) = match action {
        SharedVmMemoryBalloonAction::NoAction => ("no_action", None, None),
        SharedVmMemoryBalloonAction::Reclaim {
            new_target_bytes,
            aggressive,
            ..
        } => ("reclaim", Some(*new_target_bytes), Some(*aggressive)),
        SharedVmMemoryBalloonAction::Grow {
            new_target_bytes, ..
        } => ("grow", Some(*new_target_bytes), None),
        SharedVmMemoryBalloonAction::EmergencyStop {
            current_target_bytes,
            ..
        } => ("emergency_stop", Some(*current_target_bytes), None),
    };
    let target_bytes_after = new_target_bytes.unwrap_or(current_target_bytes);
    let sequence = resource_state.memory.decision_trace.next_sequence();
    let new_target_bytes = new_target_bytes
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    let reason_codes = shared_vm_memory_controller_reason_codes(reason)
        .iter()
        .map(|code| format!("\"{code}\""))
        .collect::<Vec<_>>()
        .join(",");
    let pressure_state_before =
        shared_vm_memory_pressure_state_before(host_available_bytes, guest_available_bytes);
    let pressure_state_after = shared_vm_memory_pressure_state_after(
        action,
        reason,
        host_available_bytes,
        guest_available_bytes,
    );
    let host_pressure_state = shared_vm_host_pressure_state_name(host_available_bytes);
    let guest_available_bytes = guest_available_bytes
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    let aggressive = aggressive
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    let event = format!(
        concat!(
            "{{\"event\":\"ControllerDecisionEvent\",\"version\":1,",
            "\"schema_version\":1,",
            "\"trace_id\":\"{}\",\"epoch\":{},\"epoch_id\":{},\"sequence\":{},\"decision_seq\":{},",
            "\"action\":\"{}\",\"reason\":\"{}\",\"reason_codes\":[{}],",
            "\"target_bytes_before\":{},\"target_bytes_after\":{},",
            "\"pressure_state_before\":\"{}\",\"pressure_state_after\":\"{}\",",
            "\"host\":{{\"pressure_state\":\"{}\",\"available_bytes\":{}}},",
            "\"guest\":{{\"available_bytes\":{}}},",
            "\"context\":{{",
            "\"current_target_bytes\":{},\"new_target_bytes\":{},",
            "\"floor_bytes\":{},\"ceiling_bytes\":{},",
            "\"host_available_bytes\":{},\"guest_available_bytes\":{},",
            "\"guest_probe_ready\":{},\"aggressive\":{},",
            "\"host_memory_reserve_bytes\":{},\"host_memory_emergency_bytes\":{},",
            "\"guest_memory_grow_threshold_bytes\":{},\"memory_balloon_step_bytes\":{}",
            "}}}}"
        ),
        resource_state.memory.decision_trace.trace_id,
        resource_state.memory.decision_trace.epoch_millis,
        resource_state.memory.decision_trace.epoch_millis,
        sequence,
        sequence,
        action_name,
        reason,
        reason_codes,
        current_target_bytes,
        target_bytes_after,
        pressure_state_before,
        pressure_state_after,
        host_pressure_state,
        host_available_bytes,
        guest_available_bytes,
        current_target_bytes,
        new_target_bytes,
        floor_bytes,
        ceiling_bytes,
        host_available_bytes,
        guest_available_bytes,
        guest_probe_ready,
        aggressive,
        SHARED_VM_HOST_MEMORY_RESERVE_BYTES,
        SHARED_VM_HOST_MEMORY_EMERGENCY_BYTES,
        SHARED_VM_GUEST_MEMORY_GROW_THRESHOLD_BYTES,
        SHARED_VM_MEMORY_BALLOON_STEP_BYTES,
    );
    let _ = append_shared_vm_log_line(data_root, &event);
}

#[cfg(target_os = "macos")]
pub(in super::super) fn maybe_adjust_shared_vm_memory(
    queue: &DispatchQueue,
    virtual_machine: &Retained<VZVirtualMachine>,
    data_root: &Path,
    resource_state: &mut SharedVmResourceState,
) -> Result<()> {
    let virtual_machine_ptr = &**virtual_machine as *const VZVirtualMachine;
    let guest_probe_ready = shared_vm_owner_guest_probe_ready(data_root);
    let now = std::time::Instant::now();
    if now < resource_state.memory.next_check_at {
        return Ok(());
    }
    resource_state.memory.next_check_at = now + SHARED_VM_MEMORY_POLL_INTERVAL;

    let current_target_bytes = shared_vm_memory_target_bytes_on_queue(queue, virtual_machine_ptr)?;
    let host_available_bytes = host_available_memory_bytes(resource_state.host_port)?;
    let guest_available_bytes =
        if !guest_probe_ready || host_available_bytes < SHARED_VM_HOST_MEMORY_RESERVE_BYTES {
            None
        } else {
            guest_memory_available_bytes(queue, virtual_machine).ok()
        };
    let action = resolve_shared_vm_memory_balloon_action(
        current_target_bytes,
        resource_state.memory.ceiling_bytes,
        resource_state.memory.floor_bytes,
        guest_probe_ready,
        guest_available_bytes,
        host_available_bytes,
    );
    append_shared_vm_memory_controller_decision_event(
        data_root,
        resource_state,
        guest_probe_ready,
        current_target_bytes,
        host_available_bytes,
        guest_available_bytes,
        &action,
    );

    match action {
        SharedVmMemoryBalloonAction::NoAction => Ok(()),
        SharedVmMemoryBalloonAction::Reclaim {
            new_target_bytes,
            available_host_bytes,
            aggressive,
        } => {
            compact_guest_memory_best_effort(queue, virtual_machine);
            request_shared_vm_memory_target_bytes_on_queue(
                queue,
                virtual_machine_ptr,
                new_target_bytes,
            )?;
            append_shared_vm_log_line(
                data_root,
                &format!(
                    "requested AVF memory reclaim from {:.2} GiB to {:.2} GiB after host available memory fell to {:.2} GiB{}",
                    current_target_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                    new_target_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                    available_host_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                    if aggressive {
                        " (emergency reclaim to the configured floor)"
                    } else {
                        ""
                    }
                ),
            )
        }
        SharedVmMemoryBalloonAction::Grow {
            new_target_bytes,
            available_host_bytes,
            guest_available_bytes,
        } => {
            request_shared_vm_memory_target_bytes_on_queue(
                queue,
                virtual_machine_ptr,
                new_target_bytes,
            )?;
            append_shared_vm_log_line(
                data_root,
                &format!(
                    "requested AVF memory growth from {:.2} GiB to {:.2} GiB after guest MemAvailable fell to {:.2} GiB with host available memory at {:.2} GiB",
                    current_target_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                    new_target_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                    guest_available_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                    available_host_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                ),
            )
        }
        SharedVmMemoryBalloonAction::EmergencyStop {
            available_host_bytes,
            current_target_bytes,
            floor_bytes,
        } => {
            let note = format!(
                "host memory pressure emergency: host available memory fell to {:.2} GiB while the AVF memory target was already at its floor of {:.2} GiB (current target {:.2} GiB)",
                available_host_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                floor_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                current_target_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
            );
            request_shared_vm_memory_pressure_stop(data_root, &note)?;
            bail!("{note}");
        }
    }
}
