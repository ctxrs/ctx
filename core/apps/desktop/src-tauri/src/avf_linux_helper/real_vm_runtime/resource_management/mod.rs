use super::*;

mod data_disk;
mod memory_controller;
mod memory_policy;
mod state;
mod watchdog;

#[cfg(test)]
mod controller_safety;

#[cfg(unix)]
fn parse_single_u64_output(stdout: &[u8], context: &str) -> Result<u64> {
    let raw = String::from_utf8_lossy(stdout).trim().to_string();
    raw.parse::<u64>()
        .with_context(|| format!("parsing `{raw}` as an integer for {context}"))
}

#[cfg(target_os = "macos")]
pub(super) use self::data_disk::maybe_grow_shared_vm_data_disk;
#[cfg(test)]
pub(in super::super) use self::data_disk::{
    resolve_shared_vm_data_disk_growth_decision, SharedVmDataDiskGrowthDecision,
};
#[cfg(target_os = "macos")]
pub(super) use self::memory_controller::{
    host_available_memory_bytes, maybe_adjust_shared_vm_memory,
};
pub(super) use self::memory_policy::align_down_to_mebibyte;
#[cfg(test)]
pub(in super::super) use self::memory_policy::{
    resolve_shared_vm_memory_balloon_action, SharedVmMemoryBalloonAction,
};
#[cfg(target_os = "macos")]
pub(super) use self::state::SharedVmResourceState;
pub(in super::super) use self::watchdog::{
    resolve_shared_vm_memory_watchdog_exit_action, resolve_shared_vm_memory_watchdog_sample_action,
    SharedVmMemoryWatchdogExitAction, SharedVmMemoryWatchdogSampleAction,
};

#[cfg(test)]
pub(in super::super) use self::controller_safety::{
    replay_shared_vm_controller_safety_trace, shared_vm_controller_safety_trace_canonical_json,
    SharedVmControllerSafetyHostPressureState, SharedVmControllerSafetyPressureState,
    SharedVmControllerSafetyReplayDecision, SharedVmControllerSafetyReplayPhase,
    SharedVmControllerSafetyReplayState, SharedVmControllerSafetyReplayStep,
};
