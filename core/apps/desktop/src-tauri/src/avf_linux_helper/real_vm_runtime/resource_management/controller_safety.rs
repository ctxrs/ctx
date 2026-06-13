use super::super::*;
use super::memory_policy::{
    align_down_to_mebibyte, resolve_shared_vm_memory_balloon_action,
    shared_vm_memory_controller_decision_reason, shared_vm_memory_controller_reason_codes,
    shared_vm_memory_pressure_state_after, SharedVmMemoryBalloonAction, MEBIBYTE_BYTES,
};

const SHARED_VM_CONTROLLER_SAFETY_PREFLIGHT_HOLD_BAND_BYTES: u64 = 64 * MEBIBYTE_BYTES;
const SHARED_VM_CONTROLLER_SAFETY_PREFLIGHT_SHRINK_STEP_BYTES: u64 =
    SHARED_VM_MEMORY_BALLOON_STEP_BYTES / 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super::super) enum SharedVmControllerSafetyReplayPhase {
    WarmIdle,
    Claim,
    Hold,
    Release,
    HostPressure,
}

impl TryFrom<&str> for SharedVmControllerSafetyReplayPhase {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self> {
        match value {
            "warm_idle" => Ok(Self::WarmIdle),
            "claim" => Ok(Self::Claim),
            "hold" => Ok(Self::Hold),
            "release" => Ok(Self::Release),
            "host_pressure" => Ok(Self::HostPressure),
            _ => bail!("unknown controller-safety replay phase `{value}`"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super::super) enum SharedVmControllerSafetyHostPressureState {
    Normal,
    Elevated,
    Emergency,
}

impl TryFrom<&str> for SharedVmControllerSafetyHostPressureState {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self> {
        match value {
            "normal" => Ok(Self::Normal),
            "elevated" => Ok(Self::Elevated),
            "emergency" => Ok(Self::Emergency),
            _ => bail!("unknown controller-safety host pressure state `{value}`"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super::super) enum SharedVmControllerSafetyPressureState {
    Balanced,
    HostReclaim,
    GuestProtected,
    Emergency,
}

impl SharedVmControllerSafetyPressureState {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::HostReclaim => "host_reclaim",
            Self::GuestProtected => "guest_protected",
            Self::Emergency => "emergency",
        }
    }
}

impl TryFrom<&str> for SharedVmControllerSafetyPressureState {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self> {
        match value {
            "balanced" => Ok(Self::Balanced),
            "host_reclaim" => Ok(Self::HostReclaim),
            "guest_protected" => Ok(Self::GuestProtected),
            "emergency" => Ok(Self::Emergency),
            _ => bail!("unknown controller-safety pressure state `{value}`"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in super::super::super) struct SharedVmControllerSafetyReplayState {
    pub target_bytes: u64,
    pub floor_bytes: u64,
    pub ceiling_bytes: u64,
    pub pressure_state: SharedVmControllerSafetyPressureState,
    pub cooldown_remaining_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super::super) struct SharedVmControllerSafetyReplayStep {
    pub step_index: u64,
    pub time_since_start_ms: u64,
    pub time_since_last_step_ms: u64,
    pub phase: SharedVmControllerSafetyReplayPhase,
    pub host_available_bytes: u64,
    pub host_pressure_state: SharedVmControllerSafetyHostPressureState,
    pub host_compressor_delta_bytes: u64,
    pub host_pageout_delta_bytes: u64,
    pub host_swap_delta_bytes: u64,
    pub guest_working_set_bytes: u64,
    pub guest_reclaimable_bytes: u64,
    pub guest_available_bytes: u64,
    pub guest_swap_bytes: u64,
    pub guest_under_pressure: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in super::super::super) struct SharedVmControllerSafetyReplayDecision {
    pub step_index: u64,
    pub action: &'static str,
    pub target_bytes_before: u64,
    pub target_bytes_after: u64,
    pub pressure_state_before: &'static str,
    pub pressure_state_after: &'static str,
    pub reason_codes: Vec<&'static str>,
    pub emergency_path: bool,
    pub invariants_passed: Vec<&'static str>,
}

impl SharedVmControllerSafetyReplayDecision {
    pub(super) fn to_canonical_json(&self) -> String {
        format!(
            concat!(
                "{{",
                "\"step_index\":{},",
                "\"action\":\"{}\",",
                "\"target_bytes_before\":{},",
                "\"target_bytes_after\":{},",
                "\"pressure_state_before\":\"{}\",",
                "\"pressure_state_after\":\"{}\",",
                "\"reason_codes\":[{}],",
                "\"emergency_path\":{},",
                "\"invariants_passed\":[{}]",
                "}}"
            ),
            self.step_index,
            self.action,
            self.target_bytes_before,
            self.target_bytes_after,
            self.pressure_state_before,
            self.pressure_state_after,
            shared_vm_controller_safety_canonical_json_strings(&self.reason_codes),
            self.emergency_path,
            shared_vm_controller_safety_canonical_json_strings(&self.invariants_passed),
        )
    }
}

pub(in super::super::super) fn replay_shared_vm_controller_safety_trace(
    initial_state: &SharedVmControllerSafetyReplayState,
    steps: &[SharedVmControllerSafetyReplayStep],
) -> Result<Vec<SharedVmControllerSafetyReplayDecision>> {
    let mut state = initial_state.clone();
    steps
        .iter()
        .map(|step| replay_shared_vm_controller_safety_step(&mut state, step))
        .collect()
}

pub(in super::super::super) fn shared_vm_controller_safety_trace_canonical_json(
    decisions: &[SharedVmControllerSafetyReplayDecision],
) -> String {
    format!(
        "[{}]",
        decisions
            .iter()
            .map(SharedVmControllerSafetyReplayDecision::to_canonical_json)
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn shared_vm_controller_safety_canonical_json_strings(values: &[&'static str]) -> String {
    values
        .iter()
        .map(|value| format!("\"{value}\""))
        .collect::<Vec<_>>()
        .join(",")
}

fn replay_shared_vm_controller_safety_step(
    state: &mut SharedVmControllerSafetyReplayState,
    step: &SharedVmControllerSafetyReplayStep,
) -> Result<SharedVmControllerSafetyReplayDecision> {
    let ceiling_bytes = align_down_to_mebibyte(state.ceiling_bytes.max(MEBIBYTE_BYTES));
    let floor_bytes =
        align_down_to_mebibyte(state.floor_bytes.max(MEBIBYTE_BYTES)).min(ceiling_bytes);
    let target_bytes_before =
        align_down_to_mebibyte(state.target_bytes).clamp(floor_bytes, ceiling_bytes);
    let pressure_state_before = state.pressure_state;
    state.cooldown_remaining_ms = state
        .cooldown_remaining_ms
        .saturating_sub(step.time_since_last_step_ms);

    let (action, target_bytes_after, pressure_state_after, reason_codes, emergency_path) = if state
        .cooldown_remaining_ms
        > 0
    {
        (
            "hold",
            target_bytes_before,
            pressure_state_before,
            vec!["cooldown_active"],
            false,
        )
    } else {
        match step.host_pressure_state {
            SharedVmControllerSafetyHostPressureState::Emergency => (
                "emergency_shrink",
                shared_vm_controller_safety_emergency_target_bytes(
                    target_bytes_before,
                    floor_bytes,
                    ceiling_bytes,
                ),
                SharedVmControllerSafetyPressureState::Emergency,
                shared_vm_memory_controller_reason_codes("host_memory_emergency").to_vec(),
                true,
            ),
            SharedVmControllerSafetyHostPressureState::Elevated if step.guest_under_pressure => (
                "hold",
                target_bytes_before,
                SharedVmControllerSafetyPressureState::GuestProtected,
                vec!["guest_pressure_blocks_shrink"],
                false,
            ),
            SharedVmControllerSafetyHostPressureState::Elevated => (
                if target_bytes_before > floor_bytes {
                    "shrink"
                } else {
                    "hold"
                },
                target_bytes_before
                    .saturating_sub(SHARED_VM_CONTROLLER_SAFETY_PREFLIGHT_SHRINK_STEP_BYTES)
                    .max(floor_bytes),
                SharedVmControllerSafetyPressureState::HostReclaim,
                vec!["host_pressure_reclaim"],
                false,
            ),
            SharedVmControllerSafetyHostPressureState::Normal => {
                let guest_available_bytes = Some(
                    step.guest_available_bytes
                        .saturating_add(SHARED_VM_CONTROLLER_SAFETY_PREFLIGHT_HOLD_BAND_BYTES),
                );
                let resolved_action = resolve_shared_vm_memory_balloon_action(
                    target_bytes_before,
                    ceiling_bytes,
                    floor_bytes,
                    true,
                    guest_available_bytes,
                    step.host_available_bytes,
                );
                let reason = shared_vm_memory_controller_decision_reason(
                    &resolved_action,
                    true,
                    target_bytes_before,
                    ceiling_bytes,
                    floor_bytes,
                    guest_available_bytes,
                    step.host_available_bytes,
                );
                let reason_codes = shared_vm_memory_controller_reason_codes(reason).to_vec();
                let pressure_state_after =
                    if matches!(resolved_action, SharedVmMemoryBalloonAction::NoAction)
                        && reason_codes.as_slice() == ["stable_band"]
                    {
                        pressure_state_before
                    } else {
                        SharedVmControllerSafetyPressureState::try_from(
                            shared_vm_memory_pressure_state_after(
                                &resolved_action,
                                reason,
                                step.host_available_bytes,
                                guest_available_bytes,
                            ),
                        )
                        .context("controller-safety replay pressure state")?
                    };
                let (action, target_bytes_after, emergency_path) =
                    shared_vm_controller_safety_normalize_balloon_action(
                        target_bytes_before,
                        &resolved_action,
                    );
                (
                    action,
                    target_bytes_after,
                    pressure_state_after,
                    reason_codes,
                    emergency_path,
                )
            }
        }
    };

    let invariants_passed = shared_vm_controller_safety_invariants_passed(
        step,
        action,
        target_bytes_before,
        target_bytes_after,
        pressure_state_after,
        &reason_codes,
        emergency_path,
        floor_bytes,
        ceiling_bytes,
    );
    state.target_bytes = target_bytes_after;
    state.floor_bytes = floor_bytes;
    state.ceiling_bytes = ceiling_bytes;
    state.pressure_state = pressure_state_after;

    Ok(SharedVmControllerSafetyReplayDecision {
        step_index: step.step_index,
        action,
        target_bytes_before,
        target_bytes_after,
        pressure_state_before: pressure_state_before.as_str(),
        pressure_state_after: pressure_state_after.as_str(),
        reason_codes,
        emergency_path,
        invariants_passed,
    })
}

fn shared_vm_controller_safety_emergency_target_bytes(
    target_bytes_before: u64,
    floor_bytes: u64,
    ceiling_bytes: u64,
) -> u64 {
    floor_bytes
        .saturating_add(SHARED_VM_MEMORY_BALLOON_STEP_BYTES)
        .min(ceiling_bytes)
        .min(target_bytes_before)
        .max(floor_bytes)
}

fn shared_vm_controller_safety_normalize_balloon_action(
    current_target_bytes: u64,
    action: &SharedVmMemoryBalloonAction,
) -> (&'static str, u64, bool) {
    match action {
        SharedVmMemoryBalloonAction::NoAction => ("hold", current_target_bytes, false),
        SharedVmMemoryBalloonAction::Grow {
            new_target_bytes, ..
        } => ("grow", *new_target_bytes, false),
        SharedVmMemoryBalloonAction::Reclaim {
            new_target_bytes,
            aggressive,
            ..
        } => {
            if *aggressive {
                ("emergency_shrink", *new_target_bytes, true)
            } else {
                ("shrink", *new_target_bytes, false)
            }
        }
        SharedVmMemoryBalloonAction::EmergencyStop {
            current_target_bytes,
            ..
        } => ("emergency_shrink", *current_target_bytes, true),
    }
}

fn shared_vm_controller_safety_invariants_passed(
    step: &SharedVmControllerSafetyReplayStep,
    action: &'static str,
    target_bytes_before: u64,
    target_bytes_after: u64,
    pressure_state_after: SharedVmControllerSafetyPressureState,
    reason_codes: &[&'static str],
    emergency_path: bool,
    floor_bytes: u64,
    ceiling_bytes: u64,
) -> Vec<&'static str> {
    let mut invariants = Vec::new();

    if target_bytes_before >= floor_bytes
        && target_bytes_before <= ceiling_bytes
        && target_bytes_after >= floor_bytes
        && target_bytes_after <= ceiling_bytes
    {
        invariants.push("target_within_bounds");
    }

    if step.guest_under_pressure
        && !matches!(
            step.host_pressure_state,
            SharedVmControllerSafetyHostPressureState::Emergency
        )
        && !matches!(action, "shrink" | "emergency_shrink")
        && !reason_codes.is_empty()
    {
        invariants.push("non_emergency_guest_pressure_blocks_shrink");
    }

    if matches!(
        step.host_pressure_state,
        SharedVmControllerSafetyHostPressureState::Normal
    ) && !step.guest_under_pressure
        && action == "hold"
        && target_bytes_after == target_bytes_before
        && reason_codes == ["stable_band"]
        && !emergency_path
    {
        invariants.push("stable_inputs_no_oscillation");
    }

    if matches!(
        step.host_pressure_state,
        SharedVmControllerSafetyHostPressureState::Emergency
    ) && action == "emergency_shrink"
        && pressure_state_after == SharedVmControllerSafetyPressureState::Emergency
        && emergency_path
        && reason_codes.contains(&"host_emergency")
    {
        invariants.push("emergency_path_explicit");
    }

    invariants
}
