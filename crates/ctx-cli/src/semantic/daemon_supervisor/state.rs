use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::compact_json;

use super::super::{
    health_search::create_private_dir_all,
    paths_status::{
        daemon_root_path, pid_from_lock_json, read_pid_lock_json, write_private_json_file,
    },
};

const SUPERVISOR_RECEIPT_FILE: &str = "supervisor.json";
static SUPERVISOR_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

#[cfg(unix)]
fn sync_supervisor_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .with_context(|| format!("open supervisor directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("sync supervisor directory {}", path.display()))
}

#[cfg(not(unix))]
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

pub(super) fn write_supervisor_receipt(
    data_root: &Path,
    kind: &str,
    status: &str,
    autostart: bool,
    restart: bool,
    artifact: Option<&Path>,
    limitation: Option<&str>,
    last_error: Option<String>,
) -> Result<()> {
    let root = daemon_root_path(data_root);
    create_private_dir_all(&root)?;
    let installed = status == "installed";
    let owner_pid = installed
        .then(|| read_pid_lock_json(&super::super::paths_status::daemon_lock_path(data_root)))
        .flatten()
        .as_ref()
        .and_then(pid_from_lock_json);
    write_private_json_file(
        &root.join(SUPERVISOR_RECEIPT_FILE),
        &compact_json(json!({
            "schema_version": 1,
            "kind": kind,
            "status": status,
            "autostart_supported": autostart,
            "restart_supported": restart,
            "registration_verified": installed,
            "live_owner_verified": installed,
            "owner_pid": owner_pid,
            "artifact_path": artifact,
            "limitation": limitation,
            "last_error": last_error,
        })),
    )
}

pub(super) fn daemon_supervisor_report(data_root: &Path) -> Value {
    let path = daemon_root_path(data_root).join(SUPERVISOR_RECEIPT_FILE);
    fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| {
            compact_json(json!({
                "schema_version": 1,
                "kind": "unconfigured",
                "status": "unknown",
                "autostart_supported": false,
                "restart_supported": false,
                "receipt_path": path,
            }))
        })
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
