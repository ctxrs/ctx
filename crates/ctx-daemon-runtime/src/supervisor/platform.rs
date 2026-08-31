use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use crate::{
    daemon_lock_binary_identity_matches, daemon_lock_is_active, daemon_lock_path,
    pid_from_lock_json, read_pid_lock_json, SupervisorManagerOperability,
};

#[cfg(target_os = "macos")]
use crate::write_atomic_supervisor_file;
#[cfg(target_os = "linux")]
use crate::write_atomic_supervisor_file_if_changed;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::{SupervisorIdentity, SupervisorSpec};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SupervisorManagerEnvironment {
    values: BTreeMap<OsString, OsString>,
}

impl SupervisorManagerEnvironment {
    pub fn new(values: BTreeMap<OsString, OsString>) -> Self {
        Self { values }
    }

    pub fn values(&self) -> &BTreeMap<OsString, OsString> {
        &self.values
    }

    pub fn get(&self, name: &str) -> Option<&OsStr> {
        self.values.get(OsStr::new(name)).map(OsString::as_os_str)
    }
}

pub fn manager_environment_value<'a>(
    environment: &'a SupervisorManagerEnvironment,
    name: &str,
) -> Option<&'a OsStr> {
    environment.get(name)
}

pub fn supervisor_command(
    program: &str,
    manager_environment: &SupervisorManagerEnvironment,
) -> Command {
    let mut command = Command::new(program);
    command.env_clear().envs(manager_environment.values());
    command
}

pub fn supervisor_output(command: &mut Command) -> std::io::Result<Output> {
    command.output()
}

