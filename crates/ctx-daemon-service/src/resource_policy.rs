use std::path::Path;

#[cfg(target_os = "linux")]
use std::fs;

use ctx_semantic_model::{semantic_model_load_resource_facts, semantic_query_service_supported};
use serde_json::{json, Value};

use crate::compact_json;

const SEMANTIC_INDEX_MIN_AVAILABLE_MEMORY_BYTES: u64 = 1024 * 1024 * 1024;
const SEMANTIC_BACKGROUND_MIN_AVAILABLE_DISK_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SemanticBackgroundOperation {
    ModelLoad,
    IndexBatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", test)),
    allow(dead_code)
)]
pub(super) enum SemanticResourceDeferralReason {
    MemoryPressure,
    DiskPressure,
    BatteryPower,
    EnergySaver,
    ThermalPressure,
}

impl SemanticResourceDeferralReason {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::MemoryPressure => "memory_pressure",
            Self::DiskPressure => "disk_pressure",
            Self::BatteryPower => "battery_power",
            Self::EnergySaver => "energy_saver",
            Self::ThermalPressure => "thermal_pressure",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SemanticResourceDeferred {
    reason: SemanticResourceDeferralReason,
    available_memory_bytes: Option<u64>,
    required_available_memory_bytes: Option<u64>,
    available_disk_bytes: Option<u64>,
    required_available_disk_bytes: Option<u64>,
}

impl SemanticResourceDeferred {
    #[cfg(test)]
    pub(super) const fn disk_pressure_for_test() -> Self {
        Self {
            reason: SemanticResourceDeferralReason::DiskPressure,
            available_memory_bytes: None,
            required_available_memory_bytes: None,
            available_disk_bytes: Some(0),
            required_available_disk_bytes: Some(SEMANTIC_BACKGROUND_MIN_AVAILABLE_DISK_BYTES),
        }
    }

    pub(super) fn reason(self) -> SemanticResourceDeferralReason {
        self.reason
    }

