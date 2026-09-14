use std::path::{Path, PathBuf};

use ctx_daemon_runtime::{
    daemon_lock_path, daemon_owner_binary_identity_matches, daemon_root_path, daemon_status_path,
    pid_lock_file_is_orphaned, pid_lock_file_reports_running, process_state, read_daemon_status,
    read_pid_lock_file, read_pid_lock_json,
};
use ctx_daemon_service::daemon_wakeup_report;
use serde_json::{json, Value};

use crate::{compact_json, supervisor, DaemonApplicationHost, DaemonConfigSnapshot, DaemonMode};

mod config_reload;
mod core_refresh_job;

use config_reload::daemon_config_reload_report;
pub use config_reload::DaemonConfigReloadContext;

pub struct DaemonStatusPreparation<'a> {
    host: &'a dyn DaemonApplicationHost,
    data_root: &'a Path,
    enabled: bool,
    mode: DaemonMode,
    status_value: Option<Value>,
    status: String,
    lock_path: PathBuf,
    status_path: PathBuf,
    lock_value: Option<Value>,
    lock_pid: Option<u32>,
    owner_identity_matches: bool,
    owner_identity_mismatch: bool,
    running: bool,
    stale_lock_overrides_lifecycle: bool,
    stale_running_status: bool,
    pid: Option<u32>,
    config_reload: Value,
    semantic_runtime_active: bool,
    start_mode: Option<String>,
    trigger_command: Option<String>,
    trigger_provenance: Option<String>,
    core_refresh_job: Value,
}

#[derive(Debug, Clone, Copy)]
pub struct DaemonSemanticStatusContext<'a> {
    pub daemon_mode: DaemonMode,
    pub daemon_running: bool,
    pub semantic_runtime_active: bool,
    pub config_reload: DaemonConfigReloadContext<'a>,
}

#[derive(Debug)]
pub struct DaemonStatusSnapshot {
    value: Value,
}

impl DaemonStatusSnapshot {
    pub fn into_json(self) -> Value {
        self.value
    }
}