pub fn command_success(command: &mut Command, label: &str) -> Result<()> {
    let output = supervisor_output(command).with_context(|| format!("run {label}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(anyhow!(
        "{label} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

pub(crate) fn probe_supervisor_manager_bounded(
    command: &mut Command,
    label: &str,
) -> Result<SupervisorManagerOperability> {
    probe_supervisor_manager_with_timeout(
        command,
        label,
        Duration::from_secs(5),
        Duration::from_millis(10),
    )
}

fn probe_supervisor_manager_with_timeout(
    command: &mut Command,
    label: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<SupervisorManagerOperability> {
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SupervisorManagerOperability::Unavailable {
                reason: format!("{label} is unavailable: {error}"),
            });
        }
        Err(error) => {
            return Err(error).with_context(|| format!("start {label} operability probe"));
        }
    };
    let started = Instant::now();
    loop {
        match child
            .try_wait()
            .with_context(|| format!("observe {label} operability probe"))?
        {
            Some(status) if status.success() => {
                return Ok(SupervisorManagerOperability::Operational);
            }
            Some(status) => {
                return Ok(SupervisorManagerOperability::Unavailable {
                    reason: format!("{label} is unavailable: process exited with {status}"),
                });
            }
            None => {}
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            child
                .kill()
                .with_context(|| format!("terminate timed-out {label} operability probe"))?;
            child
                .wait()
                .with_context(|| format!("reap timed-out {label} operability probe"))?;
            return Ok(SupervisorManagerOperability::Unavailable {
                reason: format!(
                    "{label} is unavailable: operability probe timed out after {} ms",
                    timeout.as_millis()
                ),
            });
        }
        thread::sleep(
            poll_interval
                .max(Duration::from_millis(1))
                .min(timeout.saturating_sub(elapsed)),
        );
    }
}

#[cfg(target_os = "linux")]
pub fn probe_systemd_user_manager(
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<SupervisorManagerOperability> {
    let mut command = supervisor_command("systemctl", manager_environment);
    command.args(["--user", "show", "--property=Version", "--value"]);
    probe_supervisor_manager_bounded(&mut command, "systemd user manager")
}

#[cfg(target_os = "linux")]
pub fn linux_systemd_unit_path(
    manager_environment: &SupervisorManagerEnvironment,
    service_name: &str,
) -> Result<PathBuf> {
    let root = manager_environment_value(manager_environment, "XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            manager_environment_value(manager_environment, "HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .ok_or_else(|| anyhow!("resolve user configuration directory for systemd"))?;
    Ok(root.join("systemd").join("user").join(service_name))
}

#[cfg(target_os = "linux")]
pub fn install_systemd_supervisor(
    data_root: &Path,
    spec: &SupervisorSpec,
    manager_environment: &SupervisorManagerEnvironment,
    migrate_owner: &dyn Fn(&Path) -> Result<()>,
) -> Result<PathBuf> {
    let identity = spec.identity();
    let service_name = identity.name();
    let path = identity.artifact_path();
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("systemd user unit has no parent directory"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create systemd user unit directory {}", parent.display()))?;
    crate::write_supervisor_environment(spec)?;
    let unit = super::linux_systemd_unit(spec)?;
    write_atomic_supervisor_file_if_changed(path, unit.as_bytes())?;
    // Installation is a repair path: reload even when the on-disk bytes are
    // already current because systemd may still hold an older loaded unit.
    systemctl_user(["daemon-reload"], manager_environment)?;
    systemctl_user(["enable", service_name], manager_environment)?;
    migrate_owner(data_root)?;
    restart_systemd_supervisor(identity, manager_environment)?;
    Ok(path.to_path_buf())
}

#[cfg(target_os = "linux")]
pub fn disable_systemd_supervisor(
    identity: &SupervisorIdentity,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Option<PathBuf>> {
    let service_name = identity.name();
    let path = identity.artifact_path();
    let disabled = systemctl_user_capture(["disable", "--now", service_name], manager_environment)?;
    if !disabled.status.success()
        && systemctl_user_capture(["is-enabled", service_name], manager_environment)?
            .status
            .success()
    {
        return Err(anyhow!(
            "systemd user service remained enabled after disable: {}",
            String::from_utf8_lossy(&disabled.stderr).trim()
        ));
    }
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("remove systemd user unit {}", path.display()))
        }
    }
    systemctl_user(["daemon-reload"], manager_environment)?;
    if systemctl_user_capture(["is-enabled", service_name], manager_environment)?
        .status
        .success()
        || systemctl_user_capture(["is-active", service_name], manager_environment)?
            .status
            .success()
    {
        return Err(anyhow!(
            "systemd user service remained registered or active after removal"
        ));
    }
    Ok(Some(path.to_path_buf()))
}

#[cfg(target_os = "linux")]
pub fn verify_systemd_registration(
    spec: &SupervisorSpec,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    crate::verify_supervisor_environment(spec)?;
    let identity = spec.identity();
    let service_name = identity.name();
    let path = identity.artifact_path();
    let registered = fs::read_to_string(path)
        .with_context(|| format!("read systemd user unit {}", path.display()))?;
    if registered != super::linux_systemd_unit(spec)? {
        return Err(anyhow!(
            "systemd user service registration does not match the maintained definition"
        ));
    }
    let enabled = systemctl_user_capture(["is-enabled", service_name], manager_environment)?;
    if !enabled.status.success() || String::from_utf8_lossy(&enabled.stdout).trim() != "enabled" {
        return Err(anyhow!("systemd user service is not durably enabled"));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn systemd_live_owner_pid(
    spec: &SupervisorSpec,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<u32> {
    let service_name = spec.identity().name();
    verify_systemd_registration(spec, manager_environment)?;
    let active = systemctl_user_capture(["is-active", service_name], manager_environment)?;
    if !active.status.success() || String::from_utf8_lossy(&active.stdout).trim() != "active" {
        return Err(anyhow!("systemd user service is not active"));
    }
    let output = systemctl_user_capture(
        ["show", service_name, "--property=MainPID", "--value"],
        manager_environment,
    )?;
    systemd_main_pid(&output.stdout)
}

#[cfg(target_os = "linux")]
pub fn start_systemd_supervisor(
    identity: &SupervisorIdentity,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    systemctl_user(["start", identity.name()], manager_environment)
}

#[cfg(target_os = "linux")]
fn restart_systemd_supervisor(
    identity: &SupervisorIdentity,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    systemctl_user(["restart", identity.name()], manager_environment)
}

#[cfg(target_os = "linux")]
fn systemctl_user<const N: usize>(
    args: [&str; N],
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    let output = systemctl_user_capture(args, manager_environment)?;
    if output.status.success() {
        return Ok(());
    }
    Err(anyhow!(
        "systemctl --user failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(target_os = "linux")]
pub fn systemctl_user_capture<const N: usize>(
    args: [&str; N],
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Output> {
    let mut command = supervisor_command("systemctl", manager_environment);
    command.arg("--user").args(args);
    supervisor_output(&mut command).context("run systemctl --user")
}

pub fn systemd_main_pid(output: &[u8]) -> Result<u32> {
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
pub fn launch_agent_path(
    manager_environment: &SupervisorManagerEnvironment,
    label: &str,
) -> Result<PathBuf> {
    let home = manager_environment_value(manager_environment, "HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("resolve user home for LaunchAgent"))?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist")))
}

#[cfg(target_os = "macos")]
fn launchctl_domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

#[cfg(target_os = "macos")]
pub fn probe_launchd_gui_user_domain(
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<SupervisorManagerOperability> {
    let domain = launchctl_domain();
    let mut command = launchd_gui_user_domain_probe_command(manager_environment, &domain);
    probe_supervisor_manager_bounded(&mut command, "launchd GUI user domain")
}

#[cfg(any(test, target_os = "macos"))]
fn launchd_gui_user_domain_probe_command(
    manager_environment: &SupervisorManagerEnvironment,
    domain: &str,
) -> Command {
    let mut command = supervisor_command("launchctl", manager_environment);
    command.args(["print", domain]);
    command
}

#[cfg(target_os = "macos")]
pub fn install_launch_agent(
    data_root: &Path,
    spec: &SupervisorSpec,
    manager_environment: &SupervisorManagerEnvironment,
    migrate_owner: &dyn Fn(&Path) -> Result<()>,
) -> Result<PathBuf> {
    let identity = spec.identity();
    let path = identity.artifact_path();
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("LaunchAgent has no parent directory"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create LaunchAgent directory {}", parent.display()))?;
    crate::write_supervisor_environment(spec)?;
    write_atomic_supervisor_file(path, super::launch_agent_plist(spec)?.as_bytes())?;
    let domain = launchctl_domain();
    let mut bootout = supervisor_command("launchctl", manager_environment);
    bootout.args(["bootout", &domain]).arg(path);
    let bootout = supervisor_output(&mut bootout).context("run launchctl bootout")?;
    if launchctl_print(&domain, identity.name(), manager_environment)?
        .status
        .success()
    {
        return Err(anyhow!(
            "LaunchAgent remained registered after bootout: {}",
            String::from_utf8_lossy(&bootout.stderr).trim()
        ));
    }
    migrate_owner(data_root)?;
    let mut bootstrap = supervisor_command("launchctl", manager_environment);
    bootstrap.args(["bootstrap", &domain]).arg(path);
    command_success(&mut bootstrap, "launchctl bootstrap")?;
    start_launch_agent(identity, manager_environment)?;
    Ok(path.to_path_buf())
}

#[cfg(target_os = "macos")]
pub fn disable_launch_agent(
    identity: &SupervisorIdentity,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Option<PathBuf>> {
    let label = identity.name();
    let path = identity.artifact_path();
    let domain = launchctl_domain();
    let mut bootout = supervisor_command("launchctl", manager_environment);
    bootout.args(["bootout", &domain]).arg(path);
    let bootout = supervisor_output(&mut bootout).context("run launchctl bootout")?;
    if launchctl_print(&domain, label, manager_environment)?
        .status
        .success()
    {
        return Err(anyhow!(
            "LaunchAgent remained registered after bootout: {}",
            String::from_utf8_lossy(&bootout.stderr).trim()
        ));
    }
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("remove ctx LaunchAgent"),
    }
    Ok(Some(path.to_path_buf()))
}

#[cfg(target_os = "macos")]
pub fn verify_launch_agent_registration(
    spec: &SupervisorSpec,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    crate::verify_supervisor_environment(spec)?;
    let identity = spec.identity();
    let path = identity.artifact_path();
    let registered =
        fs::read_to_string(path).with_context(|| format!("read LaunchAgent {}", path.display()))?;
    if registered != super::launch_agent_plist(spec)? {
        return Err(anyhow!(
            "LaunchAgent registration does not match the maintained definition"
        ));
    }
    let output = launchctl_print(&launchctl_domain(), identity.name(), manager_environment)?;
    if !output.status.success() {
        return Err(anyhow!(
            "LaunchAgent is not registered in the current GUI login domain: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn launch_agent_live_owner_pid(
    spec: &SupervisorSpec,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<u32> {
    verify_launch_agent_registration(spec, manager_environment)?;
    let output = launchctl_print(
        &launchctl_domain(),
        spec.identity().name(),
        manager_environment,
    )?;
    launchctl_print_pid(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| anyhow!("LaunchAgent GUI registration has no live process identity"))
}

#[cfg(target_os = "macos")]
fn launchctl_print(
    domain: &str,
    label: &str,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Output> {
    let mut print = supervisor_command("launchctl", manager_environment);
    print.args(["print", &format!("{domain}/{label}")]);
    supervisor_output(&mut print).context("run launchctl print in GUI domain")
}

pub fn launchctl_print_pid(output: &str) -> Option<u32> {
    output.lines().find_map(|line| {
        let (key, value) = line.trim().split_once('=')?;
        (key.trim() == "pid")
            .then(|| value.trim().parse::<u32>().ok())
            .flatten()
    })
}

#[cfg(target_os = "macos")]
pub fn start_launch_agent(
    identity: &SupervisorIdentity,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    let domain = launchctl_domain();
    let mut kickstart = supervisor_command("launchctl", manager_environment);
    kickstart.args(["kickstart", "-k", &format!("{domain}/{}", identity.name())]);
    command_success(&mut kickstart, "launchctl kickstart")
}

pub fn verify_daemon_owner_identity(
    data_root: &Path,
    executable: &Path,
    manager_pid: Option<u32>,
) -> Result<u32> {
    if !daemon_lock_is_active(data_root) {
        return Err(anyhow!("native supervisor has no live daemon owner lock"));
    }
    let lock = read_pid_lock_json(&daemon_lock_path(data_root))
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
    if !daemon_lock_binary_identity_matches(&lock, executable)? {
        return Err(anyhow!(
            "native supervisor daemon lock identifies a different ctx binary image"
        ));
    }
    let process_executable = supervisor_process_executable(pid).ok_or_else(|| {
        anyhow!("native supervisor live process executable identity is unavailable")
    })?;
    if !same_canonical_path(&process_executable, executable) {
        return Err(anyhow!(
            "native supervisor live process is not the installed ctx executable"
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
    unsafe { CloseHandle(handle) };
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::cell::Cell;
    use std::os::unix::fs::{symlink, MetadataExt as _, PermissionsExt as _};

    use super::*;

    #[test]
    fn systemd_probe_distinguishes_an_operational_manager_from_an_unavailable_one() {
        let temp = tempfile::tempdir().expect("temporary manager probe root");
        let systemctl = temp.path().join("systemctl");
        symlink("/bin/true", &systemctl).expect("link operational systemctl");
        let environment = SupervisorManagerEnvironment::new(BTreeMap::from([(
            OsString::from("PATH"),
            temp.path().as_os_str().to_os_string(),
        )]));

        assert_eq!(
            probe_systemd_user_manager(&environment).expect("operational manager probe"),
            SupervisorManagerOperability::Operational
        );

        let unavailable = tempfile::tempdir().expect("temporary unavailable manager root");
        symlink("/bin/false", unavailable.path().join("systemctl"))
            .expect("link unavailable systemctl");
        let unavailable_environment = SupervisorManagerEnvironment::new(BTreeMap::from([(
            OsString::from("PATH"),
            unavailable.path().as_os_str().to_os_string(),
        )]));
        let probe = probe_systemd_user_manager(&unavailable_environment)
            .expect("normal nonzero manager probe");
        assert!(matches!(
            probe,
            SupervisorManagerOperability::Unavailable { reason }
                if reason.contains("process exited with")
        ));
    }

    #[test]
    fn bounded_probe_discards_manager_output_and_reports_only_status() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "printf 'unbounded-task-list-marker'; printf 'diagnostic-marker' >&2; exit 7",
        ]);
        let result = probe_supervisor_manager_bounded(&mut command, "test manager")
            .expect("normal nonzero manager probe");
        let SupervisorManagerOperability::Unavailable { reason } = result else {
            panic!("nonzero manager probe unexpectedly succeeded");
        };
        assert!(reason.contains("process exited with"));
        assert!(!reason.contains("task-list-marker"));
        assert!(!reason.contains("diagnostic-marker"));
    }

    #[test]
    fn missing_manager_is_unavailable_but_spawn_permission_failure_is_fatal() {
        let temp = tempfile::tempdir().expect("temporary manager probe root");
        let mut missing = Command::new(temp.path().join("missing-manager"));
        assert!(matches!(
            probe_supervisor_manager_bounded(&mut missing, "missing manager")
                .expect("missing executable is an expected unavailable manager"),
            SupervisorManagerOperability::Unavailable { reason }
                if reason.contains("missing manager is unavailable")
        ));

        let denied_path = temp.path().join("manager-without-execute-permission");
        fs::write(&denied_path, "#!/bin/sh\nexit 0\n").expect("write denied manager");
        fs::set_permissions(&denied_path, fs::Permissions::from_mode(0o600))
            .expect("remove manager execute permission");
        let mut denied = Command::new(&denied_path);
        let error = probe_supervisor_manager_bounded(&mut denied, "permission-denied manager")
            .expect_err("spawn permission failure must not degrade setup");
        assert!(
            format!("{error:#}").contains("start permission-denied manager operability probe"),
            "{error:#}"
        );
    }

    #[test]
    fn manager_probe_timeout_is_bounded_and_degrades_without_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 30"]);
        let started = Instant::now();
        let probe = probe_supervisor_manager_with_timeout(
            &mut command,
            "blocked manager",
            Duration::from_millis(25),
            Duration::from_millis(1),
        )
        .expect("manager timeout is an expected unavailable result");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(matches!(
            probe,
            SupervisorManagerOperability::Unavailable { reason }
                if reason.contains("timed out after 25 ms")
        ));
    }

    #[test]
    fn systemd_install_repairs_metadata_and_completes_restart_after_handoff() {
        let temp = tempfile::tempdir().expect("temporary systemd fixture");
        let systemctl = temp.path().join("systemctl");
        let log = temp.path().join("systemctl.log");
        let restarted = temp.path().join("restart-complete");
        fs::write(
            &systemctl,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$CTX_TEST_SYSTEMCTL_LOG\"\ncase \"$*\" in *restart*) sleep 0.05; : > \"$CTX_TEST_SYSTEMCTL_RESTARTED\" ;; esac\n",
        )
        .expect("write fake systemctl");
        fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o700))
            .expect("make fake systemctl executable");
        let manager_environment = SupervisorManagerEnvironment::new(BTreeMap::from([
            (
                OsString::from("PATH"),
                temp.path().as_os_str().to_os_string(),
            ),
            (
                OsString::from("CTX_TEST_SYSTEMCTL_LOG"),
                log.as_os_str().to_os_string(),
            ),
            (
                OsString::from("CTX_TEST_SYSTEMCTL_RESTARTED"),
                restarted.as_os_str().to_os_string(),
            ),
        ]));
        let unit = temp.path().join("ctx.service");
        let identity = SupervisorIdentity::new("ctx.service", unit.clone()).unwrap();
        let spec = SupervisorSpec::new(
            identity,
            "ctx test daemon",
            crate::supervisor_environment_path(temp.path()),
            crate::NormalizedLaunch::new(
                PathBuf::from("/opt/ctx/bin/ctx"),
                vec![OsString::from("daemon"), OsString::from("run")],
                BTreeMap::from([(OsString::from("HOME"), OsString::from("/home/tester"))]),
            ),
        )
        .unwrap();
        let migrations = Cell::new(0_u32);
        let migrate = |_: &Path| {
            migrations.set(migrations.get() + 1);
            Ok(())
        };

        install_systemd_supervisor(temp.path(), &spec, &manager_environment, &migrate).unwrap();
        let first_log = fs::read_to_string(&log).unwrap();
        assert!(first_log.contains("--user daemon-reload"));
        assert!(first_log.contains("--user enable ctx.service"));
        assert!(first_log.contains("--user restart ctx.service"));
        assert!(!first_log.contains("--user start ctx.service"));
        assert!(
            restarted.is_file(),
            "blocking restart must complete before return"
        );
        let inode = fs::metadata(&unit).unwrap().ino();

        fs::set_permissions(&unit, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(&log, "").unwrap();
        install_systemd_supervisor(temp.path(), &spec, &manager_environment, &migrate).unwrap();
        let second_log = fs::read_to_string(&log).unwrap();
        assert!(second_log.contains("daemon-reload"));
        assert!(second_log.contains("--user enable ctx.service"));
        assert!(second_log.contains("--user restart ctx.service"));
        let repaired = fs::metadata(&unit).unwrap();
        assert_eq!(repaired.ino(), inode);
        assert_eq!(repaired.permissions().mode() & 0o7777, 0o600);

        let alias = temp.path().join("ctx.service.alias");
        fs::hard_link(&unit, &alias).unwrap();
        fs::write(&log, "").unwrap();
        install_systemd_supervisor(temp.path(), &spec, &manager_environment, &migrate).unwrap();
        let replaced = fs::metadata(&unit).unwrap();
        assert_ne!(replaced.ino(), inode);
        assert_eq!(replaced.nlink(), 1);
        assert_eq!(fs::metadata(&alias).unwrap().ino(), inode);
        assert_eq!(migrations.get(), 3);
    }
}

#[cfg(test)]
mod manager_probe_command_tests {
    use super::*;

    #[test]
    fn launchd_probe_reads_only_the_gui_user_domain() {
        let environment = SupervisorManagerEnvironment::new(BTreeMap::new());
        let command = launchd_gui_user_domain_probe_command(&environment, "gui/501");
        assert_eq!(command.get_program(), "launchctl");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["print", "gui/501"]
                .iter()
                .map(OsStr::new)
                .collect::<Vec<_>>()
        );
    }
}
