use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::utc_now;
use serde_json::{json, Value};

use crate::install_marker::{active_install_attribution_from_value, ActiveInstallAttribution};

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
#[cfg(debug_assertions)]
const DEBUG_UPGRADE_TARGET_ENV: &str = "CTX_UPGRADE_TEST_TARGET";

#[derive(Debug, Clone)]
pub(in crate::upgrade) struct InstallMarker {
    pub(in crate::upgrade) install_path: PathBuf,
    pub(in crate::upgrade) platform: String,
    pub(in crate::upgrade) channel: String,
    pub(in crate::upgrade) version: String,
    pub(in crate::upgrade) sha256: String,
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
    let marker = std::fs::read(&marker_path)
        .with_context(|| format!("read ctx install marker {}", marker_path.display()))?;
    Ok(InstallFingerprint {
        binary_sha256: sha256_hex(&binary),
        marker_sha256: sha256_hex(&marker),
    })
}

#[derive(Debug)]
pub(in crate::upgrade) enum ManagedInstallMarker {
    Absent,
    Valid(InstallMarker),
    Invalid { reason: String },
}

pub(in crate::upgrade) fn managed_install_marker_for_current_exe() -> Result<ManagedInstallMarker> {
    classify_install_marker_for_current_exe(platform_key()?)
}

fn classify_install_marker_for_current_exe(platform: &str) -> Result<ManagedInstallMarker> {
    let path = current_install_path()?;
    Ok(classify_install_marker_at(&path, platform))
}

fn classify_install_marker_at(path: &Path, platform: &str) -> ManagedInstallMarker {
    let marker = match read_install_marker_at(path) {
        Ok(Some(marker)) => marker,
        Ok(None) => return ManagedInstallMarker::Absent,
        Err(error) => {
            return ManagedInstallMarker::Invalid {
                reason: format!("{error:#}"),
            }
        }
    };
    match verify_install_marker(&marker, platform) {
        Ok(()) => ManagedInstallMarker::Valid(marker),
        Err(error) => ManagedInstallMarker::Invalid {
            reason: format!("{error:#}"),
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

fn absent_install_marker_error() -> anyhow::Error {
    anyhow!("ctx is not installed by the hosted installer; reinstall with curl -fsSL https://ctx.rs/install | sh to enable managed upgrades")
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
        return Err(anyhow!(
            "ctx install marker hash mismatch; reinstall with curl -fsSL https://ctx.rs/install | sh"
        ));
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

pub(crate) fn is_valid_install_attempt_id(value: &str) -> bool {
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

pub(super) fn install_marker_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("ctx"))
        .to_os_string();
    file_name.push(".install.json");
    path.with_file_name(file_name)
}

pub(in crate::upgrade) fn current_install_path() -> Result<PathBuf> {
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
    #[cfg(debug_assertions)]
    return env::var_os(DEBUG_UPGRADE_TARGET_ENV)
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(env::current_exe)
        .context("resolve current ctx executable");
    #[cfg(not(debug_assertions))]
    return env::current_exe().context("resolve current ctx executable");
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
