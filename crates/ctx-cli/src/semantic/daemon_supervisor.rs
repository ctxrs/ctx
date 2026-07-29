use std::{
    env, fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::default_data_root;
use serde_json::{json, Value};

use crate::{compact_json, identity};

use super::{
    health_search::create_private_dir_all,
    paths_status::{daemon_lock_is_active, daemon_root_path, write_private_json_file},
    query_service::daemon_source_refresh_request,
};

const SUPERVISOR_RECEIPT_FILE: &str = "supervisor.json";
const SUPERVISOR_HANDOFF_TIMEOUT: Duration = Duration::from_secs(5);
static SUPERVISOR_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const SUPERVISOR_ENV_ALLOWLIST: &[&str] = &[
    "DBUS_SESSION_BUS_ADDRESS",
    "DISPLAY",
    "HOME",
    "LANG",
    "LC_ALL",
    "PATH",
    "SystemRoot",
    "TEMP",
    "TMP",
    "TMPDIR",
    "USER",
    "USERPROFILE",
    "WAYLAND_DISPLAY",
    "WINDIR",
    "XDG_CONFIG_HOME",
    "XDG_RUNTIME_DIR",
];

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum DaemonSupervisorStart {
    Native,
    Fallback,
}

pub(super) fn ensure_daemon_supervisor(data_root: &Path) -> Result<DaemonSupervisorStart> {
    let Some(executable) = safely_supported_managed_install(data_root)? else {
        write_supervisor_receipt(
            data_root,
            "cli_self_heal",
            "fallback",
            false,
            false,
            None,
            Some(
                "native per-user restart registration requires the hosted installer and the default data root",
            ),
            None,
        )?;
        return Ok(DaemonSupervisorStart::Fallback);
    };
    let current = daemon_supervisor_report(data_root);
    let current_artifact_exists = current
        .get("artifact_path")
        .and_then(Value::as_str)
        .map(Path::new)
        .is_some_and(Path::is_file);
    if current.get("kind").and_then(Value::as_str) == Some(native_supervisor_kind())
        && current.get("status").and_then(Value::as_str) == Some("installed")
        && current_artifact_exists
        && daemon_lock_is_active(data_root)
    {
        return Ok(DaemonSupervisorStart::Native);
    }
    match install_native_supervisor(data_root, &executable) {
        Ok(artifact) => {
            wait_for_daemon_ownership(data_root)?;
            write_supervisor_receipt(
                data_root,
                native_supervisor_kind(),
                "installed",
                true,
                true,
                Some(&artifact),
                None,
                None,
            )?;
            Ok(DaemonSupervisorStart::Native)
        }
        Err(error) => {
            if let Err(cleanup_error) = disable_native_supervisor() {
                write_supervisor_receipt(
                    data_root,
                    native_supervisor_kind(),
                    "install_cleanup_failed",
                    false,
                    false,
                    native_supervisor_artifact_path(data_root)?.as_deref(),
                    Some("native registration failed and its partial state could not be removed"),
                    Some(format!("{error:#}; cleanup: {cleanup_error:#}")),
                )?;
                return Err(error.context(format!(
                    "also failed to remove partial native supervisor state: {cleanup_error:#}"
                )));
            }
            write_supervisor_receipt(
                data_root,
                "cli_self_heal",
                "fallback",
                false,
                false,
                None,
                Some(native_supervisor_limitation()),
                Some(format!("{error:#}")),
            )?;
            Ok(DaemonSupervisorStart::Fallback)
        }
    }
}

pub(super) fn disable_daemon_supervisor(data_root: &Path) -> Result<()> {
    let current = daemon_supervisor_report(data_root);
    let receipt_installed = current.get("kind").and_then(Value::as_str)
        == Some(native_supervisor_kind())
        && matches!(
            current.get("status").and_then(Value::as_str),
            Some("installed" | "install_cleanup_failed" | "disable_failed")
        );
    let interrupted_native_install = safely_supported_managed_install(data_root)?.is_some()
        && native_supervisor_artifact_path(data_root)?
            .as_deref()
            .is_some_and(Path::exists);
    if !receipt_installed && !interrupted_native_install {
        return write_supervisor_receipt(
            data_root,
            current
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("cli_self_heal"),
            "disabled",
            false,
            false,
            None,
            None,
            None,
        );
    }
    let result = disable_native_supervisor();
    match result {
        Ok(artifact) => write_supervisor_receipt(
            data_root,
            native_supervisor_kind(),
            "disabled",
            false,
            false,
            artifact.as_deref(),
            None,
            None,
        ),
        Err(error) => {
            write_supervisor_receipt(
                data_root,
                native_supervisor_kind(),
                "disable_failed",
                false,
                false,
                None,
                Some("native per-user registration could not be fully removed"),
                Some(format!("{error:#}")),
            )?;
            Err(error)
        }
    }
}

fn wait_for_daemon_ownership(data_root: &Path) -> Result<()> {
    let deadline = Instant::now() + SUPERVISOR_HANDOFF_TIMEOUT;
    while !daemon_lock_is_active(data_root) {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "native supervisor did not start daemon lifecycle ownership"
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

fn safely_supported_managed_install(data_root: &Path) -> Result<Option<PathBuf>> {
    let default_root = default_data_root().context("resolve default ctx data root")?;
    if data_root != default_root {
        return Ok(None);
    }
    crate::upgrade::managed_install_executable()
}

fn migrate_existing_daemon_to_supervisor(data_root: &Path) -> Result<()> {
    if !daemon_lock_is_active(data_root) {
        return Ok(());
    }
    let response = daemon_source_refresh_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "supervisor_handoff",
        })),
        Duration::from_millis(500),
        16 * 1024,
    )?;
    if response
        .as_ref()
        .and_then(|value| value.get("ok"))
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(anyhow!(
            "running daemon did not accept native-supervisor handoff"
        ));
    }
    let deadline = Instant::now() + SUPERVISOR_HANDOFF_TIMEOUT;
    while daemon_lock_is_active(data_root) {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out waiting for daemon native-supervisor handoff"
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_native_supervisor(data_root: &Path, executable: &Path) -> Result<PathBuf> {
    let path = linux_systemd_unit_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("systemd user unit has no parent directory"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create systemd user unit directory {}", parent.display()))?;
    write_atomic_file(&path, linux_systemd_unit(executable, data_root).as_bytes())?;
    systemctl_user(["daemon-reload"])?;
    systemctl_user(["enable", "ctx.service"])?;
    migrate_existing_daemon_to_supervisor(data_root)?;
    systemctl_user(["start", "ctx.service"])?;
    Ok(path)
}

#[cfg(target_os = "linux")]
fn disable_native_supervisor() -> Result<Option<PathBuf>> {
    let path = linux_systemd_unit_path()?;
    let _ = systemctl_user(["disable", "--now", "ctx.service"]);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("remove systemd user unit {}", path.display()))
        }
    }
    systemctl_user(["daemon-reload"])?;
    Ok(Some(path))
}

