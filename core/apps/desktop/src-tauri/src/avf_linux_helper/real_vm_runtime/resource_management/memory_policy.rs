use super::super::*;

pub(super) const MEBIBYTE_BYTES: u64 = 1024 * 1024;

pub(in super::super) fn align_down_to_mebibyte(bytes: u64) -> u64 {
    bytes - (bytes % MEBIBYTE_BYTES)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in super::super::super) enum SharedVmMemoryBalloonAction {
    NoAction,
    Reclaim {
        new_target_bytes: u64,
        available_host_bytes: u64,
        aggressive: bool,
    },
    Grow {
        new_target_bytes: u64,
        available_host_bytes: u64,
        guest_available_bytes: u64,
    },
    EmergencyStop {
        available_host_bytes: u64,
        current_target_bytes: u64,
        floor_bytes: u64,
    },
}

pub(in super::super::super) fn resolve_shared_vm_memory_balloon_action(
    current_target_bytes: u64,
    ceiling_bytes: u64,
    floor_bytes: u64,
    guest_probe_ready: bool,
    guest_available_bytes: Option<u64>,
    host_available_bytes: u64,
) -> SharedVmMemoryBalloonAction {
    let ceiling_bytes = align_down_to_mebibyte(ceiling_bytes.max(MEBIBYTE_BYTES));
    let floor_bytes = align_down_to_mebibyte(floor_bytes.max(MEBIBYTE_BYTES)).min(ceiling_bytes);
    let current_target_bytes =
        align_down_to_mebibyte(current_target_bytes).clamp(floor_bytes, ceiling_bytes);

    if !guest_probe_ready {
        return SharedVmMemoryBalloonAction::NoAction;
    }

    if host_available_bytes < SHARED_VM_HOST_MEMORY_EMERGENCY_BYTES {
        if current_target_bytes <= floor_bytes {
            return SharedVmMemoryBalloonAction::EmergencyStop {
                available_host_bytes: host_available_bytes,
                current_target_bytes,
                floor_bytes,
            };
        }
        return SharedVmMemoryBalloonAction::Reclaim {
            new_target_bytes: floor_bytes,
            available_host_bytes: host_available_bytes,
            aggressive: true,
        };
    }

    if host_available_bytes < SHARED_VM_HOST_MEMORY_RESERVE_BYTES
        && current_target_bytes > floor_bytes
    {
        return SharedVmMemoryBalloonAction::Reclaim {
            new_target_bytes: current_target_bytes
                .saturating_sub(SHARED_VM_MEMORY_BALLOON_STEP_BYTES)
                .max(floor_bytes),
            available_host_bytes: host_available_bytes,
            aggressive: false,
        };
    }

    let Some(guest_available_bytes) = guest_available_bytes else {
        return SharedVmMemoryBalloonAction::NoAction;
    };
    if guest_available_bytes >= SHARED_VM_GUEST_MEMORY_GROW_THRESHOLD_BYTES
        || current_target_bytes >= ceiling_bytes
        || host_available_bytes
            <= SHARED_VM_HOST_MEMORY_RESERVE_BYTES + SHARED_VM_MEMORY_BALLOON_STEP_BYTES
    {
        return SharedVmMemoryBalloonAction::NoAction;
    }

    SharedVmMemoryBalloonAction::Grow {
        new_target_bytes: current_target_bytes
            .saturating_add(SHARED_VM_MEMORY_BALLOON_STEP_BYTES)
            .min(ceiling_bytes),
        available_host_bytes: host_available_bytes,
        guest_available_bytes,
    }
}