    pub(super) fn to_json(self) -> Value {
        compact_json(json!({
            "reason": self.reason.as_str(),
            "available_memory_bytes": self.available_memory_bytes,
            "required_available_memory_bytes": self.required_available_memory_bytes,
            "available_disk_bytes": self.available_disk_bytes,
            "required_available_disk_bytes": self.required_available_disk_bytes,
        }))
    }
}

pub(super) fn semantic_background_resource_deferred(
    storage_path: &Path,
    operation: SemanticBackgroundOperation,
) -> Option<SemanticResourceDeferred> {
    let model_facts = semantic_model_load_resource_facts();
    semantic_background_resource_deferred_for(
        model_facts.available_memory_bytes(),
        model_facts.required_available_memory_bytes(),
        semantic_available_space(storage_path),
        semantic_energy_deferral_reason(),
        operation,
    )
}

/// External inference uses no local model memory or energy budget, but index
/// publication still requires the same local disk safety margin.
pub(super) fn semantic_external_background_resource_deferred(
    storage_path: &Path,
) -> Option<SemanticResourceDeferred> {
    let available_disk_bytes = semantic_available_space_unconditionally(storage_path);
    available_disk_bytes
        .is_some_and(|available| available < SEMANTIC_BACKGROUND_MIN_AVAILABLE_DISK_BYTES)
        .then_some(SemanticResourceDeferred {
            reason: SemanticResourceDeferralReason::DiskPressure,
            available_memory_bytes: None,
            required_available_memory_bytes: None,
            available_disk_bytes,
            required_available_disk_bytes: Some(SEMANTIC_BACKGROUND_MIN_AVAILABLE_DISK_BYTES),
        })
}

fn semantic_background_resource_deferred_for(
    available_memory_bytes: Option<u64>,
    model_load_required_memory_bytes: u64,
    available_disk_bytes: Option<u64>,
    energy_reason: Option<SemanticResourceDeferralReason>,
    operation: SemanticBackgroundOperation,
) -> Option<SemanticResourceDeferred> {
    let required_memory = match operation {
        SemanticBackgroundOperation::ModelLoad => model_load_required_memory_bytes,
        SemanticBackgroundOperation::IndexBatch => SEMANTIC_INDEX_MIN_AVAILABLE_MEMORY_BYTES,
    };
    let deferred = |reason, required_memory, required_disk| SemanticResourceDeferred {
        reason,
        available_memory_bytes,
        required_available_memory_bytes: required_memory,
        available_disk_bytes,
        required_available_disk_bytes: required_disk,
    };
    if available_memory_bytes.is_some_and(|available| available < required_memory) {
        return Some(deferred(
            SemanticResourceDeferralReason::MemoryPressure,
            Some(required_memory),
            None,
        ));
    }
    if available_disk_bytes
        .is_some_and(|available| available < SEMANTIC_BACKGROUND_MIN_AVAILABLE_DISK_BYTES)
    {
        return Some(deferred(
            SemanticResourceDeferralReason::DiskPressure,
            None,
            Some(SEMANTIC_BACKGROUND_MIN_AVAILABLE_DISK_BYTES),
        ));
    }
    energy_reason.map(|reason| deferred(reason, None, None))
}

pub(super) fn semantic_resource_deferral_releases_runtime(
    reason: SemanticResourceDeferralReason,
) -> bool {
    reason == SemanticResourceDeferralReason::MemoryPressure
}

fn semantic_available_space(path: &Path) -> Option<u64> {
    if !semantic_query_service_supported() {
        return None;
    }
    semantic_available_space_unconditionally(path)
}

fn semantic_available_space_unconditionally(path: &Path) -> Option<u64> {
    let mut candidate = path;
    while !candidate.exists() {
        candidate = candidate.parent()?;
    }
    fs2::available_space(candidate).ok()
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn semantic_energy_deferral_reason_for(
    external_power: Option<bool>,
    energy_saver: Option<bool>,
    serious_thermal_pressure: Option<bool>,
) -> Option<SemanticResourceDeferralReason> {
    if external_power == Some(false) {
        Some(SemanticResourceDeferralReason::BatteryPower)
    } else if energy_saver == Some(true) {
        Some(SemanticResourceDeferralReason::EnergySaver)
    } else if serious_thermal_pressure == Some(true) {
        Some(SemanticResourceDeferralReason::ThermalPressure)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn semantic_energy_deferral_reason() -> Option<SemanticResourceDeferralReason> {
    let root = Path::new("/sys/class/power_supply");
    let mut saw_system_battery = false;
    let mut external_online = false;
    for entry in fs::read_dir(root).ok()? {
        let path = entry.ok()?.path();
        match fs::read_to_string(path.join("type")).ok()?.trim() {
            "Battery" => {
                let scope = fs::read_to_string(path.join("scope")).ok();
                if scope
                    .as_deref()
                    .is_none_or(|scope| scope.trim() != "Device")
                {
                    saw_system_battery = true;
                }
            }
            "Mains" | "UPS" | "USB" | "USB_C" | "USB_PD" | "Wireless" => {
                external_online |= fs::read_to_string(path.join("online"))
                    .ok()
                    .is_some_and(|value| value.trim() == "1");
            }
            _ => {}
        }
    }
    semantic_energy_deferral_reason_for(saw_system_battery.then_some(external_online), None, None)
}

#[cfg(target_os = "macos")]
fn semantic_energy_deferral_reason() -> Option<SemanticResourceDeferralReason> {
    let process = objc2_foundation::NSProcessInfo::processInfo();
    semantic_energy_deferral_reason_for(
        None,
        Some(process.isLowPowerModeEnabled()),
        Some(process.thermalState().0 >= 2),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn semantic_energy_deferral_reason() -> Option<SemanticResourceDeferralReason> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_ordering_and_runtime_release_are_frozen() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let model = semantic_background_resource_deferred_for(
            Some(2 * GIB - 1),
            2 * GIB,
            Some(8 * GIB),
            Some(SemanticResourceDeferralReason::BatteryPower),
            SemanticBackgroundOperation::ModelLoad,
        )
        .unwrap();
        assert_eq!(
            model.reason(),
            SemanticResourceDeferralReason::MemoryPressure
        );
        assert!(semantic_resource_deferral_releases_runtime(model.reason()));

        let index = semantic_background_resource_deferred_for(
            Some(8 * GIB),
            2 * GIB,
            Some(GIB - 1),
            Some(SemanticResourceDeferralReason::BatteryPower),
            SemanticBackgroundOperation::IndexBatch,
        )
        .unwrap();
        assert_eq!(index.reason(), SemanticResourceDeferralReason::DiskPressure);
        assert!(!semantic_resource_deferral_releases_runtime(index.reason()));
        assert_eq!(
            index.required_available_disk_bytes,
            Some(SEMANTIC_BACKGROUND_MIN_AVAILABLE_DISK_BYTES)
        );
        assert!(semantic_background_resource_deferred_for(
            Some(8 * GIB),
            2 * GIB,
            Some(GIB),
            None,
            SemanticBackgroundOperation::IndexBatch,
        )
        .is_none());
    }

    #[test]
    fn energy_and_thermal_signals_defer_only_when_known_pressured() {
        assert_eq!(
            semantic_energy_deferral_reason_for(Some(false), Some(false), Some(false)),
            Some(SemanticResourceDeferralReason::BatteryPower)
        );
        assert_eq!(
            semantic_energy_deferral_reason_for(Some(true), Some(true), Some(false)),
            Some(SemanticResourceDeferralReason::EnergySaver)
        );
        assert_eq!(
            semantic_energy_deferral_reason_for(Some(true), Some(false), Some(true)),
            Some(SemanticResourceDeferralReason::ThermalPressure)
        );
        assert_eq!(semantic_energy_deferral_reason_for(None, None, None), None);
    }
}
