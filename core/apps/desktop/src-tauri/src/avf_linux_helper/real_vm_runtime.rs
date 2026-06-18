use super::*;

#[path = "real_vm_runtime/guest_control.rs"]
mod guest_control;
#[path = "real_vm_runtime/owner.rs"]
mod owner;
#[path = "real_vm_runtime/processes.rs"]
mod processes;
#[path = "real_vm_runtime/readiness.rs"]
mod readiness;
#[path = "real_vm_runtime/resource_management/mod.rs"]
mod resource_management;
#[path = "real_vm_runtime/shutdown.rs"]
mod shutdown;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn mach_host_self() -> libc::mach_port_t;
}

#[cfg(all(target_os = "macos", unix))]
use self::guest_control::run_owner_guest_exec_capture;
pub(super) use self::guest_control::shared_vm_owner_guest_probe_ready;
#[cfg(all(test, target_os = "macos", unix))]
pub(super) use self::guest_control::{
    is_transient_guest_control_connect_nserror, relay_shared_vm_control_client,
};
pub(super) use self::owner::{run_shared_vm, run_shared_vm_memory_watchdog};
#[cfg(test)]
pub(super) use self::processes::stop_shared_vm_owner_after_readiness_failure;
#[cfg(test)]
pub(super) use self::processes::wait_for_socket_accepting_connections;
pub(super) use self::processes::{
    spawn_guest_agent_server, spawn_real_shared_vm_owner, spawn_shared_vm_server,
};
pub(super) use self::readiness::{
    cold_boot_real_guest_exec_ready_timeout, format_duration_ms,
    real_guest_exec_ready_timeout_for_start, reset_writable_shared_vm_runtime_state,
};
#[cfg(test)]
pub(super) use self::readiness::{
    default_real_guest_exec_ready_timeout, extract_shared_vm_readiness_phase_lines,
    shared_vm_guest_readiness_args, shared_vm_readiness_failure_requires_writable_rootfs_reset,
    summarize_shared_vm_readiness_phase_lines, wait_for_guest_control_ready_marker,
    wait_for_real_guest_exec_ready, wait_for_real_guest_exec_ready_with_owner_process,
    wait_for_real_guest_launch_ready_with_owner_process,
};
use self::resource_management::align_down_to_mebibyte;
#[cfg(target_os = "macos")]
use self::resource_management::{
    host_available_memory_bytes, maybe_adjust_shared_vm_memory, maybe_grow_shared_vm_data_disk,
    SharedVmResourceState,
};
#[cfg(test)]
pub(super) use self::resource_management::{
    replay_shared_vm_controller_safety_trace, shared_vm_controller_safety_trace_canonical_json,
    SharedVmControllerSafetyHostPressureState, SharedVmControllerSafetyPressureState,
    SharedVmControllerSafetyReplayDecision, SharedVmControllerSafetyReplayPhase,
    SharedVmControllerSafetyReplayState, SharedVmControllerSafetyReplayStep,
};
#[cfg(test)]
pub(super) use self::resource_management::{
    resolve_shared_vm_data_disk_growth_decision, resolve_shared_vm_memory_balloon_action,
    SharedVmDataDiskGrowthDecision, SharedVmMemoryBalloonAction,
};
pub(super) use self::resource_management::{
    resolve_shared_vm_memory_watchdog_exit_action, resolve_shared_vm_memory_watchdog_sample_action,
    SharedVmMemoryWatchdogExitAction, SharedVmMemoryWatchdogSampleAction,
};
pub(super) use self::shutdown::describe_saved_state_path_context;
#[cfg(all(test, target_os = "macos"))]
pub(super) use self::shutdown::persist_shared_vm_owner_error_state;
