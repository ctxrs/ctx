use super::super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in super::super::super) enum SharedVmMemoryWatchdogSampleAction {
    NoAction {
        next_consecutive_emergency_samples: u32,
    },
    RequestStop {
        next_consecutive_emergency_samples: u32,
        available_host_bytes: u64,
    },
}

pub(in super::super::super) fn resolve_shared_vm_memory_watchdog_sample_action(
    consecutive_emergency_samples: u32,
    available_host_bytes: u64,
) -> SharedVmMemoryWatchdogSampleAction {
    if available_host_bytes >= SHARED_VM_HOST_MEMORY_EMERGENCY_BYTES {
        return SharedVmMemoryWatchdogSampleAction::NoAction {
            next_consecutive_emergency_samples: 0,
        };
    }

    let next_consecutive_emergency_samples = consecutive_emergency_samples.saturating_add(1);
    if next_consecutive_emergency_samples >= SHARED_VM_MEMORY_WATCHDOG_CONFIRMATION_POLLS {
        return SharedVmMemoryWatchdogSampleAction::RequestStop {
            next_consecutive_emergency_samples,
            available_host_bytes,
        };
    }

    SharedVmMemoryWatchdogSampleAction::NoAction {
        next_consecutive_emergency_samples,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in super::super::super) enum SharedVmMemoryWatchdogExitAction {
    OwnerExitedAfterRequest,
    OwnerExitedAfterSigterm,
    EscalateToSigkill,
}

pub(in super::super::super) fn resolve_shared_vm_memory_watchdog_exit_action(
    owner_exited_after_request: bool,
    owner_exited_after_sigterm: bool,
) -> SharedVmMemoryWatchdogExitAction {
    if owner_exited_after_request {
        SharedVmMemoryWatchdogExitAction::OwnerExitedAfterRequest
    } else if owner_exited_after_sigterm {
        SharedVmMemoryWatchdogExitAction::OwnerExitedAfterSigterm
    } else {
        SharedVmMemoryWatchdogExitAction::EscalateToSigkill
    }
}