pub(super) fn prepare_daemon_status<'a>(
    host: &'a dyn DaemonApplicationHost,
    data_root: &'a Path,
    disabled_overrides_lifecycle: bool,
    current_config: Option<&DaemonConfigSnapshot>,
    default_daemon_enabled: bool,
) -> DaemonStatusPreparation<'a> {
    let status_value = read_daemon_status(data_root);
    let enabled = current_config
        .map(|config| config.enabled)
        .unwrap_or(default_daemon_enabled);
    let mode = current_config
        .map(|config| config.mode)
        .or_else(|| {
            status_value
                .as_ref()
                .and_then(|status| status.get("config_reload"))
                .and_then(|reload| reload.get("applied"))
                .and_then(|applied| applied.get("daemon_mode"))
                .and_then(Value::as_str)
                .and_then(DaemonMode::parse)
        })
        .unwrap_or(DaemonMode::Full);
    let lock_path = daemon_lock_path(data_root);
    let status_path = daemon_status_path(data_root);
    let lock_value = read_pid_lock_json(&lock_path);
    let lock_pid = read_pid_lock_file(&lock_path);
    let mut status = status_value
        .as_ref()
        .and_then(|value| json_string(value, "status"))
        .unwrap_or_else(|| "unknown".to_owned());
    let lock_state = lock_pid.map(process_state);
    let lock_reports_running =
        pid_lock_file_reports_running(&lock_path, lock_state, status.as_str());
    let owner_identity_matches = lock_reports_running
        && lock_value.as_ref().is_some_and(|identity| {
            identity
                .get("binary")
                .and_then(Value::as_str)
                .map(Path::new)
                .and_then(|executable| {
                    daemon_owner_binary_identity_matches(identity, executable).ok()
                })
                .unwrap_or(false)
        });
    let owner_identity_mismatch = lock_reports_running && !owner_identity_matches;
    let running = lock_reports_running && owner_identity_matches;
    let stale_lock = lock_path.exists() && pid_lock_file_is_orphaned(&lock_path);
    let stale_lock_overrides_lifecycle = (stale_lock || owner_identity_mismatch)
        && !["completed", "stopped", "failed"].contains(&status.as_str());
    let stale_running_status = !running && status == "running";
    if running {
        status = "running".to_owned();
    } else if stale_lock_overrides_lifecycle || stale_running_status {
        status = "stale_lock".to_owned();
    } else if !enabled && (disabled_overrides_lifecycle || status == "unknown") {
        status = "disabled".to_owned();
    }
    let pid = if running {
        lock_pid
    } else {
        status_value
            .as_ref()
            .and_then(|value| json_u32(value, "pid"))
    };
    let config_reload = daemon_config_reload_report(status_value.as_ref(), running, current_config);
    let semantic_runtime_active = running
        && status_value
            .as_ref()
            .and_then(|value| value.get("semantic_runtime_active"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let start_mode = status_value
        .as_ref()
        .and_then(|value| json_string(value, "start_mode"));
    let trigger_command = status_value
        .as_ref()
        .and_then(|value| json_string(value, "trigger_command"));
    let trigger_provenance = if start_mode.as_deref() == Some("auto") {
        Some("autostart".to_owned())
    } else {
        trigger_command
            .clone()
            .or_else(|| Some("manual".to_owned()))
    };
    let core_refresh_job = core_refresh_job::daemon_core_refresh_job_report(
        data_root,
        disabled_overrides_lifecycle,
        current_config
            .map(|config| config.enabled)
            .unwrap_or(default_daemon_enabled),
    );
    DaemonStatusPreparation {
        host,
        data_root,
        enabled,
        mode,
        status_value,
        status,
        lock_path,
        status_path,
        lock_value,
        lock_pid,
        owner_identity_matches,
        owner_identity_mismatch,
        running,
        stale_lock_overrides_lifecycle,
        stale_running_status,
        pid,
        config_reload,
        semantic_runtime_active,
        start_mode,
        trigger_command,
        trigger_provenance,
        core_refresh_job,
    }
}

impl DaemonStatusPreparation<'_> {
    pub fn semantic_context(&self) -> DaemonSemanticStatusContext<'_> {
        let reload = &self.config_reload;
        DaemonSemanticStatusContext {
            daemon_mode: self.mode,
            daemon_running: self.running,
            semantic_runtime_active: self.semantic_runtime_active,
            config_reload: DaemonConfigReloadContext {
                status: reload
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                out_of_sync: reload
                    .get("out_of_sync")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                requested_daemon_enabled: reload
                    .pointer("/requested/daemon_enabled")
                    .and_then(Value::as_bool),
                requested_semantic_enabled: reload
                    .pointer("/requested/semantic_enabled")
                    .and_then(Value::as_bool),
                requested_semantic_executor: reload
                    .pointer("/requested/semantic_executor")
                    .and_then(Value::as_str),
                requested_semantic_contract_fingerprint: reload
                    .pointer("/requested/semantic_contract_fingerprint")
                    .and_then(Value::as_str),
                requested_semantic_builtin_throttling_configured: reload
                    .pointer("/requested/semantic_builtin_throttling_configured")
                    .and_then(Value::as_bool),
                requested_semantic_builtin_throttling_effective: reload
                    .pointer("/requested/semantic_builtin_throttling_effective")
                    .and_then(Value::as_bool),
                applied_daemon_enabled: reload
                    .pointer("/applied/daemon_enabled")
                    .and_then(Value::as_bool),
                applied_semantic_enabled: reload
                    .pointer("/applied/semantic_enabled")
                    .and_then(Value::as_bool),
                applied_semantic_executor: reload
                    .pointer("/applied/semantic_executor")
                    .and_then(Value::as_str),
                applied_semantic_contract_fingerprint: reload
                    .pointer("/applied/semantic_contract_fingerprint")
                    .and_then(Value::as_str),
                applied_semantic_builtin_throttling_configured: reload
                    .pointer("/applied/semantic_builtin_throttling_configured")
                    .and_then(Value::as_bool),
                applied_semantic_builtin_throttling_effective: reload
                    .pointer("/applied/semantic_builtin_throttling_effective")
                    .and_then(Value::as_bool),
                last_error: reload.get("last_error").and_then(Value::as_str),
            },
        }
    }

    pub fn finish(self) -> DaemonStatusSnapshot {
        let lock_identity = compact_json(json!({
            "path": self.lock_path,
            "active": self.running,
            "owner_id": self
                .lock_value
                .as_ref()
                .and_then(|value| json_string(value, "owner_id")),
            "pid": self.lock_pid,
            "binary": self
                .lock_value
                .as_ref()
                .and_then(|value| json_string(value, "binary")),
            "binary_sha256": self
                .lock_value
                .as_ref()
                .and_then(|value| json_string(value, "binary_sha256")),
            "owner_image_matches": self.owner_identity_matches,
            "protocol": self
                .lock_value
                .as_ref()
                .and_then(|value| json_string(value, "lock_protocol")),
        }));
        let endpoint = daemon_core_refresh_endpoint_report(self.host, self.data_root);
        let supervisor = supervisor::daemon_supervisor_report(self.host, self.data_root);
        let wakeup = daemon_wakeup_report(self.data_root);
        // Diagnostic only: an old scheduler heartbeat does not revoke live
        // lock ownership or authorize killing a slow worker.
        let heartbeat_age_ms = self.status_value.as_ref().and_then(|value| {
            if !self.running || json_u32(value, "pid") != self.pid {
                return None;
            }
            let heartbeat = json_i64(value, "heartbeat_at_ms").filter(|time| *time > 0)?;
            let age = ctx_history_core::utc_now()
                .timestamp_millis()
                .checked_sub(heartbeat)?;
            (age >= 0).then_some(age)
        });
        DaemonStatusSnapshot {
            value: compact_json(json!({
                "status": self.status,
                "enabled": self.enabled,
                "mode": self.mode.as_str(),
                "running": self.running,
                "recoverable": self.stale_lock_overrides_lifecycle || self.stale_running_status,
                "reason": if self.owner_identity_mismatch {
                    Some("daemon_owner_identity_mismatch".to_owned())
                } else if self.stale_lock_overrides_lifecycle {
                    Some("daemon_lock_stale".to_owned())
                } else if self.stale_running_status {
                    Some("daemon_status_stale".to_owned())
                } else {
                    self.status_value
                        .as_ref()
                        .and_then(|value| json_string(value, "reason"))
                },
                "pid": self.pid,
                "live_pid": self.running.then_some(self.pid).flatten(),
                "started_at_ms": self.status_value.as_ref().and_then(|value| json_i64(value, "started_at_ms")),
                "heartbeat_at_ms": self.status_value.as_ref().and_then(|value| json_i64(value, "heartbeat_at_ms")),
                "heartbeat_age_ms": heartbeat_age_ms,
                "heartbeat_stale": heartbeat_age_ms.map(|age| age > crate::lifecycle::DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS),
                "finished_at_ms": self.status_value.as_ref().and_then(|value| json_i64(value, "finished_at_ms")),
                "start_mode": self.start_mode,
                "trigger_command": self.trigger_command,
                "trigger_provenance": self.trigger_provenance,
                "last_error": self.status_value.as_ref().and_then(|value| json_string(value, "last_error")),
                "semantic_runtime_active": self.semantic_runtime_active,
                "config_reload": self.config_reload,
                "lock_path": self.lock_path,
                "lock_identity": lock_identity,
                "core_refresh_endpoint": endpoint,
                "supervisor": supervisor,
                "wakeup": wakeup,
                "status_path": self.status_path,
                "jobs": {
                    "core_refresh": self.core_refresh_job,
                },
            })),
        }
    }
}

fn daemon_core_refresh_endpoint_report(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
) -> Value {
    let identity_path = daemon_root_path(data_root).join("source-refresh-endpoint.json");
    let identity = host.observe_source_refresh_endpoint(&identity_path);
    compact_json(json!({
        "identity_path": identity_path,
        "available": identity.available,
        "transport": identity.transport,
        "owner_pid": identity.owner_pid,
        "address": identity.address,
    }))
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn json_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn json_u32(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

#[cfg(test)]
mod tests;
