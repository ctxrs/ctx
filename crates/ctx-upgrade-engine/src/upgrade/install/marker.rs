use std::{
    env, io,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::utc_now;
use serde_json::{json, Value};

use crate::install_marker::{
    active_install_attribution_from_value, is_staging_dogfood_marker, ActiveInstallAttribution,
};

use super::super::state::atomic_write_json;
use super::super::{platform_key, sha256_hex, UpgradePlan};
use super::lock::canonical_executable;
#[cfg(windows)]
use super::lock::canonical_recovery_executable;
use super::lock_fs::{read_stable_file, StableFileKind};

const MIN_INSTALL_ATTEMPT_ID_BODY_BYTES: usize = 8;
const MAX_INSTALL_ATTEMPT_ID_BODY_BYTES: usize = 128;
const MAX_INSTALL_MARKER_BYTES: u64 = 64 * 1024;
const MAX_MANAGED_BINARY_BYTES: u64 = 128 * 1024 * 1024;
const TEST_HARNESS_UPGRADE_TARGET_ENV: &str = "CTX_UPGRADE_TEST_TARGET";

#[derive(Debug, Clone)]
pub struct InstallMarker {
    pub install_path: PathBuf,
    pub platform: String,
    pub channel: String,
    pub version: String,
    pub sha256: String,
    pub staging_dogfood: bool,
}

/// A plan-time snapshot rechecked under the executable lock immediately before
/// publication.  Digesting both files makes a plan from one data root unable
/// to overwrite a replacement published by another root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::upgrade) struct InstallFingerprint {
    pub(in crate::upgrade) binary_sha256: String,
    pub(in crate::upgrade) marker_sha256: String,
}

pub(in crate::upgrade) fn install_fingerprint(path: &Path) -> Result<InstallFingerprint> {
    let binary = std::fs::read(path)
        .with_context(|| format!("read managed ctx executable {}", path.display()))?;
    let marker_path = install_marker_path(path);
    // An unmanaged installation has no marker beside its executable, so its
    // plan-time fingerprint digests an empty marker. Managed publication
    // paths still require a valid marker before any fingerprint comparison.
    let marker = match std::fs::read(&marker_path) {
        Ok(marker) => marker,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read ctx install marker {}", marker_path.display()))
        }
    };
    Ok(InstallFingerprint {
        binary_sha256: sha256_hex(&binary),
        marker_sha256: sha256_hex(&marker),
    })
}

#[derive(Debug)]
pub enum ManagedInstallMarker {
    Absent,
    Valid(InstallMarker),
    Invalid { reason: String },
}

pub fn managed_install_marker_for_current_exe() -> Result<ManagedInstallMarker> {
    classify_install_marker_for_current_exe(platform_key()?)
}

fn classify_install_marker_for_current_exe(platform: &str) -> Result<ManagedInstallMarker> {
    let path = current_install_path()?;
    Ok(classify_install_marker_at(&path, platform))
}

pub(in crate::upgrade) fn classify_install_marker_at(
    path: &Path,
    platform: &str,
) -> ManagedInstallMarker {
    let marker = match read_install_marker_at(path) {
        Ok(Some(marker)) => marker,
        Ok(None) => return ManagedInstallMarker::Absent,
        Err(error) => {
            return ManagedInstallMarker::Invalid {
                reason: invalid_install_marker_reason(&error),
            }
        }
    };
    match verify_install_marker(&marker, platform) {
        Ok(()) => ManagedInstallMarker::Valid(marker),
        Err(error) => ManagedInstallMarker::Invalid {
            reason: invalid_install_marker_reason(&error),
        },
    }
}

