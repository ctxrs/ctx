use std::{
    fs,
    path::{Path, PathBuf},
};
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use std::{
    io::Write as _,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::Result;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use anyhow::{anyhow, Context};
use serde_json::{json, Value};

use crate::compact_json;

use super::super::{
    health_search::create_private_dir_all,
    paths_status::{daemon_root_path, write_private_json_file},
};

const SUPERVISOR_RECEIPT_FILE: &str = "supervisor.json";
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
static SUPERVISOR_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub(super) fn write_atomic_file(path: &Path, body: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("supervisor artifact has no parent directory"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create supervisor artifact directory {}", parent.display()))?;
    let sequence = SUPERVISOR_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("ctx-supervisor"),
        std::process::id(),
        sequence,
    ));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temp)
            .with_context(|| format!("create supervisor artifact {}", temp.display()))?;
        file.write_all(body)
            .with_context(|| format!("write supervisor artifact {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("sync supervisor artifact {}", temp.display()))?;
        drop(file);
        fs::rename(&temp, path)
            .with_context(|| format!("publish supervisor artifact {}", path.display()))?;
        sync_supervisor_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(all(any(target_os = "linux", target_os = "macos", windows), unix))]
fn sync_supervisor_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .with_context(|| format!("open supervisor directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("sync supervisor directory {}", path.display()))
}

#[cfg(all(any(target_os = "linux", target_os = "macos", windows), not(unix)))]
fn sync_supervisor_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn native_supervisor_artifact_path(_data_root: &Path) -> Result<Option<PathBuf>> {
    super::linux_systemd_unit_path().map(Some)
}

#[cfg(target_os = "macos")]
pub(super) fn native_supervisor_artifact_path(_data_root: &Path) -> Result<Option<PathBuf>> {
    let home =
        crate::identity::home_dir().ok_or_else(|| anyhow!("resolve user home for LaunchAgent"))?;
    Ok(Some(
        home.join("Library")
            .join("LaunchAgents")
            .join("rs.ctx.daemon.plist"),
    ))
}

#[cfg(windows)]
pub(super) fn native_supervisor_artifact_path(data_root: &Path) -> Result<Option<PathBuf>> {
    Ok(Some(daemon_root_path(data_root).join("windows-task.xml")))
}

#[cfg(target_os = "freebsd")]
pub(super) fn native_supervisor_artifact_path(_data_root: &Path) -> Result<Option<PathBuf>> {
    Ok(None)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    windows
)))]
pub(super) fn native_supervisor_artifact_path(_data_root: &Path) -> Result<Option<PathBuf>> {
    Ok(None)
}

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
        "systemd user services are unavailable; retrieval commands retain CLI self-healing"
    }
    #[cfg(target_os = "macos")]
    {
        "LaunchAgent registration is unavailable; retrieval commands retain CLI self-healing"
    }
    #[cfg(windows)]
    {
        "Task Scheduler registration is unavailable; retrieval commands retain CLI self-healing"
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
        "this platform has no maintained native per-user supervisor integration; retrieval commands retain CLI self-healing"
    }
}
