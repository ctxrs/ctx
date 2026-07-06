use std::{fs, io::Write, path::Path, time::Duration};

use anyhow::{Context, Result};
use ctx_history_core::utc_now;
use serde_json::{json, Value};

use super::{types::UpgradePlan, util::now_unix_s};

pub(super) const STATE_FILE: &str = "upgrade-state.json";
const LOG_FILE: &str = "logs/upgrade.log";

pub(super) fn write_state_checked(
    data_root: &Path,
    plan: &UpgradePlan,
    status: &str,
) -> Result<()> {
    let body = json!({
        "schema_version": 1,
        "status": status,
        "checked_at": utc_now(),
        "last_checked_unix_s": now_unix_s(),
        "current_version": plan.current_version,
        "latest_version": plan.latest_version,
        "update_available": plan.update_available,
        "channel": plan.channel,
        "platform": plan.platform,
        "metadata_url": plan.metadata_url,
        "artifact_url": plan.artifact_url,
        "install_path": plan.install_path,
        "managed": plan.managed,
    });
    atomic_write_json(&data_root.join(STATE_FILE), &body)
}

pub(super) fn write_state_error(data_root: &Path, error: &str) -> Result<()> {
    let body = json!({
        "schema_version": 1,
        "status": "error",
        "checked_at": utc_now(),
        "last_checked_unix_s": now_unix_s(),
        "error": error,
    });
    atomic_write_json(&data_root.join(STATE_FILE), &body)
}

pub(super) fn should_check_now(data_root: &Path, interval: Duration) -> bool {
    if interval.is_zero() {
        return true;
    }
    let Some(value) = read_json_file(&data_root.join(STATE_FILE)) else {
        return true;
    };
    let Some(last) = value.get("last_checked_unix_s").and_then(Value::as_u64) else {
        return true;
    };
    now_unix_s().saturating_sub(last) >= interval.as_secs()
}

pub(super) fn read_json_file(path: &Path) -> Option<Value> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

pub(super) fn atomic_write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let body = serde_json::to_vec_pretty(value)?;
    fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} to {}", tmp.display(), path.display()))
}

pub(super) fn append_upgrade_log(data_root: &Path, message: &str) {
    let path = data_root.join(LOG_FILE);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{} {}", utc_now().to_rfc3339(), message);
    }
}
