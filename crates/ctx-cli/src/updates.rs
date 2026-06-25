use std::{
    env, fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{config::AppConfig, net};

const UPDATE_STATE_FILE: &str = "update-state.json";

#[derive(Debug, Clone)]
pub struct UpdateOptions {
    pub apply: bool,
    pub check_only: bool,
    pub force: bool,
    pub quiet: bool,
}

#[derive(Debug, Clone)]
pub struct UpdateOutcome {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub channel: String,
    pub manifest_url: String,
    pub platform: String,
    pub update_available: bool,
    pub action: &'static str,
    pub applied: bool,
    pub artifact_url: Option<String>,
    pub install_path: Option<PathBuf>,
    pub message: String,
}

impl UpdateOutcome {
    pub fn json(&self) -> Value {
        json!({
            "schema_version": 1,
            "current_version": self.current_version,
            "latest_version": self.latest_version,
            "channel": self.channel,
            "manifest_url": self.manifest_url,
            "platform": self.platform,
            "update_available": self.update_available,
            "action": self.action,
            "applied": self.applied,
            "artifact_url": self.artifact_url,
            "install_path": self.install_path,
            "message": self.message,
        })
    }
}

#[derive(Debug, Clone)]
struct Artifact {
    url: String,
    sha256: String,
    bytes: Option<u64>,
}

pub fn maybe_auto_update(data_root: &Path, config: &AppConfig, json_output: bool) {
    if !config.updates.auto_update || json_output || env_flag("CTX_DISABLE_AUTO_UPDATE") {
        return;
    }
    if !should_check_now(data_root, config.updates.check_interval) {
        return;
    }
    let options = UpdateOptions {
        apply: true,
        check_only: false,
        force: false,
        quiet: true,
    };
    match check_or_apply_update(data_root, config, options) {
        Ok(outcome) => {
            let _ = write_update_state(data_root, &outcome);
            if outcome.applied || outcome.update_available {
                eprintln!("{}", outcome.message);
            }
        }
        Err(err) => {
            let _ = write_update_state_error(data_root, &err.to_string());
            if env::var_os("CTX_UPDATE_DEBUG").is_some() {
                eprintln!("ctx update check failed: {err:#}");
            }
        }
    }
}

pub fn check_or_apply_update(
    data_root: &Path,
    config: &AppConfig,
    options: UpdateOptions,
) -> Result<UpdateOutcome> {
    fs::create_dir_all(data_root)?;
    let current_version = env!("CARGO_PKG_VERSION").to_owned();
    let channel = config.updates.channel.clone();
    let platform = platform_key();
    let manifest_url = manifest_url(config);
    let manifest_bytes = net::get_bytes(&manifest_url)?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parse update manifest {manifest_url}"))?;
    let latest_version = manifest_version(&manifest);
    let update_available = options.force
        || latest_version
            .as_deref()
            .is_some_and(|latest| version_gt(latest, &current_version));

    if !update_available {
        let outcome = UpdateOutcome {
            current_version,
            latest_version,
            channel,
            manifest_url,
            platform,
            update_available: false,
            action: "none",
            applied: false,
            artifact_url: None,
            install_path: None,
            message: "ctx is up to date".to_owned(),
        };
        write_update_state(data_root, &outcome)?;
        return Ok(outcome);
    }

    let artifact = resolve_artifact(&manifest, &platform, &manifest_url)?;
    let should_apply = options.apply || (config.updates.auto_update && !options.check_only);
    if !should_apply {
        let latest = latest_version
            .clone()
            .unwrap_or_else(|| "unknown".to_owned());
        let outcome = UpdateOutcome {
            current_version,
            latest_version,
            channel,
            manifest_url,
            platform,
            update_available: true,
            action: "check_only",
            applied: false,
            artifact_url: Some(artifact.url),
            install_path: None,
            message: format!("ctx {latest} is available; run `ctx update --apply` to install"),
        };
        write_update_state(data_root, &outcome)?;
        return Ok(outcome);
    }

    if is_dev_binary()
        && !env_flag("CTX_ALLOW_DEV_AUTO_UPDATE")
        && env::var_os("CTX_UPDATE_TARGET").is_none()
    {
        let latest = latest_version
            .clone()
            .unwrap_or_else(|| "unknown".to_owned());
        let outcome = UpdateOutcome {
            current_version,
            latest_version,
            channel,
            manifest_url,
            platform,
            update_available: true,
            action: "skipped_dev_binary",
            applied: false,
            artifact_url: Some(artifact.url),
            install_path: None,
            message: format!("ctx {latest} is available; dev binary auto-update skipped"),
        };
        write_update_state(data_root, &outcome)?;
        return Ok(outcome);
    }

    let artifact_bytes = net::get_bytes(&artifact.url)?;
    if let Some(expected_bytes) = artifact.bytes {
        if artifact_bytes.len() as u64 != expected_bytes {
            return Err(anyhow!(
                "downloaded artifact size mismatch: expected {expected_bytes}, got {}",
                artifact_bytes.len()
            ));
        }
    }
    verify_sha256(&artifact_bytes, &artifact.sha256)?;
    let target = update_target()?;
    replace_binary(&target, &artifact_bytes)?;
    let latest = latest_version
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    let outcome = UpdateOutcome {
        current_version,
        latest_version,
        channel,
        manifest_url,
        platform,
        update_available: true,
        action: "applied",
        applied: true,
        artifact_url: Some(artifact.url),
        install_path: Some(target),
        message: format!("ctx updated to {latest}; rerun your command to use the new version"),
    };
    write_update_state(data_root, &outcome)?;
    if !options.quiet {
        eprintln!("{}", outcome.message);
    }
    Ok(outcome)
}

fn should_check_now(data_root: &Path, interval: Duration) -> bool {
    if interval.is_zero() {
        return true;
    }
    let path = data_root.join(UPDATE_STATE_FILE);
    let Ok(value) = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .ok_or(())
    else {
        return true;
    };
    let Some(last_checked) = value
        .get("last_checked_unix_s")
        .and_then(|value| value.as_u64())
    else {
        return true;
    };
    now_unix_s().saturating_sub(last_checked) >= interval.as_secs()
}

fn write_update_state(data_root: &Path, outcome: &UpdateOutcome) -> Result<()> {
    fs::create_dir_all(data_root)?;
    let path = data_root.join(UPDATE_STATE_FILE);
    let body = serde_json::to_vec_pretty(&json!({
        "schema_version": 1,
        "last_checked_unix_s": now_unix_s(),
        "last_result": outcome.action,
        "latest_version": outcome.latest_version,
        "update_available": outcome.update_available,
        "applied": outcome.applied,
    }))?;
    fs::write(path, body)?;
    Ok(())
}

fn write_update_state_error(data_root: &Path, error: &str) -> Result<()> {
    fs::create_dir_all(data_root)?;
    let path = data_root.join(UPDATE_STATE_FILE);
    let body = serde_json::to_vec_pretty(&json!({
        "schema_version": 1,
        "last_checked_unix_s": now_unix_s(),
        "last_result": "error",
        "error": error,
    }))?;
    fs::write(path, body)?;
    Ok(())
}

fn manifest_url(config: &AppConfig) -> String {
    if let Ok(url) = env::var("CTX_UPDATE_MANIFEST_URL") {
        if !url.trim().is_empty() {
            return url;
        }
    }
    format!(
        "{}/releases/{}/latest.json",
        config.updates.endpoint_base.trim_end_matches('/'),
        config.updates.channel
    )
}

fn manifest_version(manifest: &Value) -> Option<String> {
    manifest
        .get("latest_version")
        .or_else(|| manifest.get("version"))
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

fn resolve_artifact(manifest: &Value, platform: &str, manifest_url: &str) -> Result<Artifact> {
    let platform_manifest = manifest
        .get("platforms")
        .and_then(|value| value.get(platform));
    if let Some(platform_manifest) = platform_manifest {
        for kind in ["cli", "binary"] {
            if let Some(artifact) = platform_manifest.get(kind) {
                if let Some(resolved) = artifact_from_value(artifact, manifest_url) {
                    return Ok(resolved);
                }
            }
        }
    }
    if manifest
        .get("platform")
        .and_then(|value| value.as_str())
        .map_or(true, |candidate| candidate == platform)
    {
        if let Some(artifact) = manifest
            .get("artifacts")
            .and_then(|value| value.as_array())
            .and_then(|items| items.first())
            .and_then(|value| artifact_from_value(value, manifest_url))
        {
            return Ok(artifact);
        }
    }
    Err(anyhow!("manifest has no ctx CLI artifact for {platform}"))
}

fn artifact_from_value(value: &Value, manifest_url: &str) -> Option<Artifact> {
    let url = value
        .get("url")
        .or_else(|| value.get("url_path"))
        .or_else(|| value.get("download_url"))
        .or_else(|| value.get("path"))
        .and_then(|value| value.as_str())?;
    let sha256 = value.get("sha256").and_then(|value| value.as_str())?;
    Some(Artifact {
        url: resolve_url(url, manifest_url),
        sha256: sha256.to_owned(),
        bytes: value.get("bytes").and_then(|value| value.as_u64()),
    })
}

fn resolve_url(url: &str, manifest_url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("file://") {
        return url.to_owned();
    }
    if url.starts_with('/') {
        if let Some((scheme, rest)) = manifest_url.split_once("://") {
            if let Some(host) = rest.split('/').next() {
                return format!("{scheme}://{host}{url}");
            }
        }
    }
    let base = manifest_url
        .rsplit_once('/')
        .map(|(base, _)| base)
        .unwrap_or(".");
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        url.trim_start_matches('/')
    )
}

fn verify_sha256(bytes: &[u8], expected: &str) -> Result<()> {
    let actual = hex_sha256(bytes);
    if !actual.eq_ignore_ascii_case(expected.trim()) {
        return Err(anyhow!(
            "sha256 mismatch: expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn update_target() -> Result<PathBuf> {
    if let Ok(path) = env::var("CTX_UPDATE_TARGET") {
        if !path.trim().is_empty() {
            return Ok(PathBuf::from(path));
        }
    }
    env::current_exe().context("resolve current executable")
}

fn replace_binary(target: &Path, bytes: &[u8]) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("update target has no parent: {}", target.display()))?;
    fs::create_dir_all(parent)?;
    let nonce = format!("{}.{}", std::process::id(), now_unix_s());
    let staged = parent.join(format!(".ctx-update-{nonce}"));
    let backup = parent.join(format!(".ctx-backup-{nonce}"));
    fs::write(&staged, bytes)?;
    set_executable(&staged)?;
    if target.exists() {
        fs::rename(target, &backup)
            .with_context(|| format!("move current binary {}", target.display()))?;
    }
    if let Err(err) = fs::rename(&staged, target) {
        if backup.exists() {
            let _ = fs::rename(&backup, target);
        }
        return Err(err).with_context(|| format!("promote update {}", target.display()));
    }
    if backup.exists() {
        let _ = fs::remove_file(&backup);
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn platform_key() -> String {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", "aarch64") => "macos-arm64",
        ("macos", "x86_64") => "macos-x64",
        ("windows", "x86_64") => "windows-x64",
        ("freebsd", "x86_64") => "freebsd-x64",
        (os, arch) => return format!("{os}-{arch}"),
    }
    .to_owned()
}

fn version_gt(candidate: &str, current: &str) -> bool {
    parse_version(candidate) > parse_version(current)
}

fn parse_version(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

fn is_dev_binary() -> bool {
    env::current_exe().ok().is_some_and(|path| {
        let text = path.display().to_string();
        text.contains("/target/debug/") || text.contains("/target/release/")
    })
}

fn env_flag(key: &str) -> bool {
    env::var_os(key).is_some_and(|value| {
        let value = value.to_string_lossy();
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

fn now_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_numerically() {
        assert!(version_gt("0.1.10", "0.1.2"));
        assert!(version_gt("v1.0.0", "0.9.9"));
        assert!(!version_gt("0.1.0", "0.1.0"));
    }
}
