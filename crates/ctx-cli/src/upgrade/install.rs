use std::{
    env, fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::utc_now;
use serde_json::{json, Value};

use super::path::ctx_binary_version;
use super::state::{atomic_write_json, now_unix_s, read_json_file};
use super::{platform_key, sha256_hex, UpgradePlan};

const MAX_INSTALL_ATTEMPT_ID_CHARS: usize = 128;

#[derive(Debug, Clone)]
pub(super) struct InstallMarker {
    pub(super) install_path: PathBuf,
    pub(super) platform: String,
    pub(super) channel: String,
    pub(super) version: String,
    pub(super) sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ApplyResult {
    Applied,
    Scheduled,
}

pub(super) fn apply_artifact(plan: &UpgradePlan, bytes: &[u8]) -> Result<ApplyResult> {
    let parent = plan.install_path.parent().ok_or_else(|| {
        anyhow!(
            "install path has no parent: {}",
            plan.install_path.display()
        )
    })?;
    fs::create_dir_all(parent)?;
    let unique = format!("{}.{}", std::process::id(), now_unix_s());
    let staged = parent.join(format!(".ctx-upgrade-{unique}.new"));
    {
        let mut file = fs::File::create(&staged)
            .with_context(|| format!("create staged artifact {}", staged.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    make_executable(&staged, &plan.install_path)?;
    verify_staged_version(&staged, &plan.latest_version)?;
    let result = replace_binary(&staged, plan)?;
    sync_parent(parent);
    Ok(result)
}

fn verify_staged_version(staged: &Path, expected_version: &str) -> Result<()> {
    let version = ctx_binary_version(staged)
        .with_context(|| format!("run staged ctx {}", staged.display()))?;
    if !version.contains(expected_version) {
        return Err(anyhow!(
            "staged ctx version mismatch: expected {expected_version}, got {}",
            version.trim()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn make_executable(staged: &Path, target: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(target)
        .map(|metadata| metadata.permissions().mode())
        .unwrap_or(0o755)
        | 0o111;
    fs::set_permissions(staged, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_staged: &Path, _target: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn replace_binary(staged: &Path, plan: &UpgradePlan) -> Result<ApplyResult> {
    let target = &plan.install_path;
    let backup = backup_path(target);
    if target.exists() {
        fs::copy(target, &backup)
            .with_context(|| format!("backup ctx binary to {}", backup.display()))?;
    }
    fs::rename(staged, target)?;
    Ok(ApplyResult::Applied)
}

#[cfg(windows)]
fn replace_binary(staged: &Path, plan: &UpgradePlan) -> Result<ApplyResult> {
    let target = &plan.install_path;
    let backup = backup_path(target);
    let script = staged.with_extension("ps1");
    let marker_tmp = staged.with_extension("install.json.tmp");
    let marker_path = install_marker_path(target);
    let install_attempt_id = existing_install_attempt_id(&marker_path);
    write_install_marker_to(&marker_tmp, plan, install_attempt_id.as_deref())?;
    let parent = std::process::id();
    let body = format!(
        r#"$ErrorActionPreference = 'Stop'
$parent = {parent}
$staged = {staged}
$target = {target}
$backup = {backup}
$markerTmp = {marker_tmp}
$markerPath = {marker_path}
for ($i = 0; $i -lt 80; $i++) {{
  $p = Get-Process -Id $parent -ErrorAction SilentlyContinue
  if ($null -eq $p) {{ break }}
  Start-Sleep -Milliseconds 250
}}
if (Test-Path -LiteralPath $target) {{
  [System.IO.File]::Replace($staged, $target, $backup, $true)
}} else {{
  Move-Item -LiteralPath $staged -Destination $target -Force
}}
if (Test-Path -LiteralPath $markerTmp) {{
  Move-Item -LiteralPath $markerTmp -Destination $markerPath -Force
}}
Remove-Item -LiteralPath $MyInvocation.MyCommand.Path -Force
"#,
        staged = ps_single_quote(staged),
        target = ps_single_quote(target),
        backup = ps_single_quote(&backup),
        marker_tmp = ps_single_quote(&marker_tmp),
        marker_path = ps_single_quote(&marker_path),
    );
    fs::write(&script, body)?;
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("spawn Windows ctx replacement helper")?;
    Ok(ApplyResult::Scheduled)
}

#[cfg(not(any(unix, windows)))]
fn replace_binary(_staged: &Path, _plan: &UpgradePlan) -> Result<ApplyResult> {
    Err(anyhow!(
        "self-upgrade replacement is unsupported on this platform"
    ))
}

#[cfg(windows)]
fn ps_single_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

fn backup_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("ctx");
    target.with_file_name(format!("{name}.previous"))
}

#[cfg(unix)]
fn sync_parent(parent: &Path) {
    let _ = fs::File::open(parent).and_then(|file| file.sync_all());
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) {}

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

pub(super) fn read_verified_install_marker_for_current_exe() -> Result<InstallMarker> {
    let marker = read_install_marker_for_current_exe()?;
    verify_install_marker(&marker, platform_key()?)?;
    Ok(marker)
}

pub(super) fn install_marker_for_plan(
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

pub(super) fn write_install_marker_after_upgrade(plan: &UpgradePlan) -> Result<()> {
    let marker_path = install_marker_path(&plan.install_path);
    let install_attempt_id = existing_install_attempt_id(&marker_path);
    write_install_marker_to(&marker_path, plan, install_attempt_id.as_deref())
}

fn write_install_marker_to(
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

fn existing_install_attempt_id(marker_path: &Path) -> Option<String> {
    read_json_file(marker_path).and_then(|value| optional_install_attempt_id(&value))
}

fn optional_install_attempt_id(value: &Value) -> Option<String> {
    let id = value.get("install_attempt_id")?.as_str()?.trim();
    is_valid_install_attempt_id(id).then(|| id.to_owned())
}

fn is_valid_install_attempt_id(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= MAX_INSTALL_ATTEMPT_ID_CHARS
        && value
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

fn install_marker_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("ctx");
    path.with_file_name(format!("{file_name}.install.json"))
}

pub(super) fn current_install_path() -> Result<PathBuf> {
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
