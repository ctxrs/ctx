use ctx_settings_model::{ContainerExecutionSettings, ContainerMachineMemoryProfile};
use sysinfo::System;

const DEFAULT_PRESET_HOST_MEMORY_MB: u32 = 32 * 1024;
const SANDBOX_VM_MEMORY_PRESET_FLOOR_MB: u32 = 4096;
const SANDBOX_VM_MEMORY_ECONOMY_CAP_MB: u32 = 8192;
const SANDBOX_VM_MEMORY_BALANCED_CAP_MB: u32 = 16 * 1024;
const SANDBOX_VM_MEMORY_PERFORMANCE_CAP_MB: u32 = 32 * 1024;
const MI_B: u64 = 1024 * 1024;

fn detected_host_memory_mb() -> Option<u32> {
    if let Ok(raw) = std::env::var("CTX_TEST_HOST_MEMORY_MB") {
        if let Ok(value) = raw.parse::<u32>() {
            if value > 0 {
                return Some(value);
            }
        }
    }

    let mut system = System::new();
    system.refresh_memory();
    let total_bytes = system.total_memory();
    let total_mb = total_bytes / MI_B;
    if total_mb == 0 {
        return None;
    }
    u32::try_from(total_mb).ok()
}

fn preset_memory_mb(total_memory_mb: u32, numerator: u32, denominator: u32, cap_mb: u32) -> u32 {
    total_memory_mb
        .saturating_mul(numerator)
        .checked_div(denominator)
        .unwrap_or(SANDBOX_VM_MEMORY_PRESET_FLOOR_MB)
        .clamp(SANDBOX_VM_MEMORY_PRESET_FLOOR_MB, cap_mb)
}

pub fn container_machine_memory_mb_for_host_memory(
    settings: &ContainerExecutionSettings,
    host_memory_mb: u32,
) -> u32 {
    match settings.machine.memory_profile {
        ContainerMachineMemoryProfile::Economy => {
            preset_memory_mb(host_memory_mb, 1, 8, SANDBOX_VM_MEMORY_ECONOMY_CAP_MB)
        }
        ContainerMachineMemoryProfile::Balanced => {
            preset_memory_mb(host_memory_mb, 1, 4, SANDBOX_VM_MEMORY_BALANCED_CAP_MB)
        }
        ContainerMachineMemoryProfile::Performance => {
            preset_memory_mb(host_memory_mb, 1, 2, SANDBOX_VM_MEMORY_PERFORMANCE_CAP_MB)
        }
        ContainerMachineMemoryProfile::Custom => settings
            .machine
            .custom_memory_mb
            .unwrap_or(preset_memory_mb(
                host_memory_mb,
                1,
                4,
                SANDBOX_VM_MEMORY_BALANCED_CAP_MB,
            ))
            .max(1024),
    }
}

pub fn container_machine_memory_mb(settings: &ContainerExecutionSettings) -> u32 {
    let host_memory_mb = detected_host_memory_mb().unwrap_or(DEFAULT_PRESET_HOST_MEMORY_MB);
    container_machine_memory_mb_for_host_memory(settings, host_memory_mb)
}
