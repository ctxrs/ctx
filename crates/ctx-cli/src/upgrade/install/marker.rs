use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::utc_now;
use serde_json::{json, Value};

use super::super::state::{atomic_write_json, read_json_file};
use super::super::{platform_key, sha256_hex, UpgradePlan};

const MIN_INSTALL_ATTEMPT_ID_BODY_BYTES: usize = 8;
const MAX_INSTALL_ATTEMPT_ID_BODY_BYTES: usize = 128;

#[derive(Debug, Clone)]
pub(in crate::upgrade) struct InstallMarker {
    pub(in crate::upgrade) install_path: PathBuf,
    pub(in crate::upgrade) platform: String,
    pub(in crate::upgrade) channel: String,
    pub(in crate::upgrade) version: String,
    pub(in crate::upgrade) sha256: String,
}

fn read_install_marker_for_current_exe() -> Result<InstallMarker> {
    let path = current_install_path()?;
    let marker_path = install_marker_path(&path);
    let value = read_json_file(&marker_path)
        .ok_or_else(|| anyhow!("ctx is not installed by the hosted installer; reinstall with curl -fsSL https://ctx.rs/install | sh to enable managed upgrades"))?;
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
    if install_path != path {
        return Err(anyhow!(
            "ctx install marker path mismatch: marker {}, running {}",
            install_path.display(),
            path.display()
        ));
    }
    Ok(InstallMarker {
        install_path,
        platform: string_field(&value, "platform")?,
        channel: string_field(&value, "channel")?,
        version: string_field(&value, "version")?,
        sha256: string_field(&value, "sha256")?,
    })
}

pub(in crate::upgrade) fn read_verified_install_marker_for_current_exe() -> Result<InstallMarker> {
    let marker = read_install_marker_for_current_exe()?;
    verify_install_marker(&marker, platform_key()?)?;
    Ok(marker)
}

pub(in crate::upgrade) fn install_marker_for_plan(
    require_managed: bool,
    platform: &str,
    channel: &str,
    current_version: &str,
    warnings: &mut Vec<String>,
) -> Result<InstallMarker> {
    match read_install_marker_for_current_exe() {
        Ok(marker) => match verify_install_marker(&marker, platform) {
            Ok(()) => Ok(marker),
            Err(error) if require_managed => Err(error),
            Err(error) => {
                warnings.push(error.to_string());
                fallback_install_marker(platform, channel, current_version)
            }
        },
        Err(error) if require_managed => Err(error),
        Err(error) => {
            warnings.push(error.to_string());
            fallback_install_marker(platform, channel, current_version)
        }
    }
}

fn fallback_install_marker(
    platform: &str,
    channel: &str,
    current_version: &str,
) -> Result<InstallMarker> {
    Ok(InstallMarker {
        install_path: current_install_path()?,
        platform: platform.to_owned(),
        channel: channel.to_owned(),
        version: current_version.to_owned(),
        sha256: current_binary_sha().unwrap_or_default(),
    })
}

fn verify_install_marker(marker: &InstallMarker, platform: &str) -> Result<()> {
    if marker.platform != platform {
        return Err(anyhow!(
            "ctx install marker platform mismatch: marker {}, current {platform}",
            marker.platform
        ));
    }
    let actual = current_binary_sha()?;
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
    install_attempt_id: Option<&str>,
) -> Result<()> {
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
        "installed_at": utc_now(),
    });
    if let Some(install_attempt_id) = install_attempt_id {
        if let Some(object) = body.as_object_mut() {
            object.insert(
                "install_attempt_id".to_owned(),
                Value::String(install_attempt_id.to_owned()),
            );
        }
    }
    atomic_write_json(marker_path, &body)
}

pub(super) fn existing_install_attempt_id(marker_path: &Path) -> Option<String> {
    read_json_file(marker_path).and_then(|value| optional_install_attempt_id(&value))
}

fn optional_install_attempt_id(value: &Value) -> Option<String> {
    let id = value.get("install_attempt_id")?.as_str()?;
    is_valid_install_attempt_id(id).then(|| id.to_owned())
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

pub(super) fn install_marker_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("ctx");
    path.with_file_name(format!("{file_name}.install.json"))
}

pub(in crate::upgrade) fn current_install_path() -> Result<PathBuf> {
    env::var_os("CTX_UPGRADE_TARGET")
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(env::current_exe)
        .context("resolve current ctx executable")
}

fn current_binary_sha() -> Result<String> {
    let path = current_install_path()?;
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(sha256_hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::is_valid_install_attempt_id;

    #[test]
    fn install_attempt_id_matches_hosted_canonical_form() {
        assert!(is_valid_install_attempt_id("ia_12345678"));
        assert!(is_valid_install_attempt_id(&format!(
            "ia_{}",
            "a".repeat(128)
        )));
        for invalid in [
            "12345678",
            "ia_1234567",
            "ia_contains space",
            " ia_12345678",
            "ia_12345678\n",
        ] {
            assert!(
                !is_valid_install_attempt_id(invalid),
                "accepted {invalid:?}"
            );
        }
        assert!(!is_valid_install_attempt_id(&format!(
            "ia_{}",
            "a".repeat(129)
        )));
    }
}