#[cfg(target_os = "linux")]
fn linux_systemd_unit_path() -> Result<PathBuf> {
    let root = env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| identity::home_dir().map(|home| home.join(".config")))
        .ok_or_else(|| anyhow!("resolve user configuration directory for systemd"))?;
    Ok(root.join("systemd").join("user").join("ctx.service"))
}

#[cfg(target_os = "linux")]
fn linux_systemd_unit(executable: &Path, data_root: &Path) -> String {
    format!(
        "[Unit]\nDescription=ctx persistent history daemon\n\n[Service]\nType=simple\nExecStart=/usr/bin/env -i HOME=%h PATH=/usr/local/bin:/usr/bin:/bin {} --data-root {} daemon run --format=json\nRestart=on-failure\nRestartSec=2\nStandardOutput=null\nStandardError=journal\n\n[Install]\nWantedBy=default.target\n",
        systemd_quote(executable),
        systemd_quote(data_root),
    )
}

#[cfg(target_os = "linux")]
fn systemd_quote(path: &Path) -> String {
    let value = path.to_string_lossy().replace('%', "%%");
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(target_os = "linux")]
fn systemctl_user<const N: usize>(args: [&str; N]) -> Result<()> {
    let mut command = supervisor_command("systemctl");
    command.arg("--user").args(args);
    let output = supervisor_output(&mut command).context("run systemctl --user")?;
    if output.status.success() {
        return Ok(());
    }
    Err(anyhow!(
        "systemctl --user failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(target_os = "macos")]
fn install_native_supervisor(data_root: &Path, executable: &Path) -> Result<PathBuf> {
    let home = identity::home_dir().ok_or_else(|| anyhow!("resolve user home for LaunchAgent"))?;
    let path = home
        .join("Library")
        .join("LaunchAgents")
        .join("rs.ctx.daemon.plist");
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("LaunchAgent has no parent directory"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create LaunchAgent directory {}", parent.display()))?;
    write_atomic_file(&path, launch_agent_plist(executable, data_root).as_bytes())?;
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let mut bootout = supervisor_command("launchctl");
    bootout.args(["bootout", &domain]).arg(&path);
    let _ = supervisor_output(&mut bootout);
    migrate_existing_daemon_to_supervisor(data_root)?;
    let mut bootstrap = supervisor_command("launchctl");
    bootstrap.args(["bootstrap", &domain]).arg(&path);
    command_success(&mut bootstrap, "launchctl bootstrap")?;
    let mut kickstart = supervisor_command("launchctl");
    kickstart.args(["kickstart", "-k", &format!("{domain}/rs.ctx.daemon")]);
    command_success(&mut kickstart, "launchctl kickstart")?;
    Ok(path)
}

#[cfg(target_os = "macos")]
fn disable_native_supervisor() -> Result<Option<PathBuf>> {
    let home = identity::home_dir().ok_or_else(|| anyhow!("resolve user home for LaunchAgent"))?;
    let path = home
        .join("Library")
        .join("LaunchAgents")
        .join("rs.ctx.daemon.plist");
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let mut bootout = supervisor_command("launchctl");
    bootout.args(["bootout", &domain]).arg(&path);
    let _ = supervisor_output(&mut bootout);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("remove ctx LaunchAgent"),
    }
    Ok(Some(path))
}

#[cfg(target_os = "macos")]
fn launch_agent_plist(executable: &Path, data_root: &Path) -> String {
    let home = identity::home_dir().unwrap_or_default();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>rs.ctx.daemon</string>\n<key>ProgramArguments</key><array><string>/usr/bin/env</string><string>-i</string><string>HOME={}</string><string>PATH=/usr/local/bin:/usr/bin:/bin</string><string>{}</string><string>--data-root</string><string>{}</string><string>daemon</string><string>run</string><string>--format=json</string></array>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n<key>ProcessType</key><string>Background</string>\n<key>StandardOutPath</key><string>/dev/null</string>\n<key>StandardErrorPath</key><string>/dev/null</string>\n</dict></plist>\n",
        xml_escape(&home.to_string_lossy()),
        xml_escape(&executable.to_string_lossy()),
        xml_escape(&data_root.to_string_lossy()),
    )
}

#[cfg(windows)]
fn install_native_supervisor(_data_root: &Path, _executable: &Path) -> Result<PathBuf> {
    Err(anyhow!(native_supervisor_limitation()))
}

#[cfg(windows)]
fn disable_native_supervisor() -> Result<Option<PathBuf>> {
    let mut end = supervisor_command("schtasks");
    end.args(["/End", "/TN", "ctx-daemon"]);
    let _ = supervisor_output(&mut end);
    let mut delete = supervisor_command("schtasks");
    delete.args(["/Delete", "/TN", "ctx-daemon", "/F"]);
    let output = supervisor_output(&mut delete).context("run schtasks /Delete")?;
    if output.status.success() || String::from_utf8_lossy(&output.stderr).contains("cannot find") {
        return Ok(None);
    }
    Err(anyhow!(
        "schtasks /Delete failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn install_native_supervisor(_data_root: &Path, _executable: &Path) -> Result<PathBuf> {
    Err(anyhow!(native_supervisor_limitation()))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn disable_native_supervisor() -> Result<Option<PathBuf>> {
    Ok(None)
}

#[cfg(any(target_os = "macos", windows))]
fn command_success(command: &mut Command, label: &str) -> Result<()> {
    let output = supervisor_output(command).with_context(|| format!("run {label}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(anyhow!(
        "{label} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

fn supervisor_command(program: &str) -> Command {
    let inherited = SUPERVISOR_ENV_ALLOWLIST
        .iter()
        .filter_map(|name| env::var_os(name).map(|value| (*name, value)))
        .collect::<Vec<_>>();
    let mut command = Command::new(program);
    command.env_clear().envs(inherited);
    command
}

fn supervisor_output(command: &mut Command) -> std::io::Result<std::process::Output> {
    crate::process_environment::sanitize_release_authority_env(command);
    command.output()
}

#[cfg(any(target_os = "macos", windows))]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn write_atomic_file(path: &Path, body: &[u8]) -> Result<()> {
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
fn native_supervisor_artifact_path(_data_root: &Path) -> Result<Option<PathBuf>> {
    linux_systemd_unit_path().map(Some)
}

#[cfg(target_os = "macos")]
fn native_supervisor_artifact_path(_data_root: &Path) -> Result<Option<PathBuf>> {
    let home = identity::home_dir().ok_or_else(|| anyhow!("resolve user home for LaunchAgent"))?;
    Ok(Some(
        home.join("Library")
            .join("LaunchAgents")
            .join("rs.ctx.daemon.plist"),
    ))
}

#[cfg(windows)]
fn native_supervisor_artifact_path(data_root: &Path) -> Result<Option<PathBuf>> {
    Ok(Some(daemon_root_path(data_root).join("windows-task.xml")))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn native_supervisor_artifact_path(_data_root: &Path) -> Result<Option<PathBuf>> {
    Ok(None)
}

fn write_supervisor_receipt(
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
    write_private_json_file(
        &root.join(SUPERVISOR_RECEIPT_FILE),
        &compact_json(json!({
            "schema_version": 1,
            "kind": kind,
            "status": status,
            "autostart_supported": autostart,
            "restart_supported": restart,
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
fn native_supervisor_kind() -> &'static str {
    "systemd_user"
}

#[cfg(target_os = "macos")]
fn native_supervisor_kind() -> &'static str {
    "launch_agent"
}

#[cfg(windows)]
fn native_supervisor_kind() -> &'static str {
    "windows_task_scheduler"
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn native_supervisor_kind() -> &'static str {
    "unsupported"
}

fn native_supervisor_limitation() -> &'static str {
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
        "Task Scheduler cannot guarantee a clear inherited environment for ctx; retrieval commands retain CLI self-healing"
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        "this platform has no maintained native per-user supervisor integration; retrieval commands retain CLI self-healing"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn systemd_unit_is_persistent_and_restart_on_failure() {
        let unit = linux_systemd_unit(
            Path::new("/home/user/.local/bin/ctx"),
            Path::new("/home/user/.local/share/ctx"),
        );
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("ExecStart=/usr/bin/env -i "));
        assert!(!unit.contains("CTX_RELEASE_"));
        assert!(!unit.contains("idle-exit-seconds"));
        assert!(!unit.contains("loop-interval-seconds"));
    }
}
