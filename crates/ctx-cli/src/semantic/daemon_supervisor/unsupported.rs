use super::*;

#[cfg(target_os = "freebsd")]
pub(super) fn install_native_supervisor(
    _data_root: &Path,
    _executable: &Path,
    _environment: &SupervisorEnvironmentSnapshot,
) -> Result<PathBuf> {
    Err(anyhow!(native_supervisor_limitation()))
}

#[cfg(target_os = "freebsd")]
pub(super) fn disable_native_supervisor(_data_root: &Path) -> Result<Option<PathBuf>> {
    Ok(None)
}

#[cfg(target_os = "freebsd")]
pub(super) fn verify_native_supervisor_registration(
    _data_root: &Path,
    _executable: &Path,
) -> Result<()> {
    Err(anyhow!(native_supervisor_limitation()))
}

#[cfg(target_os = "freebsd")]
pub(super) fn verify_native_supervisor(_data_root: &Path, _executable: &Path) -> Result<u32> {
    Err(anyhow!(native_supervisor_limitation()))
}

#[cfg(target_os = "freebsd")]
pub(super) fn start_native_supervisor(_data_root: &Path) -> Result<()> {
    Err(anyhow!(native_supervisor_limitation()))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    windows
)))]
pub(super) fn install_native_supervisor(
    _data_root: &Path,
    _executable: &Path,
    _environment: &SupervisorEnvironmentSnapshot,
) -> Result<PathBuf> {
    Err(anyhow!(native_supervisor_limitation()))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    windows
)))]
pub(super) fn disable_native_supervisor(_data_root: &Path) -> Result<Option<PathBuf>> {
    Ok(None)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    windows
)))]
pub(super) fn verify_native_supervisor_registration(
    _data_root: &Path,
    _executable: &Path,
) -> Result<()> {
    Err(anyhow!(native_supervisor_limitation()))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    windows
)))]
pub(super) fn verify_native_supervisor(_data_root: &Path, _executable: &Path) -> Result<u32> {
    Err(anyhow!(native_supervisor_limitation()))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    windows
)))]
pub(super) fn start_native_supervisor(_data_root: &Path) -> Result<()> {
    Err(anyhow!(native_supervisor_limitation()))
}
