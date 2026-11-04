use std::{path::Path, time::Duration};

use crate::AttributionMaintenanceStatus;
use ctx_daemon_runtime::{
    daemon_lock_is_owned_by, daemon_lock_path, pid_from_lock_json, read_pid_lock_json,
};

/// Observe admitted attribution work through the responsive owner, never infer
/// it from process existence or the duration of a materialization pass.
pub fn attribution_maintenance_status(data_root: &Path) -> Option<AttributionMaintenanceStatus> {
    let lock_path = daemon_lock_path(data_root);
    let owner = read_pid_lock_json(&lock_path)?;
    let pid = pid_from_lock_json(&owner)?;
    // Only persistent owners promise automatic attribution maintenance. IPC
    // polling extends finite workers' idle grace and can prevent their exit.
    if owner["persistent"] != true || !daemon_lock_is_owned_by(data_root, pid) {
        return None;
    }
    let response = ctx_daemon_service::daemon_source_refresh_request(
        data_root,
        serde_json::json!({"schema_version": 1, "op": "lifecycle_ping"}),
        Duration::from_millis(500),
        16 * 1024,
    )
    .ok()??;
    if response["schema_version"] != 1
        || response["ok"] != true
        || response["owner"] != "daemon"
        || response["service"] != "lifecycle"
        || response["pid"] != pid
        || read_pid_lock_json(&lock_path).as_ref() != Some(&owner)
        || !matches!(response["readiness"].as_str(), Some("starting" | "ready"))
    {
        return None;
    }
    serde_json::from_value(response.get("attribution_maintenance")?.clone()).ok()
}