fn read_install_marker_at(path: &Path) -> Result<Option<InstallMarker>> {
    let marker_path = install_marker_path(path);
    let Some(bytes) = read_install_marker_bytes(&marker_path)? else {
        return Ok(None);
    };
    let value: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse ctx install marker {}", marker_path.display()))?;
    let manager = value
        .get("manager")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if manager != "ctx-hosted-installer" {
        return Err(anyhow!(
            "ctx install marker has unsupported manager: {manager}"
        ));
    }
    let install_path = value
        .get("install_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("ctx install marker missing install_path"))?;
    let canonical_install_path =
        canonical_executable(&install_path).context("canonicalize ctx install marker path")?;
    if canonical_install_path != path {
        return Err(anyhow!(
            "ctx install marker path mismatch: marker {}, running {}",
            install_path.display(),
            path.display()
        ));
    }
    Ok(Some(InstallMarker {
        install_path: canonical_install_path,
        platform: string_field(&value, "platform")?,
        channel: string_field(&value, "channel")?,
        version: string_field(&value, "version")?,
        sha256: string_field(&value, "sha256")?,
        staging_dogfood: is_staging_dogfood_marker(&value),
    }))
}

pub(in crate::upgrade) fn install_marker_for_plan(
    require_managed: bool,
    platform: &str,
    channel: &str,
    current_version: &str,
    warnings: &mut Vec<String>,
) -> Result<InstallMarker> {
    match classify_install_marker_for_current_exe(platform)? {
        ManagedInstallMarker::Valid(marker) => Ok(marker),
        ManagedInstallMarker::Absent if require_managed => Err(absent_install_marker_error()),
        ManagedInstallMarker::Absent => {
            warnings.push(absent_install_marker_error().to_string());
            fallback_install_marker(platform, channel, current_version)
        }
        ManagedInstallMarker::Invalid { reason } if require_managed => Err(anyhow!(reason)),
        ManagedInstallMarker::Invalid { reason } => {
            warnings.push(reason);
            fallback_install_marker(platform, channel, current_version)
        }
    }
}

pub(in crate::upgrade) fn absent_install_marker_error() -> anyhow::Error {
    anyhow!(
        "ctx is not installed by the hosted installer; {}",
        unmanaged_install_conversion_guidance()
    )
}

/// An installation is unmanaged when no install marker is plainly present
/// beside the executable (third-party packaging, source builds). A present
/// but invalid marker is a distinct inconsistent state and keeps the
/// fail-closed managed-install errors.
pub(in crate::upgrade) fn installation_is_unmanaged_at(path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(install_marker_path(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    )
}

/// Whether the running ctx is an unmanaged installation whose executable
/// directory the hosted installer never mutates. Resolution failures count
/// as managed so unexpected states stay fail-closed.
pub fn current_exe_is_unmanaged() -> bool {
    current_install_path()
        .map(|path| installation_is_unmanaged_at(&path))
        .unwrap_or(false)
}

#[cfg(windows)]
pub fn unmanaged_install_conversion_guidance() -> &'static str {
    "to enable managed upgrades, run ctx daemon disable --prepare-uninstall --format=json, then after a successful receipt move or remove this unmanaged executable and rerun irm https://ctx.rs/install.ps1 | iex (or choose a different empty BinDir); see ctx docs show unmanaged-installs"
}

#[cfg(not(windows))]
pub fn unmanaged_install_conversion_guidance() -> &'static str {
    "to enable managed upgrades, run ctx daemon disable --prepare-uninstall --format=json, then after a successful receipt move or remove this unmanaged executable and rerun curl -fsSL https://ctx.rs/install | sh (or choose a different empty binary directory); see ctx docs show unmanaged-installs"
}

#[cfg(windows)]
pub fn invalid_install_marker_recovery_guidance() -> &'static str {
    "run ctx daemon disable --prepare-uninstall --format=json, then after a successful receipt move or remove the executable and invalid marker and rerun irm https://ctx.rs/install.ps1 | iex (or choose a different empty BinDir); see ctx docs show unmanaged-installs"
}

#[cfg(not(windows))]
pub fn invalid_install_marker_recovery_guidance() -> &'static str {
    "run ctx daemon disable --prepare-uninstall --format=json, then after a successful receipt move or remove the executable and invalid marker and rerun curl -fsSL https://ctx.rs/install | sh (or choose a different empty binary directory); see ctx docs show unmanaged-installs"
}

fn invalid_install_marker_reason(error: &anyhow::Error) -> String {
    format!("{error:#}; {}", invalid_install_marker_recovery_guidance())
}

fn fallback_install_marker(
    platform: &str,
    channel: &str,
    current_version: &str,
) -> Result<InstallMarker> {
    let install_path = current_install_path()?;
    Ok(InstallMarker {
        sha256: current_binary_sha_at(&install_path).unwrap_or_default(),
        install_path,
        platform: platform.to_owned(),
        channel: channel.to_owned(),
        version: current_version.to_owned(),
        staging_dogfood: false,
    })
}

