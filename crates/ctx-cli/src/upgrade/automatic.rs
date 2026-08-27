use std::{ffi::OsString, path::Path};

use anyhow::{Context, Result};
use ctx_daemon_runtime::{spawn_detached, NormalizedLaunch};

use crate::config::AppConfig;

pub(crate) fn maybe_spawn_automatic(
    data_root: &Path,
    startup_config: &AppConfig,
    reload_config: bool,
) {
    let reloaded;
    let config = if reload_config {
        let Ok(config) = AppConfig::load(data_root) else {
            return;
        };
        reloaded = config;
        &reloaded
    } else {
        startup_config
    };
    if !super::automatic_upgrade_eligible_hint(config)
        || config.persistent_automatic_upgrade_driver_enabled()
    {
        return;
    }
    if !ctx_upgrade_engine::automatic_upgrade_check_due(config.upgrade.interval).unwrap_or(false) {
        return;
    }
    let _ = spawn_automatic_worker(data_root);
}

fn spawn_automatic_worker(data_root: &Path) -> Result<()> {
    let executable = ctx_upgrade_engine::current_install_path()
        .context("resolve installed ctx automatic-upgrade worker")?;
    let args = vec![
        OsString::from("--data-root"),
        data_root.as_os_str().to_os_string(),
        OsString::from("upgrade"),
        OsString::from("--automatic-worker"),
    ];

    #[cfg(windows)]
    let (args, startup_receipt) = {
        let mut args = args;
        let receipt = WindowsStartupReceipt::create()?;
        args.extend([
            OsString::from("--parent-pid"),
            OsString::from(std::process::id().to_string()),
            OsString::from("--startup-receipt"),
            OsString::from(receipt.name()),
        ]);
        (args, Some(receipt))
    };

    let launch = NormalizedLaunch::new(
        executable,
        args,
        crate::process_environment::sanitized_release_authority_environment(),
    );
    let _child = spawn_detached(launch).context("spawn detached automatic-upgrade worker")?;

    #[cfg(windows)]
    startup_receipt
        .expect("Windows automatic worker has a startup receipt")
        .wait()?;

    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn wait_for_invoking_parent(
    _parent_pid: Option<u32>,
    _startup_receipt: Option<&str>,
) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
pub(crate) fn wait_for_invoking_parent(
    parent_pid: Option<u32>,
    startup_receipt: Option<&str>,
) -> Result<()> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::{
            OpenEventW, OpenProcess, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
            PROCESS_SYNCHRONIZE,
        },
    };

    const PARENT_EXIT_TIMEOUT_MS: u32 = 30_000;

    let parent_pid = parent_pid.context("automatic worker missing --parent-pid")?;
    let receipt_name = startup_receipt.context("automatic worker missing --startup-receipt")?;
    let receipt_name = wide(receipt_name);
    let receipt = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, receipt_name.as_ptr()) };
    if receipt.is_null() {
        return Err(std::io::Error::last_os_error())
            .context("open automatic-worker startup receipt");
    }
    let parent = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, parent_pid) };
    if parent.is_null() {
        unsafe {
            CloseHandle(receipt);
        }
        return Err(std::io::Error::last_os_error()).context("open invoking ctx process");
    }
    if unsafe { SetEvent(receipt) } == 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            CloseHandle(parent);
            CloseHandle(receipt);
        }
        return Err(error).context("signal automatic-worker startup receipt");
    }
    unsafe {
        CloseHandle(receipt);
    }
    let wait = unsafe { WaitForSingleObject(parent, PARENT_EXIT_TIMEOUT_MS) };
    unsafe {
        CloseHandle(parent);
    }
    match wait {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => anyhow::bail!("timed out waiting for invoking ctx process to exit"),
        WAIT_FAILED => {
            Err(std::io::Error::last_os_error()).context("wait for invoking ctx process to exit")
        }
        other => anyhow::bail!("unexpected invoking-process wait result {other}"),
    }
}

#[cfg(windows)]
struct WindowsStartupReceipt {
    handle: windows_sys::Win32::Foundation::HANDLE,
    name: String,
}

#[cfg(windows)]
impl WindowsStartupReceipt {
    fn create() -> Result<Self> {
        use windows_sys::Win32::System::Threading::CreateEventW;

        let name = format!("Local\\ctx-auto-upgrade-{}", uuid::Uuid::new_v4().simple());
        let wide_name = wide(&name);
        let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, wide_name.as_ptr()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error())
                .context("create automatic-worker startup receipt");
        }
        Ok(Self { handle, name })
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn wait(self) -> Result<()> {
        use windows_sys::Win32::{
            Foundation::{WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
            System::Threading::WaitForSingleObject,
        };

        const STARTUP_TIMEOUT_MS: u32 = 2_000;
        match unsafe { WaitForSingleObject(self.handle, STARTUP_TIMEOUT_MS) } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => anyhow::bail!("automatic-upgrade worker startup timed out"),
            WAIT_FAILED => Err(std::io::Error::last_os_error())
                .context("wait for automatic-worker startup receipt"),
            other => anyhow::bail!("unexpected automatic-worker startup wait result {other}"),
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsStartupReceipt {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
