use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ctx_history_core::default_data_root;
use serde_json::{json, Value};

use crate::{compact_json, identity};

use super::{
    paths_status::{daemon_lock_is_active, pid_from_lock_json, read_pid_lock_json},
    query_service::daemon_source_refresh_request,
};

mod state;
#[cfg(test)]
mod tests;

use state::{
    native_supervisor_artifact_path, native_supervisor_kind, native_supervisor_limitation,
    write_atomic_file, write_supervisor_receipt,
};

const SUPERVISOR_HANDOFF_TIMEOUT: Duration = Duration::from_secs(5);
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
        if verify_native_supervisor(data_root, &executable).is_ok() {
            write_supervisor_receipt(
                data_root,
                native_supervisor_kind(),
                "installed",
                true,
                true,
                native_supervisor_artifact_path(data_root)?.as_deref(),
                None,
                None,
            )?;
            return Ok(DaemonSupervisorStart::Native);
        }
    }
    let installation = (|| {
        let artifact = install_native_supervisor(data_root, &executable)?;
        wait_for_daemon_ownership(data_root)?;
        verify_native_supervisor(data_root, &executable)?;
        Ok::<_, anyhow::Error>(artifact)
    })();
    match installation {
        Ok(artifact) => {
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
            if let Err(cleanup_error) = disable_native_supervisor(data_root) {
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
            let authority_blocker = native_supervisor_product_authority_blocker();
            write_supervisor_receipt(
                data_root,
                if authority_blocker {
                    native_supervisor_kind()
                } else {
                    "cli_self_heal"
                },
                if authority_blocker {
                    "degraded"
                } else {
                    "install_failed"
                },
                false,
                false,
                None,
                Some(native_supervisor_limitation()),
                Some(format!("{error:#}")),
            )?;
            if authority_blocker {
                Ok(DaemonSupervisorStart::Fallback)
            } else {
                Err(error).context("install and verify native per-user ctx daemon supervisor")
            }
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
    let result = disable_native_supervisor(data_root);
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

fn verify_daemon_owner_identity(
    data_root: &Path,
    executable: &Path,
    manager_pid: Option<u32>,
) -> Result<u32> {
    if !daemon_lock_is_active(data_root) {
        return Err(anyhow!("native supervisor has no live daemon owner lock"));
    }
    let lock = read_pid_lock_json(&super::paths_status::daemon_lock_path(data_root))
        .ok_or_else(|| anyhow!("native supervisor daemon lock has no readable identity"))?;
    let pid = pid_from_lock_json(&lock)
        .ok_or_else(|| anyhow!("native supervisor daemon lock has no process identity"))?;
    if manager_pid.is_some_and(|expected| expected != pid) {
        return Err(anyhow!(
            "native supervisor process identity does not own the ctx daemon lock"
        ));
    }
    let recorded_executable = lock
        .get("binary")
        .and_then(Value::as_str)
        .map(Path::new)
        .ok_or_else(|| anyhow!("native supervisor daemon lock has no executable identity"))?;
    if !same_canonical_path(recorded_executable, executable) {
        return Err(anyhow!(
            "native supervisor daemon lock does not identify the installed ctx executable"
        ));
    }
    if let Some(process_executable) = supervisor_process_executable(pid) {
        if !same_canonical_path(&process_executable, executable) {
            return Err(anyhow!(
                "native supervisor live process is not the installed ctx executable"
            ));
        }
    } else {
        return Err(anyhow!(
            "native supervisor live process executable identity is unavailable"
        ));
    }
    Ok(pid)
}

fn same_canonical_path(left: &Path, right: &Path) -> bool {
    fs::canonicalize(left).ok() == fs::canonicalize(right).ok()
}

#[cfg(target_os = "linux")]
fn supervisor_process_executable(pid: u32) -> Option<PathBuf> {
    fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(target_os = "macos")]
fn supervisor_process_executable(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;

    const MAX_PATH_BYTES: usize = 4096;
    unsafe extern "C" {
        fn proc_pidpath(pid: libc::c_int, buffer: *mut libc::c_void, size: u32) -> libc::c_int;
    }
    let mut buffer = vec![0_u8; MAX_PATH_BYTES];
    let length = unsafe {
        proc_pidpath(
            libc::pid_t::try_from(pid).ok()?,
            buffer.as_mut_ptr().cast(),
            u32::try_from(buffer.len()).ok()?,
        )
    };
    if length <= 0 {
        return None;
    }
    CStr::from_bytes_until_nul(&buffer)
        .ok()
        .map(|path| PathBuf::from(path.to_string_lossy().into_owned()))
}

#[cfg(windows)]
fn supervisor_process_executable(pid: u32) -> Option<PathBuf> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut buffer = vec![0_u16; 32_768];
    let mut length = u32::try_from(buffer.len()).ok()?;
    let succeeded =
        unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &raw mut length) };
    unsafe {
        CloseHandle(handle);
    }
    (succeeded != 0).then(|| {
        PathBuf::from(String::from_utf16_lossy(
            &buffer[..usize::try_from(length).unwrap_or(0)],
        ))
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn supervisor_process_executable(_pid: u32) -> Option<PathBuf> {
    None
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
fn disable_native_supervisor(_data_root: &Path) -> Result<Option<PathBuf>> {
    let path = linux_systemd_unit_path()?;
    let disabled = systemctl_user_capture(["disable", "--now", "ctx.service"])?;
    if !disabled.status.success()
        && systemctl_user_capture(["is-enabled", "ctx.service"])?
            .status
            .success()
    {
        return Err(anyhow!(
            "systemd user service remained enabled after disable: {}",
            String::from_utf8_lossy(&disabled.stderr).trim()
        ));
    }
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("remove systemd user unit {}", path.display()))
        }
    }
    systemctl_user(["daemon-reload"])?;
    if systemctl_user_capture(["is-enabled", "ctx.service"])?
        .status
        .success()
        || systemctl_user_capture(["is-active", "ctx.service"])?
            .status
            .success()
    {
        return Err(anyhow!(
            "systemd user service remained registered or active after removal"
        ));
    }
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
    let output = systemctl_user_capture(args)?;
    if output.status.success() {
        return Ok(());
    }
    Err(anyhow!(
        "systemctl --user failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(target_os = "linux")]
fn systemctl_user_capture<const N: usize>(args: [&str; N]) -> Result<std::process::Output> {
    let mut command = supervisor_command("systemctl");
    command.arg("--user").args(args);
    supervisor_output(&mut command).context("run systemctl --user")
}

#[cfg(target_os = "linux")]
fn verify_native_supervisor(data_root: &Path, executable: &Path) -> Result<()> {
    let enabled = systemctl_user_capture(["is-enabled", "ctx.service"])?;
    if !enabled.status.success() || String::from_utf8_lossy(&enabled.stdout).trim() != "enabled" {
        return Err(anyhow!("systemd user service is not durably enabled"));
    }
    let active = systemctl_user_capture(["is-active", "ctx.service"])?;
    if !active.status.success() || String::from_utf8_lossy(&active.stdout).trim() != "active" {
        return Err(anyhow!("systemd user service is not active"));
    }
    let pid =
        systemctl_user_capture(["show", "ctx.service", "--property=MainPID", "--value"])?.stdout;
    let pid = systemd_main_pid(&pid)?;
    verify_daemon_owner_identity(data_root, executable, Some(pid)).map(|_| ())
}

fn systemd_main_pid(output: &[u8]) -> Result<u32> {
    String::from_utf8_lossy(output)
        .trim()
        .parse::<u32>()
        .context("parse systemd user service MainPID")
        .and_then(|pid| {
            (pid != 0)
                .then_some(pid)
                .ok_or_else(|| anyhow!("systemd user service has no live MainPID"))
        })
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
    let bootout = supervisor_output(&mut bootout).context("run launchctl bootout")?;
    let registration = launchctl_print(&domain)?;
    if registration.status.success() {
        return Err(anyhow!(
            "LaunchAgent remained registered after bootout: {}",
            String::from_utf8_lossy(&bootout.stderr).trim()
        ));
    }
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
fn disable_native_supervisor(_data_root: &Path) -> Result<Option<PathBuf>> {
    let home = identity::home_dir().ok_or_else(|| anyhow!("resolve user home for LaunchAgent"))?;
    let path = home
        .join("Library")
        .join("LaunchAgents")
        .join("rs.ctx.daemon.plist");
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let mut bootout = supervisor_command("launchctl");
    bootout.args(["bootout", &domain]).arg(&path);
    let bootout = supervisor_output(&mut bootout).context("run launchctl bootout")?;
    let registration = launchctl_print(&domain)?;
    if registration.status.success() {
        return Err(anyhow!(
            "LaunchAgent remained registered after bootout: {}",
            String::from_utf8_lossy(&bootout.stderr).trim()
        ));
    }
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("remove ctx LaunchAgent"),
    }
    Ok(Some(path))
}

fn launch_agent_plist(executable: &Path, data_root: &Path) -> String {
    let home = identity::home_dir().unwrap_or_default();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>rs.ctx.daemon</string>\n<key>ProgramArguments</key><array><string>/usr/bin/env</string><string>-i</string><string>HOME={}</string><string>PATH=/usr/local/bin:/usr/bin:/bin</string><string>{}</string><string>--data-root</string><string>{}</string><string>daemon</string><string>run</string><string>--format=json</string></array>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n<key>ProcessType</key><string>Background</string>\n<key>StandardOutPath</key><string>/dev/null</string>\n<key>StandardErrorPath</key><string>/dev/null</string>\n</dict></plist>\n",
        xml_escape(&home.to_string_lossy()),
        xml_escape(&executable.to_string_lossy()),
        xml_escape(&data_root.to_string_lossy()),
    )
}

#[cfg(target_os = "macos")]
fn launchctl_print(domain: &str) -> Result<std::process::Output> {
    let mut print = supervisor_command("launchctl");
    print.args(["print", &format!("{domain}/rs.ctx.daemon")]);
    supervisor_output(&mut print).context("run launchctl print in GUI domain")
}

fn launchctl_print_pid(output: &str) -> Option<u32> {
    output.lines().find_map(|line| {
        let (key, value) = line.trim().split_once('=')?;
        (key.trim() == "pid")
            .then(|| value.trim().parse::<u32>().ok())
            .flatten()
    })
}

#[cfg(target_os = "macos")]
fn verify_native_supervisor(data_root: &Path, executable: &Path) -> Result<()> {
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let output = launchctl_print(&domain)?;
    if !output.status.success() {
        return Err(anyhow!(
            "LaunchAgent is not registered in the current GUI login domain: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let pid = launchctl_print_pid(&String::from_utf8_lossy(&output))
        .ok_or_else(|| anyhow!("LaunchAgent GUI registration has no live process identity"))?;
    verify_daemon_owner_identity(data_root, executable, Some(pid)).map(|_| ())
}

#[cfg(windows)]
fn install_native_supervisor(data_root: &Path, executable: &Path) -> Result<PathBuf> {
    let path = daemon_root_path(data_root).join("windows-task.xml");
    let system_root =
        env::var_os("SystemRoot").ok_or_else(|| anyhow!("Windows SystemRoot is unavailable"))?;
    let sid = current_windows_user_sid()?;
    let task_name = windows_task_name(&sid);
    let xml = windows_task_xml(
        executable,
        data_root,
        Path::new(&system_root),
        &sid,
        &task_name,
    );
    write_atomic_file(&path, xml.as_bytes())?;

    let mut create = supervisor_command("schtasks");
    create
        .args(["/Create", "/TN"])
        .arg(&task_name)
        .arg("/XML")
        .arg(&path)
        .arg("/F");
    command_success(&mut create, "schtasks /Create")?;
    migrate_existing_daemon_to_supervisor(data_root)?;
    let mut run = supervisor_command("schtasks");
    run.args(["/Run", "/TN"]).arg(&task_name);
    command_success(&mut run, "schtasks /Run")?;
    Ok(path)
}

#[cfg(windows)]
fn disable_native_supervisor(data_root: &Path) -> Result<Option<PathBuf>> {
    let path = daemon_root_path(data_root).join("windows-task.xml");
    let task_name = windows_task_name(&current_windows_user_sid()?);
    let mut end = supervisor_command("schtasks");
    end.args(["/End", "/TN"]).arg(&task_name);
    let _ = supervisor_output(&mut end);
    let mut delete = supervisor_command("schtasks");
    delete.args(["/Delete", "/TN"]).arg(&task_name).arg("/F");
    let output = supervisor_output(&mut delete).context("run schtasks /Delete")?;
    let query = query_windows_task(&task_name)?;
    if !output.status.success() && query.status.success() {
        return Err(anyhow!(
            "schtasks /Delete failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if query.status.success() {
        return Err(anyhow!(
            "ctx scheduled task remained registered after deletion"
        ));
    }
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("remove ctx scheduled-task definition"),
    }
    Ok(Some(path))
}

const WINDOWS_TASK_PREFIX: &str = r"\ctx-daemon-";

fn windows_task_name(user_sid: &str) -> String {
    format!("{WINDOWS_TASK_PREFIX}{user_sid}")
}

fn windows_task_xml(
    executable: &Path,
    data_root: &Path,
    system_root: &Path,
    user_sid: &str,
    task_name: &str,
) -> String {
    let powershell = system_root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let script = windows_sanitized_daemon_script(executable, data_root);
    let encoded = BASE64.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Task version=\"1.4\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n<RegistrationInfo><URI>{}</URI><Description>ctx persistent history daemon</Description></RegistrationInfo>\n<Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{}</UserId></LogonTrigger></Triggers>\n<Principals><Principal id=\"Author\"><UserId>{}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\n<Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><StartWhenAvailable>true</StartWhenAvailable><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><RestartOnFailure><Interval>PT2S</Interval><Count>999</Count></RestartOnFailure></Settings>\n<Actions Context=\"Author\"><Exec><Command>{}</Command><Arguments>-NoLogo -NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand {}</Arguments></Exec></Actions>\n</Task>\n",
        xml_escape(task_name),
        xml_escape(user_sid),
        xml_escape(user_sid),
        xml_escape(&powershell.to_string_lossy()),
        encoded,
    )
}

fn windows_sanitized_daemon_script(executable: &Path, data_root: &Path) -> String {
    let allowlist = SUPERVISOR_ENV_ALLOWLIST
        .iter()
        .map(|name| format!("'{}'", powershell_single_quote(name)))
        .collect::<Vec<_>>()
        .join(",");
    let arguments = [
        "--data-root".to_owned(),
        data_root.to_string_lossy().into_owned(),
        "daemon".to_owned(),
        "run".to_owned(),
        "--format=json".to_owned(),
    ]
    .iter()
    .map(|argument| windows_command_line_quote(argument))
    .collect::<Vec<_>>()
    .join(" ");
    format!(
        "$ErrorActionPreference='Stop';$p=New-Object System.Diagnostics.ProcessStartInfo;$p.FileName='{}';$p.UseShellExecute=$false;$p.CreateNoWindow=$true;$p.EnvironmentVariables.Clear();foreach($n in @({allowlist})){{$v=[Environment]::GetEnvironmentVariable($n);if($null -ne $v){{$p.EnvironmentVariables[$n]=$v}}}};$p.Arguments='{}';$c=[Diagnostics.Process]::Start($p);$c.WaitForExit();exit $c.ExitCode",
        powershell_single_quote(&executable.to_string_lossy()),
        powershell_single_quote(&arguments),
    )
}

fn powershell_single_quote(value: &str) -> String {
    value.replace('\'', "''")
}

fn windows_command_line_quote(value: &str) -> String {
    if !value.is_empty()
        && !value
            .chars()
            .any(|character| character.is_whitespace() || character == '"')
    {
        return value.to_owned();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for character in value.chars() {
        if character == '\\' {
            backslashes += 1;
        } else if character == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
            quoted.push('"');
            backslashes = 0;
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
            backslashes = 0;
            quoted.push(character);
        }
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(windows)]
fn current_windows_user_sid() -> Result<String> {
    let mut command = supervisor_command("whoami");
    command.args(["/user", "/fo", "csv", "/nh"]);
    let output = supervisor_output(&mut command).context("query current Windows user SID")?;
    if !output.status.success() {
        return Err(anyhow!(
            "whoami /user failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8_lossy(&output.stdout)
        .split(',')
        .nth(1)
        .map(|value| value.trim().trim_matches('"').to_owned())
        .filter(|value| value.starts_with("S-1-"))
        .ok_or_else(|| anyhow!("whoami returned no current-user SID"))
}

#[cfg(windows)]
fn query_windows_task(task_name: &str) -> Result<std::process::Output> {
    let mut query = supervisor_command("schtasks");
    query.args(["/Query", "/TN"]).arg(task_name).arg("/XML");
    supervisor_output(&mut query).context("run schtasks /Query")
}

#[cfg(windows)]
fn verify_native_supervisor(data_root: &Path, executable: &Path) -> Result<()> {
    let system_root =
        env::var_os("SystemRoot").ok_or_else(|| anyhow!("Windows SystemRoot is unavailable"))?;
    let sid = current_windows_user_sid()?;
    let task_name = windows_task_name(&sid);
    let output = query_windows_task(&task_name)?;
    if !output.status.success() {
        return Err(anyhow!(
            "ctx current-user scheduled task is not registered: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let xml = decode_supervisor_text(&output.stdout);
    if !windows_task_registration_matches(
        &xml,
        executable,
        data_root,
        Path::new(&system_root),
        &sid,
        &task_name,
    ) {
        return Err(anyhow!(
            "ctx scheduled task registration does not match the maintained definition"
        ));
    }
    if !windows_task_is_running(&task_name, Path::new(&system_root))? {
        return Err(anyhow!(
            "ctx current-user scheduled task has no live supervisor ownership"
        ));
    }
    verify_daemon_owner_identity(data_root, executable, None).map(|_| ())
}

#[cfg(windows)]
fn windows_task_is_running(task_name: &str, system_root: &Path) -> Result<bool> {
    let powershell = system_root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let mut command = supervisor_command(
        powershell
            .to_str()
            .ok_or_else(|| anyhow!("Windows PowerShell path is not Unicode"))?,
    );
    command
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
        .arg(windows_task_state_script(task_name));
    let output = supervisor_output(&mut command).context("query scheduled-task running state")?;
    Ok(output.status.success() && parse_windows_task_state(&output.stdout) == Some(4))
}

fn windows_task_state_script(task_name: &str) -> String {
    let task = task_name.trim_start_matches('\\');
    format!(
        "$t=Get-ScheduledTask -TaskPath '\\' -TaskName '{}' -ErrorAction Stop;[Console]::Out.Write([int]$t.State)",
        powershell_single_quote(task),
    )
}

fn parse_windows_task_state(output: &[u8]) -> Option<u32> {
    decode_supervisor_text(output).trim().parse().ok()
}

fn windows_task_registration_matches(
    xml: &str,
    executable: &Path,
    data_root: &Path,
    system_root: &Path,
    user_sid: &str,
    task_name: &str,
) -> bool {
    let powershell = system_root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let script = windows_sanitized_daemon_script(executable, data_root);
    let encoded = BASE64.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    xml.contains(&format!("<URI>{}</URI>", xml_escape(task_name)))
        && xml.contains(&format!("<UserId>{}</UserId>", xml_escape(user_sid)))
        && xml.contains(&format!(
            "<Command>{}</Command>",
            xml_escape(&powershell.to_string_lossy())
        ))
        && xml.contains(&format!("-EncodedCommand {encoded}"))
        && xml.contains("-EncodedCommand")
        && xml.contains("<LogonType>InteractiveToken</LogonType>")
}

fn decode_supervisor_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.iter().skip(1).step_by(2).any(|byte| *byte == 0) {
        let units = bytes
            .strip_prefix(&[0xff, 0xfe])
            .unwrap_or(bytes)
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

#[cfg(target_os = "freebsd")]
fn install_native_supervisor(_data_root: &Path, _executable: &Path) -> Result<PathBuf> {
    Err(anyhow!(native_supervisor_limitation()))
}

#[cfg(target_os = "freebsd")]
fn disable_native_supervisor(_data_root: &Path) -> Result<Option<PathBuf>> {
    Ok(None)
}

#[cfg(target_os = "freebsd")]
fn verify_native_supervisor(_data_root: &Path, _executable: &Path) -> Result<()> {
    Err(anyhow!(native_supervisor_limitation()))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    windows
)))]
fn install_native_supervisor(_data_root: &Path, _executable: &Path) -> Result<PathBuf> {
    Err(anyhow!(native_supervisor_limitation()))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    windows
)))]
fn disable_native_supervisor(_data_root: &Path) -> Result<Option<PathBuf>> {
    Ok(None)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    windows
)))]
fn verify_native_supervisor(_data_root: &Path, _executable: &Path) -> Result<()> {
    Err(anyhow!(native_supervisor_limitation()))
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

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(super) fn daemon_supervisor_report(data_root: &Path) -> Value {
    state::daemon_supervisor_report(data_root)
}

fn freebsd_supervisor_authority_blocker() -> &'static str {
    "FreeBSD has no standard current-user service manager with both login/boot registration and identity-verifiable restart ownership; ctx will not mutate the user's crontab or claim rc.d authority, so retrieval commands retain typed CLI self-healing"
}

fn native_supervisor_product_authority_blocker() -> bool {
    cfg!(not(any(target_os = "linux", target_os = "macos", windows)))
}
