use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde_json::{json, Value};

use crate::compact_json;
use ctx_daemon_runtime::{create_private_dir_all, daemon_root_path, write_private_json_file};

const SUPERVISOR_RECEIPT_FILE: &str = "supervisor.json";

pub(super) struct SupervisorReceipt {
    pub(super) kind: String,
    pub(super) status: &'static str,
    pub(super) autostart_supported: bool,
    pub(super) restart_supported: bool,
    pub(super) registration_verified: bool,
    pub(super) live_owner_verified: bool,
    pub(super) owner_pid: Option<u32>,
    pub(super) artifact_path: Option<PathBuf>,
    pub(super) executable_path: Option<PathBuf>,
    pub(super) limitation: Option<String>,
    pub(super) last_error: Option<String>,
    pub(super) recovery_reason: Option<String>,
}

pub(super) fn write_supervisor_receipt(
    data_root: &Path,
    receipt: &SupervisorReceipt,
) -> Result<()> {
    let environment_snapshot = read_supervisor_receipt(data_root)
        .and_then(|report| report.get("environment_snapshot").cloned());
    write_supervisor_receipt_with_environment_snapshot(data_root, receipt, environment_snapshot)
}

pub(super) fn write_supervisor_receipt_with_environment_snapshot(
    data_root: &Path,
    receipt: &SupervisorReceipt,
    environment_snapshot: Option<Value>,
) -> Result<()> {
    let root = daemon_root_path(data_root);
    create_private_dir_all(&root)?;
    write_private_json_file(
        &root.join(SUPERVISOR_RECEIPT_FILE),
        &compact_json(json!({
            "schema_version": 1,
            "kind": receipt.kind,
            "status": receipt.status,
            "autostart_supported": receipt.autostart_supported,
            "restart_supported": receipt.restart_supported,
            "registration_verified": receipt.registration_verified,
            "live_owner_verified": receipt.live_owner_verified,
            "owner_pid": receipt.owner_pid,
            "artifact_path": receipt.artifact_path,
            "executable_path": receipt.executable_path,
            "environment_snapshot": environment_snapshot.unwrap_or(Value::Null),
            "limitation": receipt.limitation,
            "last_error": receipt.last_error,
            "recovery_reason": receipt.recovery_reason,
        })),
    )
}

pub(super) fn write_installed_receipt(
    data_root: &Path,
    executable: &Path,
    artifact_path: Option<PathBuf>,
    owner_pid: u32,
    environment_snapshot: Option<Value>,
) -> Result<()> {
    write_installed_receipt_with_recovery_reason(
        data_root,
        executable,
        artifact_path,
        owner_pid,
        environment_snapshot,
        None,
    )
}

pub(super) fn write_installed_receipt_with_recovery_reason(
    data_root: &Path,
    executable: &Path,
    artifact_path: Option<PathBuf>,
    owner_pid: u32,
    environment_snapshot: Option<Value>,
    recovery_reason: Option<String>,
) -> Result<()> {
    let recovery_reason = recovery_reason.or_else(|| persisted_recovery_reason(data_root));
    let receipt = SupervisorReceipt {
        kind: native_supervisor_kind().to_owned(),
        status: "installed",
        autostart_supported: true,
        restart_supported: true,
        registration_verified: true,
        live_owner_verified: true,
        owner_pid: Some(owner_pid),
        artifact_path,
        executable_path: Some(executable.to_path_buf()),
        limitation: None,
        last_error: None,
        recovery_reason,
    };
    match environment_snapshot {
        Some(environment_snapshot) => write_supervisor_receipt_with_environment_snapshot(
            data_root,
            &receipt,
            Some(environment_snapshot),
        ),
        None => write_supervisor_receipt(data_root, &receipt),
    }
}

fn persisted_recovery_reason(data_root: &Path) -> Option<String> {
    read_supervisor_receipt(data_root)
        .and_then(|report| {
            report
                .get("recovery_reason")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .filter(|reason| !reason.is_empty())
}

pub(super) fn stored_supervisor_report(data_root: &Path) -> Value {
    let path = daemon_root_path(data_root).join(SUPERVISOR_RECEIPT_FILE);
    read_supervisor_receipt(data_root).unwrap_or_else(|| {
        compact_json(json!({
            "schema_version": 1,
            "kind": "unconfigured",
            "status": "unknown",
            "autostart_supported": false,
            "restart_supported": false,
            "environment_snapshot": Value::Null,
            "receipt_path": path,
        }))
    })
}

pub(super) fn persisted_supervisor_loop_interval_seconds(data_root: &Path) -> Option<u64> {
    read_supervisor_receipt(data_root)
        .and_then(|report| {
            report
                .pointer("/environment_snapshot/loop_interval_seconds")
                .and_then(Value::as_u64)
        })
        .filter(|value| (1..=3_600).contains(value))
}

fn read_supervisor_receipt(data_root: &Path) -> Option<Value> {
    fs::read_to_string(daemon_root_path(data_root).join(SUPERVISOR_RECEIPT_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

#[cfg(target_os = "linux")]
pub(super) fn native_supervisor_kind() -> &'static str {
    "systemd_user"
}

#[cfg(target_os = "macos")]
pub(super) fn native_supervisor_kind() -> &'static str {
    "launch_agent"
}

#[cfg(windows)]
pub(super) fn native_supervisor_kind() -> &'static str {
    "windows_task_scheduler"
}

#[cfg(target_os = "freebsd")]
pub(super) fn native_supervisor_kind() -> &'static str {
    "freebsd_user_supervisor_unavailable"
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    windows
)))]
pub(super) fn native_supervisor_kind() -> &'static str {
    "unsupported"
}

pub(super) fn native_supervisor_limitation() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "ctx launched a persistent detached daemon, but native automatic restart after failure, login, or reboot is unavailable because the systemd user manager is not operational"
    }
    #[cfg(target_os = "macos")]
    {
        "ctx launched a persistent detached daemon, but native automatic restart after failure, login, or reboot is unavailable because the launchd GUI user domain is not operational"
    }
    #[cfg(windows)]
    {
        "ctx launched a persistent detached daemon, but native automatic restart after failure, login, or reboot is unavailable because current-user Task Scheduler is not operational"
    }
    #[cfg(target_os = "freebsd")]
    {
        super::freebsd_supervisor_authority_blocker()
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        windows
    )))]
    {
        "ctx launched a persistent detached daemon, but native automatic restart after failure, login, or reboot is unavailable because this platform has no maintained native per-user supervisor integration"
    }
}
