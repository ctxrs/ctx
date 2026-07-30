#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use std::{env, fs, process::Command};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
#[cfg(any(test, windows))]
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ctx_history_core::managed_data_root;
use serde_json::{json, Value};

use crate::compact_json;
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
use crate::identity;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use super::{
    paths_status::{daemon_lock_is_active, pid_from_lock_json, read_pid_lock_json},
    query_service::daemon_source_refresh_request,
};

mod coordination;
mod environment;
mod report;
mod state;
#[cfg(test)]
mod tests;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod unsupported;
#[cfg(any(test, windows))]
mod windows;

use coordination::SupervisorInstallationLock;
#[cfg(any(test, target_os = "macos"))]
use environment::launch_agent_plist;
#[cfg(target_os = "linux")]
use environment::{linux_systemd_unit, linux_systemd_unit_with_environment};
use environment::{
    supervisor_environment_contract_report, supervisor_environment_snapshot,
    SupervisorEnvironmentSnapshot,
};
#[cfg(any(test, windows))]
use environment::{validated_supervisor_artifact_path, validated_supervisor_artifact_text};
pub(super) use report::daemon_supervisor_report;
#[cfg(any(test, target_os = "freebsd"))]
use report::freebsd_supervisor_authority_blocker;
use report::native_supervisor_product_authority_blocker;
#[cfg(test)]
use report::revalidated_supervisor_report_with;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use state::write_atomic_file;
use state::{
    native_supervisor_artifact_path, native_supervisor_kind, native_supervisor_limitation,
    stored_supervisor_report, write_installed_receipt, write_supervisor_receipt,
    write_supervisor_receipt_with_environment_snapshot, SupervisorReceipt,
};
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use unsupported::*;
#[cfg(any(test, windows))]
use windows::*;

const SUPERVISOR_HANDOFF_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(any(test, target_os = "linux", target_os = "macos", windows))]
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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum DaemonSupervisorUpgradeResume {
    Native,
    Fallback,
}

trait NativeSupervisorBackend: Sync {
    fn artifact_path(&self, data_root: &Path) -> Result<Option<PathBuf>>;
    fn install(
        &self,
        data_root: &Path,
        executable: &Path,
        environment: &SupervisorEnvironmentSnapshot,
    ) -> Result<PathBuf>;
    fn disable(&self, data_root: &Path) -> Result<Option<PathBuf>>;
    fn verify_registration(&self, data_root: &Path, executable: &Path) -> Result<()>;
    fn verify_live_owner(&self, data_root: &Path, executable: &Path) -> Result<u32>;
    fn start(&self, data_root: &Path) -> Result<()>;
}

struct PlatformNativeSupervisor;

impl NativeSupervisorBackend for PlatformNativeSupervisor {
    fn artifact_path(&self, data_root: &Path) -> Result<Option<PathBuf>> {
        native_supervisor_artifact_path(data_root)
    }

    fn install(
        &self,
        data_root: &Path,
        executable: &Path,
        environment: &SupervisorEnvironmentSnapshot,
    ) -> Result<PathBuf> {
        install_native_supervisor(data_root, executable, environment)
    }

    fn disable(&self, data_root: &Path) -> Result<Option<PathBuf>> {
        disable_native_supervisor(data_root)
    }

    fn verify_registration(&self, data_root: &Path, executable: &Path) -> Result<()> {
        verify_native_supervisor_registration(data_root, executable)
    }

    fn verify_live_owner(&self, data_root: &Path, executable: &Path) -> Result<u32> {
        verify_native_supervisor(data_root, executable)
    }

    fn start(&self, data_root: &Path) -> Result<()> {
        start_native_supervisor(data_root)
    }
}

pub(super) fn ensure_daemon_supervisor(data_root: &Path) -> Result<DaemonSupervisorStart> {
    let Some(executable) = safely_supported_managed_install(data_root)? else {
        let _installation_lock = SupervisorInstallationLock::acquire(data_root)?;
        write_supervisor_receipt(
            data_root,
            &SupervisorReceipt {
                kind: "cli_self_heal".to_owned(),
                status: "fallback",
                autostart_supported: false,
                restart_supported: false,
                registration_verified: false,
                live_owner_verified: false,
                owner_pid: None,
                artifact_path: None,
                executable_path: None,
                limitation: Some(
                    "native per-user restart registration requires the hosted installer and the default data root"
                        .to_owned(),
                ),
                last_error: None,
            },
        )?;
        return Ok(DaemonSupervisorStart::Fallback);
    };
    super::daemon_autostart::handoff_mismatched_daemon_owner(data_root, &executable)
        .context("replace daemon ownership held by a different ctx binary image")?;
    ensure_native_supervisor_with(data_root, &executable, &PlatformNativeSupervisor)
}