pub(super) fn shared_vm_memory_controller_decision_reason(
    action: &SharedVmMemoryBalloonAction,
    guest_probe_ready: bool,
    current_target_bytes: u64,
    ceiling_bytes: u64,
    floor_bytes: u64,
    guest_available_bytes: Option<u64>,
    host_available_bytes: u64,
) -> &'static str {
    match action {
        SharedVmMemoryBalloonAction::NoAction => {
            if !guest_probe_ready {
                "guest_probe_not_ready"
            } else if host_available_bytes < SHARED_VM_HOST_MEMORY_RESERVE_BYTES
                && current_target_bytes <= floor_bytes
            {
                "at_floor_while_host_below_reserve"
            } else if let Some(guest_available_bytes) = guest_available_bytes {
                if guest_available_bytes >= SHARED_VM_GUEST_MEMORY_GROW_THRESHOLD_BYTES {
                    "guest_memory_above_growth_threshold"
                } else if current_target_bytes >= ceiling_bytes {
                    "at_memory_ceiling"
                } else if host_available_bytes
                    <= SHARED_VM_HOST_MEMORY_RESERVE_BYTES + SHARED_VM_MEMORY_BALLOON_STEP_BYTES
                {
                    "host_growth_budget_exhausted"
                } else {
                    "no_action"
                }
            } else {
                "guest_memory_unavailable"
            }
        }
        SharedVmMemoryBalloonAction::Reclaim { aggressive, .. } => {
            if *aggressive {
                "host_memory_emergency"
            } else {
                "host_memory_below_reserve"
            }
        }
        SharedVmMemoryBalloonAction::Grow { .. } => "guest_memory_below_growth_threshold",
        SharedVmMemoryBalloonAction::EmergencyStop { .. } => "host_memory_emergency_at_floor",
    }
}

pub(super) fn shared_vm_memory_controller_reason_codes(reason: &str) -> &'static [&'static str] {
    match reason {
        "host_memory_emergency" | "host_memory_emergency_at_floor" => &["host_emergency"],
        "host_memory_below_reserve"
        | "at_floor_while_host_below_reserve"
        | "host_growth_budget_exhausted" => &["host_pressure_reclaim"],
        "guest_memory_below_growth_threshold" | "at_memory_ceiling" => &["guest_demand_grow"],
        _ => &["stable_band"],
    }
}

#[cfg(target_os = "macos")]
pub(super) fn shared_vm_host_pressure_state_name(host_available_bytes: u64) -> &'static str {
    if host_available_bytes < SHARED_VM_HOST_MEMORY_EMERGENCY_BYTES {
        "emergency"
    } else if host_available_bytes < SHARED_VM_HOST_MEMORY_RESERVE_BYTES {
        "elevated"
    } else {
        "normal"
    }
}

pub(super) fn shared_vm_memory_pressure_state_before(
    host_available_bytes: u64,
    guest_available_bytes: Option<u64>,
) -> &'static str {
    if host_available_bytes < SHARED_VM_HOST_MEMORY_EMERGENCY_BYTES {
        "emergency"
    } else if host_available_bytes < SHARED_VM_HOST_MEMORY_RESERVE_BYTES {
        "host_reclaim"
    } else if guest_available_bytes
        .map(|value| value < SHARED_VM_GUEST_MEMORY_GROW_THRESHOLD_BYTES)
        .unwrap_or(false)
    {
        "guest_protected"
    } else {
        "balanced"
    }
}

pub(super) fn shared_vm_memory_pressure_state_after(
    action: &SharedVmMemoryBalloonAction,
    reason: &str,
    host_available_bytes: u64,
    guest_available_bytes: Option<u64>,
) -> &'static str {
    match action {
        SharedVmMemoryBalloonAction::Reclaim {
            aggressive: true, ..
        }
        | SharedVmMemoryBalloonAction::EmergencyStop { .. } => "emergency",
        SharedVmMemoryBalloonAction::Reclaim { .. } => "host_reclaim",
        SharedVmMemoryBalloonAction::Grow { .. } => "guest_protected",
        SharedVmMemoryBalloonAction::NoAction => {
            if matches!(
                reason,
                "host_memory_emergency" | "host_memory_emergency_at_floor"
            ) {
                "emergency"
            } else if matches!(
                reason,
                "host_memory_below_reserve"
                    | "at_floor_while_host_below_reserve"
                    | "host_growth_budget_exhausted"
            ) {
                "host_reclaim"
            } else {
                shared_vm_memory_pressure_state_before(host_available_bytes, guest_available_bytes)
            }
        }
    }
}