fn verify_install_marker(marker: &InstallMarker, platform: &str) -> Result<()> {
    if marker.platform != platform {
        return Err(anyhow!(
            "ctx install marker platform mismatch: marker {}, current {platform}",
            marker.platform
        ));
    }
    let actual = current_binary_sha_at(&marker.install_path)?;
    if !marker.sha256.eq_ignore_ascii_case(&actual) {
        return Err(anyhow!("ctx install marker hash mismatch"));
    }
    Ok(())
}

pub(super) fn write_install_marker_to(
    marker_path: &Path,
    plan: &UpgradePlan,
    install_attribution: Option<&ActiveInstallAttribution>,
) -> Result<()> {
    let installed_at = install_attribution
        .map(|attribution| attribution.installed_at)
        .unwrap_or_else(utc_now);
    let mut body = json!({
        "schema_version": 1,
        "manager": "ctx-hosted-installer",
        "install_path": plan.install_path,
        "platform": plan.platform,
        "channel": plan.channel,
        "version": plan.latest_version,
        "sha256": plan.artifact_sha256,
        "metadata_url": plan.metadata_url,
        "artifact_url": plan.artifact_url,
        "source_commit": plan.metadata.source_commit,
        "published_at": plan.metadata.published_at,
        "store_schema_version": plan.metadata.store_schema_version,
        "installed_at": installed_at,
    });
    if let Some(install_attribution) = install_attribution {
        if let Some(object) = body.as_object_mut() {
            object.insert(
                "install_attempt_id".to_owned(),
                Value::String(install_attribution.install_attempt_id.clone()),
            );
        }
    }
    atomic_write_json(marker_path, &body)
}

pub(super) fn existing_install_attribution(marker_path: &Path) -> Option<ActiveInstallAttribution> {
    read_install_marker_bytes(marker_path)
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .and_then(|value| active_install_attribution_from_value(&value, utc_now()))
}

pub fn is_valid_install_attempt_id(value: &str) -> bool {
    let Some(body) = value.strip_prefix("ia_") else {
        return false;
    };
    (MIN_INSTALL_ATTEMPT_ID_BODY_BYTES..=MAX_INSTALL_ATTEMPT_ID_BODY_BYTES).contains(&body.len())
        && body
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn string_field(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("ctx install marker missing {key}"))
}

fn read_install_marker_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    read_stable_file(
        path,
        "ctx install marker",
        MAX_INSTALL_MARKER_BYTES,
        StableFileKind::Data,
    )
}

pub(in crate::upgrade) fn install_marker_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("ctx"))
        .to_os_string();
    file_name.push(".install.json");
    path.with_file_name(file_name)
}

pub fn current_install_path() -> Result<PathBuf> {
    // Legacy transaction fixtures need a fake shell target whose version can
    // differ from this test runner. The hook name is explicitly test-only and
    // the entire branch is absent from release binaries.
    let path = current_executable_path()?;
    canonical_executable(&path).context("resolve canonical current ctx executable")
}

#[cfg(windows)]
pub(super) fn current_install_path_for_recovery() -> Result<PathBuf> {
    let path = current_executable_path()?;
    canonical_recovery_executable(&path).context("resolve current ctx recovery executable")
}

fn current_executable_path() -> Result<PathBuf> {
    if crate::upgrade::test_harness_enabled() {
        if let Some(path) = env::var_os(TEST_HARNESS_UPGRADE_TARGET_ENV) {
            return Ok(PathBuf::from(path));
        }
    }
    env::current_exe().context("resolve current ctx executable")
}

fn current_binary_sha_at(path: &Path) -> Result<String> {
    let bytes = read_stable_file(
        path,
        "managed ctx executable",
        MAX_MANAGED_BINARY_BYTES,
        StableFileKind::Executable,
    )?
    .ok_or_else(|| anyhow!("managed ctx executable is absent: {}", path.display()))?;
    Ok(sha256_hex(&bytes))
}

#[cfg(test)]
#[path = "lock_marker_tests.rs"]
mod tests;