fn ensure_native_supervisor_with(
    data_root: &Path,
    executable: &Path,
    backend: &dyn NativeSupervisorBackend,
) -> Result<DaemonSupervisorStart> {
    let _installation_lock = SupervisorInstallationLock::acquire(data_root)?;
    let artifact = backend.artifact_path(data_root)?;

    if backend.verify_registration(data_root, executable).is_ok() {
        match backend.verify_live_owner(data_root, executable) {
            Ok(owner_pid) => {
                write_installed_receipt(data_root, executable, artifact, owner_pid, None)?;
                return Ok(DaemonSupervisorStart::Native);
            }
            Err(initial_live_error) => {
                let recovery = backend
                    .start(data_root)
                    .and_then(|()| wait_for_native_live_owner(data_root, executable, backend));
                match recovery {
                    Ok(owner_pid) => {
                        write_installed_receipt(data_root, executable, artifact, owner_pid, None)?;
                        return Ok(DaemonSupervisorStart::Native);
                    }
                    Err(recovery_error) => {
                        write_supervisor_receipt(
                            data_root,
                            &SupervisorReceipt {
                                kind: native_supervisor_kind().to_owned(),
                                status: "registered_not_running",
                                autostart_supported: true,
                                restart_supported: true,
                                registration_verified: true,
                                live_owner_verified: false,
                                owner_pid: None,
                                artifact_path: artifact,
                                executable_path: Some(executable.to_path_buf()),
                                limitation: Some(
                                    "native registration is valid but has no identity-verified live daemon owner; retrieval commands retain CLI self-healing"
                                        .to_owned(),
                                ),
                                last_error: Some(format!(
                                    "initial live check: {initial_live_error:#}; recovery: {recovery_error:#}"
                                )),
                            },
                        )?;
                        return Ok(DaemonSupervisorStart::Fallback);
                    }
                }
            }
        }
    }

    let installed_environment = supervisor_environment_snapshot()
        .context("capture native supervisor environment snapshot")?;
    let installation = backend
        .install(data_root, executable, &installed_environment)
        .and_then(|installed_artifact| {
            wait_for_native_live_owner(data_root, executable, backend)
                .map(|owner_pid| (installed_artifact, owner_pid))
        });
    match installation {
        Ok((installed_artifact, owner_pid)) => {
            write_installed_receipt(
                data_root,
                executable,
                Some(installed_artifact),
                owner_pid,
                Some(installed_environment.contract_report()),
            )?;
            Ok(DaemonSupervisorStart::Native)
        }
        Err(error) if backend.verify_registration(data_root, executable).is_ok() => {
            let recovery = backend
                .verify_live_owner(data_root, executable)
                .or_else(|_| {
                    backend.start(data_root)?;
                    wait_for_native_live_owner(data_root, executable, backend)
                });
            match recovery {
                Ok(owner_pid) => {
                    write_installed_receipt(
                        data_root,
                        executable,
                        artifact,
                        owner_pid,
                        Some(installed_environment.contract_report()),
                    )?;
                    Ok(DaemonSupervisorStart::Native)
                }
                Err(recovery_error) => {
                    write_supervisor_receipt_with_environment_snapshot(
                        data_root,
                        &SupervisorReceipt {
                            kind: native_supervisor_kind().to_owned(),
                            status: "registered_not_running",
                            autostart_supported: true,
                            restart_supported: true,
                            registration_verified: true,
                            live_owner_verified: false,
                            owner_pid: None,
                            artifact_path: artifact,
                            executable_path: Some(executable.to_path_buf()),
                            limitation: Some(
                                "native registration survived installation recovery but has no identity-verified live daemon owner; retrieval commands retain CLI self-healing"
                                    .to_owned(),
                            ),
                            last_error: Some(format!(
                                "installation: {error:#}; recovery: {recovery_error:#}"
                            )),
                        },
                        Some(installed_environment.contract_report()),
                    )?;
                    Ok(DaemonSupervisorStart::Fallback)
                }
            }
        }
        Err(error) => {
            if let Err(cleanup_error) = backend.disable(data_root) {
                write_supervisor_receipt(
                    data_root,
                    &SupervisorReceipt {
                        kind: native_supervisor_kind().to_owned(),
                        status: "install_cleanup_failed",
                        autostart_supported: false,
                        restart_supported: false,
                        registration_verified: false,
                        live_owner_verified: false,
                        owner_pid: None,
                        artifact_path: backend.artifact_path(data_root)?,
                        executable_path: Some(executable.to_path_buf()),
                        limitation: Some(
                            "native registration failed and its partial state could not be removed"
                                .to_owned(),
                        ),
                        last_error: Some(format!("{error:#}; cleanup: {cleanup_error:#}")),
                    },
                )?;
                return Err(error.context(format!(
                    "also failed to remove partial native supervisor state: {cleanup_error:#}"
                )));
            }
            let authority_blocker = native_supervisor_product_authority_blocker();
            write_supervisor_receipt(
                data_root,
                &SupervisorReceipt {
                    kind: if authority_blocker {
                        native_supervisor_kind()
                    } else {
                        "cli_self_heal"
                    }
                    .to_owned(),
                    status: if authority_blocker {
                        "degraded"
                    } else {
                        "install_failed"
                    },
                    autostart_supported: false,
                    restart_supported: false,
                    registration_verified: false,
                    live_owner_verified: false,
                    owner_pid: None,
                    artifact_path: None,
                    executable_path: Some(executable.to_path_buf()),
                    limitation: Some(native_supervisor_limitation().to_owned()),
                    last_error: Some(format!("{error:#}")),
                },
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
    let _installation_lock = SupervisorInstallationLock::acquire(data_root)?;
    let backend = PlatformNativeSupervisor;
    let current = stored_supervisor_report(data_root);
    if !is_canonical_managed_data_root(data_root)? {
        return write_supervisor_receipt(
            data_root,
            &SupervisorReceipt {
                kind: "cli_self_heal".to_owned(),
                status: "disabled",
                autostart_supported: false,
                restart_supported: false,
                registration_verified: false,
                live_owner_verified: false,
                owner_pid: None,
                artifact_path: None,
                executable_path: None,
                limitation: Some(
                    "custom data roots never own or alter the singleton native supervisor"
                        .to_owned(),
                ),
                last_error: None,
            },
        );
    }
    let managed_executable = safely_supported_managed_install(data_root)?;
    let executable = current
        .get("executable_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| managed_executable.clone());
    let receipt_native =
        current.get("kind").and_then(Value::as_str) == Some(native_supervisor_kind());
    let native_candidate = receipt_native || managed_executable.is_some();
    let artifact = if native_candidate {
        backend.artifact_path(data_root)?
    } else {
        None
    };
    let artifact_exists = native_candidate && artifact.as_deref().is_some_and(Path::exists);
    let registration_exists = native_candidate
        && executable
            .as_deref()
            .is_some_and(|executable| backend.verify_registration(data_root, executable).is_ok());
    if !native_candidate || (!artifact_exists && !registration_exists) {
        return write_supervisor_receipt(
            data_root,
            &SupervisorReceipt {
                kind: current
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("cli_self_heal")
                    .to_owned(),
                status: "disabled",
                autostart_supported: false,
                restart_supported: false,
                registration_verified: false,
                live_owner_verified: false,
                owner_pid: None,
                artifact_path: None,
                executable_path: executable,
                limitation: None,
                last_error: None,
            },
        );
    }
    let result = backend.disable(data_root);
    match result {
        Ok(artifact) => write_supervisor_receipt(
            data_root,
            &SupervisorReceipt {
                kind: native_supervisor_kind().to_owned(),
                status: "disabled",
                autostart_supported: false,
                restart_supported: false,
                registration_verified: false,
                live_owner_verified: false,
                owner_pid: None,
                artifact_path: artifact,
                executable_path: executable,
                limitation: None,
                last_error: None,
            },
        ),
        Err(error) => {
            write_supervisor_receipt(
                data_root,
                &SupervisorReceipt {
                    kind: native_supervisor_kind().to_owned(),
                    status: "disable_failed",
                    autostart_supported: false,
                    restart_supported: false,
                    registration_verified: registration_exists,
                    live_owner_verified: false,
                    owner_pid: None,
                    artifact_path: artifact,
                    executable_path: executable,
                    limitation: Some(
                        "native per-user registration could not be fully removed".to_owned(),
                    ),
                    last_error: Some(format!("{error:#}")),
                },
            )?;
            Err(error)
        }
    }
}

fn wait_for_native_live_owner(
    data_root: &Path,
    executable: &Path,
    backend: &dyn NativeSupervisorBackend,
) -> Result<u32> {
    let deadline = Instant::now() + SUPERVISOR_HANDOFF_TIMEOUT;
    loop {
        match backend.verify_live_owner(data_root, executable) {
            Ok(owner_pid) => return Ok(owner_pid),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "native supervisor did not start daemon lifecycle ownership"
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub(super) fn resume_daemon_supervisor_after_upgrade(
    data_root: &Path,
    executable: &Path,
    release_upgrade_fence: impl FnOnce() -> Result<()>,
) -> Result<DaemonSupervisorUpgradeResume> {
    resume_daemon_supervisor_after_upgrade_with(
        data_root,
        executable,
        &PlatformNativeSupervisor,
        release_upgrade_fence,
    )
}

fn resume_daemon_supervisor_after_upgrade_with(
    data_root: &Path,
    executable: &Path,
    backend: &dyn NativeSupervisorBackend,
    release_upgrade_fence: impl FnOnce() -> Result<()>,
) -> Result<DaemonSupervisorUpgradeResume> {
    let _installation_lock = SupervisorInstallationLock::acquire(data_root)?;
    if backend.verify_registration(data_root, executable).is_err() {
        return Ok(DaemonSupervisorUpgradeResume::Fallback);
    }

    release_upgrade_fence()?;
    let owner = backend
        .verify_live_owner(data_root, executable)
        .or_else(|_| {
            backend.start(data_root)?;
            wait_for_native_live_owner(data_root, executable, backend)
        });
    match owner {
        Ok(owner_pid) => {
            write_installed_receipt(
                data_root,
                executable,
                backend.artifact_path(data_root)?,
                owner_pid,
                None,
            )?;
            Ok(DaemonSupervisorUpgradeResume::Native)
        }
        Err(error) => {
            write_supervisor_receipt(
                data_root,
                &SupervisorReceipt {
                    kind: native_supervisor_kind().to_owned(),
                    status: "registered_not_running",
                    autostart_supported: true,
                    restart_supported: true,
                    registration_verified: true,
                    live_owner_verified: false,
                    owner_pid: None,
                    artifact_path: backend.artifact_path(data_root)?,
                    executable_path: Some(executable.to_path_buf()),
                    limitation: Some(
                        "the upgrade fence was released to a valid native registration, but the manager did not establish identity-verified daemon ownership; the durable restart request remains available for CLI self-healing"
                            .to_owned(),
                    ),
                    last_error: Some(format!("{error:#}")),
                },
            )?;
            Err(error).context("return upgraded daemon lifecycle ownership to native supervisor")
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
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
    if !super::paths_status::daemon_lock_binary_identity_matches(&lock, executable)? {
        return Err(anyhow!(
            "native supervisor daemon lock identifies a different ctx binary image"
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

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
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

fn safely_supported_managed_install(data_root: &Path) -> Result<Option<PathBuf>> {
    if !is_canonical_managed_data_root(data_root)? {
        return Ok(None);
    }
    crate::upgrade::managed_install_executable()
}

fn is_canonical_managed_data_root(data_root: &Path) -> Result<bool> {
    let managed_root = managed_data_root().context("resolve canonical managed ctx data root")?;
    Ok(data_root == managed_root)
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
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
fn install_native_supervisor(
    data_root: &Path,
    executable: &Path,
    environment: &SupervisorEnvironmentSnapshot,
) -> Result<PathBuf> {
    let path = linux_systemd_unit_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("systemd user unit has no parent directory"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create systemd user unit directory {}", parent.display()))?;
    let definition = linux_systemd_unit_with_environment(executable, data_root, environment)?;
    write_atomic_file(&path, definition.as_bytes())?;
    systemctl_user(["daemon-reload"])?;
    systemctl_user(["enable", "ctx.service"])?;
    migrate_existing_daemon_to_supervisor(data_root)?;
    start_native_supervisor(data_root)?;
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
fn verify_native_supervisor_registration(data_root: &Path, executable: &Path) -> Result<()> {
    let path = linux_systemd_unit_path()?;
    let registered = fs::read_to_string(&path)
        .with_context(|| format!("read systemd user unit {}", path.display()))?;
    if registered != linux_systemd_unit(executable, data_root)? {
        return Err(anyhow!(
            "systemd user service registration does not match the maintained definition"
        ));
    }
    let enabled = systemctl_user_capture(["is-enabled", "ctx.service"])?;
    if !enabled.status.success() || String::from_utf8_lossy(&enabled.stdout).trim() != "enabled" {
        return Err(anyhow!("systemd user service is not durably enabled"));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_native_supervisor(data_root: &Path, executable: &Path) -> Result<u32> {
    verify_native_supervisor_registration(data_root, executable)?;
    let active = systemctl_user_capture(["is-active", "ctx.service"])?;
    if !active.status.success() || String::from_utf8_lossy(&active.stdout).trim() != "active" {
        return Err(anyhow!("systemd user service is not active"));
    }
    let pid =
        systemctl_user_capture(["show", "ctx.service", "--property=MainPID", "--value"])?.stdout;
    let pid = systemd_main_pid(&pid)?;
    verify_daemon_owner_identity(data_root, executable, Some(pid))
}

#[cfg(target_os = "linux")]
fn start_native_supervisor(_data_root: &Path) -> Result<()> {
    systemctl_user(["start", "ctx.service"])
}

#[cfg(any(test, target_os = "linux"))]
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
fn install_native_supervisor(
    data_root: &Path,
    executable: &Path,
    environment: &SupervisorEnvironmentSnapshot,
) -> Result<PathBuf> {
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
    let definition =
        environment::launch_agent_plist_with_environment(executable, data_root, environment)?;
    write_atomic_file(&path, definition.as_bytes())?;
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
    start_native_supervisor(data_root)?;
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

#[cfg(target_os = "macos")]
fn launchctl_print(domain: &str) -> Result<std::process::Output> {
    let mut print = supervisor_command("launchctl");
    print.args(["print", &format!("{domain}/rs.ctx.daemon")]);
    supervisor_output(&mut print).context("run launchctl print in GUI domain")
}

#[cfg(any(test, target_os = "macos"))]
fn launchctl_print_pid(output: &str) -> Option<u32> {
    output.lines().find_map(|line| {
        let (key, value) = line.trim().split_once('=')?;
        (key.trim() == "pid")
            .then(|| value.trim().parse::<u32>().ok())
            .flatten()
    })
}

#[cfg(target_os = "macos")]
fn verify_native_supervisor_registration(data_root: &Path, executable: &Path) -> Result<()> {
    let path = native_supervisor_artifact_path(data_root)?
        .ok_or_else(|| anyhow!("LaunchAgent artifact path is unavailable"))?;
    let registered = fs::read_to_string(&path)
        .with_context(|| format!("read LaunchAgent {}", path.display()))?;
    if registered != launch_agent_plist(executable, data_root)? {
        return Err(anyhow!(
            "LaunchAgent registration does not match the maintained definition"
        ));
    }
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let output = launchctl_print(&domain)?;
    if !output.status.success() {
        return Err(anyhow!(
            "LaunchAgent is not registered in the current GUI login domain: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_native_supervisor(data_root: &Path, executable: &Path) -> Result<u32> {
    verify_native_supervisor_registration(data_root, executable)?;
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let output = launchctl_print(&domain)?;
    let pid = launchctl_print_pid(&String::from_utf8_lossy(&output))
        .ok_or_else(|| anyhow!("LaunchAgent GUI registration has no live process identity"))?;
    verify_daemon_owner_identity(data_root, executable, Some(pid))
}

#[cfg(target_os = "macos")]
fn start_native_supervisor(_data_root: &Path) -> Result<()> {
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let mut kickstart = supervisor_command("launchctl");
    kickstart.args(["kickstart", "-k", &format!("{domain}/rs.ctx.daemon")]);
    command_success(&mut kickstart, "launchctl kickstart")
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

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn supervisor_command(program: &str) -> Command {
    let inherited = SUPERVISOR_ENV_ALLOWLIST
        .iter()
        .filter_map(|name| env::var_os(name).map(|value| (*name, value)))
        .collect::<Vec<_>>();
    let mut command = Command::new(program);
    command.env_clear().envs(inherited);
    command
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn supervisor_output(command: &mut Command) -> std::io::Result<std::process::Output> {
    crate::process_environment::sanitize_release_authority_env(command);
    command.output()
}

#[cfg(any(test, target_os = "macos", windows))]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
