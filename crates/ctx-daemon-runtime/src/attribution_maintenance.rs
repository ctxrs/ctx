//! Observation of the final binary's single attribution worker. This is
//! process-local: a dead or unresponsive owner cannot promise completion.
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AttributionMaintenanceStatus {
    #[default]
    Idle,
    Running,
    Completed,
    Failed {
        error: String,
    },
}

static STATUS: Mutex<AttributionMaintenanceStatus> = Mutex::new(AttributionMaintenanceStatus::Idle);

pub fn record_attribution_maintenance(status: AttributionMaintenanceStatus) {
    *STATUS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = status;
}

pub fn current_attribution_maintenance_status() -> AttributionMaintenanceStatus {
    STATUS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}
