use std::{path::Path, process::Command};

use anyhow::Result;
use ctx_pro_host_protocol::{
    ProFilesystemLayout, CTX_PRO_DATA_ROOT_ENV, CTX_PRO_INSTALLATION_ID_ENV,
};

use super::support;

pub(super) fn configure_helper_environment(
    command: &mut Command,
    data_root: &Path,
    installation_id: &str,
    git_executable: &Path,
) -> Result<()> {
    let layout = ProFilesystemLayout::new(data_root);
    command
        .env_clear()
        .env("CTX_DATA_ROOT", data_root)
        .env(CTX_PRO_DATA_ROOT_ENV, layout.pro_root())
        .env(CTX_PRO_INSTALLATION_ID_ENV, installation_id)
        .env(support::GIT_EXECUTABLE_ENV, git_executable);

    #[cfg(windows)]
    command.env("SystemRoot", windows_system_root()?);

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    for (key, value) in secret_service_environment(|key| std::env::var_os(key)) {
        command.env(key, value);
    }
    Ok(())
}

#[cfg(windows)]
fn windows_system_root() -> Result<std::ffi::OsString> {
    windows_system_root_from(|buffer| unsafe {
        windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW(
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        )
    })
}

#[cfg(windows)]
fn windows_system_root_from(
    mut get_directory: impl FnMut(&mut [u16]) -> u32,
) -> Result<std::ffi::OsString> {
    use std::os::windows::ffi::OsStringExt as _;

    use anyhow::{anyhow, bail, Context as _};

    const INITIAL_CAPACITY: usize = 260;
    const MAX_WINDOWS_PATH_CODE_UNITS: usize = 32_768;

    let mut capacity = INITIAL_CAPACITY;
    loop {
        let mut buffer = vec![0_u16; capacity];
        let returned = get_directory(&mut buffer) as usize;
        if returned == 0 {
            return Err(std::io::Error::last_os_error())
                .context("helper_crashed: resolve Windows system root");
        }
        if returned >= capacity {
            capacity = returned
                .checked_add(1)
                .filter(|capacity| *capacity <= MAX_WINDOWS_PATH_CODE_UNITS)
                .ok_or_else(|| {
                    anyhow!("helper_crashed: Windows system root exceeds safe bounds")
                })?;
            continue;
        }
        if buffer[..returned].contains(&0) {
            bail!("helper_crashed: Windows system root contains an embedded terminator");
        }
        buffer.truncate(returned);
        let value = std::ffi::OsString::from_wide(&buffer);
        if !Path::new(&value).is_absolute() {
            bail!("helper_crashed: Windows system root is not absolute");
        }
        return Ok(value);
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn secret_service_environment(
    mut lookup: impl FnMut(&str) -> Option<std::ffi::OsString>,
) -> Vec<(&'static str, std::ffi::OsString)> {
    ["DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"]
        .into_iter()
        .filter_map(|key| {
            lookup(key)
                .filter(|value| !value.is_empty())
                .map(|value| (key, value))
        })
        .collect()
}

#[cfg(test)]
#[path = "client_environment_tests.rs"]
mod tests;
