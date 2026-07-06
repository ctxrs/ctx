use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};

use super::{
    diagnostics::ctx_binary_version,
    types::{ApplyResult, UpgradePlan},
    util::now_unix_s,
};

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
    Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
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
