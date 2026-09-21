//! Installation-only retirement after the incoming legacy transaction ends.

use std::{fs, io, path::Path};

use anyhow::{anyhow, bail, Result};
use ctx_managed_pair_engine::{
    retire_managed_pair_files_under_installation_lock, MANAGED_PAIR_ENVELOPE_RELATIVE_PATH,
    MANAGED_PAIR_STATE_RELATIVE_PATH,
};
use serde_json::Value;

use super::{marker, ManagedInstallMarker};
use crate::upgrade::{managed_pair::install_root_for_executable, platform_key, state};

/// Explicit upgrade/installer completion hook. Never call from ordinary startup.
/// The caller holds the existing canonical installation lock for the whole call.
pub fn cleanup_legacy_managed_pair_under_installation_lock(install_path: &Path) -> Result<()> {
    let Some(root) = install_root_for_executable(install_path) else {
        return Ok(());
    };
    let marker_path = marker::install_marker_path(install_path);
    let Some(bytes) = marker::read_install_marker_bytes(&marker_path)? else {
        return Ok(()); // Unmanaged and package-manager installs have no authority here.
    };
    let mut value: Value = serde_json::from_slice(&bytes)?;
    // A coincidental libexec/ctx-pro name is not installation ownership.
    // Our deletion order removes that slot before either pair witness.
    let paths = [
        root.join(MANAGED_PAIR_STATE_RELATIVE_PATH),
        root.join(MANAGED_PAIR_ENVELOPE_RELATIVE_PATH),
    ];
    let mut present = value.get("managed_pair") == Some(&Value::Bool(true));
    for path in paths {
        match fs::symlink_metadata(&path) {
            Ok(_) => present = true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if !present {
        return Ok(());
    }
    let marker = match marker::classify_install_marker_at(install_path, platform_key()?) {
        ManagedInstallMarker::Valid(marker) => marker,
        ManagedInstallMarker::Invalid { reason } => bail!(reason),
        ManagedInstallMarker::Absent => bail!("managed install marker disappeared"),
    };
    let version = crate::upgrade::version::parse_semver(&marker.version)?;
    if version.major < 1 || (version.major == 1 && version.minor < 5) {
        return Ok(()); // Recovery can still finish a real pre-1.5 candidate.
    }
    super::ensure_hosted_transaction_inactive_under_installation_lock(install_path)?;
    state::ensure_legacy_pair_scheduler_terminal(install_path)?;
    retire_managed_pair_files_under_installation_lock(&root)?;
    if value.get("managed_pair").is_some() {
        if marker::read_install_marker_bytes(&marker_path)?.as_deref() != Some(bytes.as_slice()) {
            bail!("managed install marker changed during legacy cleanup");
        }
        value
            .as_object_mut()
            .ok_or_else(|| anyhow!("managed install marker is not an object"))?
            .remove("managed_pair");
        state::atomic_write_json(&marker_path, &value)?;
    }
    Ok(())
}
