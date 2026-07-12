use std::{
    env, fs,
    io::Write as _,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::collections::BTreeSet;

use anyhow::{anyhow, Context, Result};
use ctx_history_core::utc_now;
#[cfg(unix)]
use flate2::read::GzDecoder;
#[cfg(unix)]
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[cfg(unix)]
use super::env_flag;
use super::path::ctx_binary_version;
use super::state::{atomic_write_json, now_unix_s, read_json_file};
use super::{platform_key, sha256_hex, UpgradePlan};

const MAX_INSTALL_ATTEMPT_ID_CHARS: usize = 128;
const MAX_RUNTIME_EXPANDED_BYTES: u64 = 1024 * 1024 * 1024;
#[cfg(unix)]
const INSTALL_TRANSACTION_FILE: &str = "upgrade-install-transaction.json";

#[derive(Debug)]
struct StagedRuntime {
    staged_path: PathBuf,
    target_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(super) struct InstallMarker {
    pub(super) install_path: PathBuf,
    pub(super) platform: String,
    pub(super) channel: String,
    pub(super) version: String,
    pub(super) sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(super) enum ApplyResult {
    Applied,
    Scheduled { helper_pid: u32 },
}

pub(super) fn apply_artifact(
    plan: &UpgradePlan,
    bytes: &[u8],
    runtime_bytes: Option<&[u8]>,
    data_root: &Path,
    upgrade_lock_path: &Path,
) -> Result<ApplyResult> {
    let parent = plan.install_path.parent().ok_or_else(|| {
        anyhow!(
            "install path has no parent: {}",
            plan.install_path.display()
        )
    })?;
    fs::create_dir_all(parent)?;
    let unique = format!("{}.{}", std::process::id(), now_unix_s());
    let staged = parent.join(format!(".ctx-upgrade-{unique}.new"));
    let marker_path = install_marker_path(&plan.install_path);
    let marker_staged = parent.join(format!(".ctx-upgrade-{unique}.install.json.new"));
    let stage_binary = || -> Result<()> {
        let mut file = fs::File::create(&staged)
            .with_context(|| format!("create staged artifact {}", staged.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        make_executable(&staged, &plan.install_path)?;
        verify_staged_version(&staged, &plan.latest_version)
    };
    if let Err(error) = stage_binary() {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    let install_attempt_id = existing_install_attempt_id(&marker_path);
    if let Err(error) = write_install_marker_to(&marker_staged, plan, install_attempt_id.as_deref())
    {
        let _ = fs::remove_file(&staged);
        let _ = fs::remove_file(&marker_staged);
        return Err(error);
    }
    let staged_runtime = match runtime_bytes {
        Some(runtime_bytes) => {
            match stage_runtime_artifact(plan, runtime_bytes, &unique, data_root) {
                Ok(runtime) => Some(runtime),
                Err(error) => {
                    let _ = fs::remove_file(&staged);
                    let _ = fs::remove_file(&marker_staged);
                    return Err(error);
                }
            }
        }
        None => None,
    };
    let result = replace_binary(
        &staged,
        plan,
        staged_runtime.as_ref(),
        &marker_staged,
        &unique,
        data_root,
        upgrade_lock_path,
    );
    if result.is_err() {
        let _ = fs::remove_file(&staged);
        let _ = fs::remove_file(&marker_staged);
        if let Some(runtime) = &staged_runtime {
            let _ = fs::remove_dir_all(&runtime.staged_path);
        }
    }
    let result = result?;
    sync_parent(parent);
    Ok(result)
}

fn stage_runtime_artifact(
    plan: &UpgradePlan,
    bytes: &[u8],
    unique: &str,
    data_root: &Path,
) -> Result<StagedRuntime> {
    let runtime = plan
        .metadata
        .onnxruntime
        .as_ref()
        .ok_or_else(|| anyhow!("ONNX Runtime bytes provided without release metadata"))?;
    let runtime_root = semantic_runtime_root(data_root)?;
    let runtime_parent = runtime_root.join("onnxruntime").join(&runtime.version);
    let target_path = runtime_parent.join(&plan.platform);
    let staged_path = runtime_parent.join(format!(".{}.ctx-upgrade-{unique}.new", plan.platform));
    let archive_path = runtime_parent.join(format!(".ctx-runtime-{unique}.download"));
    fs::create_dir_all(&runtime_parent)?;
    let result = (|| -> Result<()> {
        let mut archive = fs::File::create(&archive_path)
            .with_context(|| format!("create staged runtime {}", archive_path.display()))?;
        archive.write_all(bytes)?;
        archive.sync_all()?;
        fs::create_dir(&staged_path)
            .with_context(|| format!("create staged runtime {}", staged_path.display()))?;
        extract_runtime_archive(
            &archive_path,
            &staged_path,
            &runtime.artifact,
            &plan.platform,
            &runtime.version,
        )?;
        write_runtime_manifest(plan, &staged_path)?;
        #[cfg(unix)]
        sync_directory(&staged_path)?;
        Ok(())
    })();
    let _ = fs::remove_file(&archive_path);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&staged_path);
        return Err(error);
    }
    sync_parent(&runtime_parent);
    Ok(StagedRuntime {
        staged_path,
        target_path,
    })
}

fn semantic_runtime_root(data_root: &Path) -> Result<PathBuf> {
    let (source, root) = match env::var_os("CTX_RUNTIME_DIR") {
        Some(value) => ("CTX_RUNTIME_DIR", PathBuf::from(value)),
        None => ("selected ctx data root", data_root.join("runtime")),
    };
    validate_runtime_root(source, &root)?;
    Ok(root)
}

fn validate_runtime_root(source: &str, path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .to_str()
            .is_some_and(|value| value.trim().is_empty() || value.trim() != value)
    {
        return Err(anyhow!("{source} must not be empty or whitespace-padded"));
    }
    if !path.is_absolute() {
        return Err(anyhow!("{source} must be an absolute path"));
    }
    Ok(())
}

fn write_runtime_manifest(plan: &UpgradePlan, staged_path: &Path) -> Result<()> {
    let runtime = plan
        .metadata
        .onnxruntime
        .as_ref()
        .ok_or_else(|| anyhow!("release metadata has no ONNX Runtime sidecar"))?;
    let body = json!({
        "schema_version": 1,
        "manager": "ctx-hosted-installer",
        "metadata_trust": "signed-release-metadata",
        "runtime": "onnxruntime",
        "platform": plan.platform,
        "version": runtime.version,
        "sha256": runtime.sha256,
        "artifact_url": plan.onnxruntime_artifact_url(),
        "installed_at": utc_now(),
    });
    let manifest = staged_path.join("ctx-runtime-install.json");
    let mut file = fs::File::create(&manifest)
        .with_context(|| format!("create runtime manifest {}", manifest.display()))?;
    file.write_all(&serde_json::to_vec_pretty(&body)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn extract_runtime_archive(
    archive_path: &Path,
    destination: &Path,
    artifact_name: &str,
    platform: &str,
    version: &str,
) -> Result<()> {
    if !artifact_name.ends_with(".tar.gz") {
        return Err(anyhow!(
            "unsupported ONNX Runtime archive format for {platform}: {artifact_name}"
        ));
    }
    use std::os::unix::fs::PermissionsExt as _;

    let library = if platform.starts_with("macos-") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    let expected_files = BTreeSet::from([
        "LICENSE".to_owned(),
        "ThirdPartyNotices.txt".to_owned(),
        "VERSION_NUMBER".to_owned(),
        "GIT_COMMIT_ID".to_owned(),
        format!("lib/{library}"),
    ]);
    let mut expected_entries = expected_files.clone();
    expected_entries.insert("lib".to_owned());
    let archive_file = fs::File::open(archive_path)
        .with_context(|| format!("open runtime archive {}", archive_path.display()))?;
    let decoder = GzDecoder::new(archive_file);
    let mut archive = tar::Archive::new(decoder);
    let mut seen = BTreeSet::new();
    let mut total_size = 0_u64;
    let lib_dir = destination.join("lib");

    for entry in archive.entries().context("read ONNX Runtime archive")? {
        let mut entry = entry.context("read ONNX Runtime archive entry")?;
        let raw = std::str::from_utf8(entry.path_bytes().as_ref())
            .context("runtime archive path is not UTF-8")?
            .to_owned();
        let is_directory_name = raw.ends_with('/');
        let name = raw.strip_suffix('/').unwrap_or(&raw);
        if name.is_empty()
            || raw.contains('\\')
            || raw.starts_with('/')
            || name == "."
            || name == ".."
            || name.starts_with("../")
            || name.contains("/./")
            || name.contains("//")
            || (is_directory_name && name != "lib")
        {
            return Err(anyhow!(
                "unsafe or non-canonical runtime archive path: {raw:?}"
            ));
        }
        if !expected_entries.contains(name) {
            return Err(anyhow!("unexpected runtime archive entry: {name}"));
        }
        if !seen.insert(name.to_owned()) {
            return Err(anyhow!("duplicate runtime archive entry: {name}"));
        }
        let mode = entry.header().mode().context("read runtime archive mode")?;
        if mode & 0o7000 != 0 {
            return Err(anyhow!(
                "unsafe permission bits on runtime archive entry: {name}"
            ));
        }
        let entry_type = entry.header().entry_type();
        if name == "lib" {
            if !is_directory_name || !entry_type.is_dir() {
                return Err(anyhow!("runtime lib entry is not a directory"));
            }
            fs::create_dir_all(&lib_dir)
                .with_context(|| format!("create runtime directory {}", lib_dir.display()))?;
            fs::set_permissions(&lib_dir, fs::Permissions::from_mode(0o755))?;
            continue;
        }
        if is_directory_name || !entry_type.is_file() {
            return Err(anyhow!(
                "runtime archive entry is not a regular file: {name}"
            ));
        }
        total_size = total_size
            .checked_add(entry.size())
            .ok_or_else(|| anyhow!("runtime archive expanded size overflow"))?;
        if total_size > runtime_expanded_size_limit() {
            return Err(anyhow!(
                "runtime archive expands beyond the 1 GiB safety limit"
            ));
        }
        let target = destination.join(name);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create runtime directory {}", parent.display()))?;
        }
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .with_context(|| format!("create runtime file {}", target.display()))?;
        let copied = std::io::copy(&mut entry, &mut output)
            .with_context(|| format!("extract runtime file {name}"))?;
        if copied != entry.size() {
            return Err(anyhow!(
                "runtime archive entry size mismatch for {name}: expected {}, copied {copied}",
                entry.size()
            ));
        }
        fs::set_permissions(
            &target,
            fs::Permissions::from_mode(if name.starts_with("lib/") {
                0o755
            } else {
                0o644
            }),
        )?;
        output.flush()?;
        output.sync_all()?;
    }
    if seen != expected_entries {
        let missing = expected_entries
            .difference(&seen)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        return Err(anyhow!(
            "runtime archive entries do not exactly match the expected layout; missing: {missing}"
        ));
    }
    let actual_version = fs::read(destination.join("VERSION_NUMBER"))?;
    if actual_version != format!("{version}\n").as_bytes() {
        return Err(anyhow!("runtime VERSION_NUMBER is not exactly {version}"));
    }
    sync_directory(&lib_dir)?;
    sync_directory(destination)?;
    Ok(())
}

#[cfg(windows)]
fn extract_runtime_archive(
    archive_path: &Path,
    destination: &Path,
    artifact_name: &str,
    platform: &str,
    version: &str,
) -> Result<()> {
    if !artifact_name.to_ascii_lowercase().ends_with(".zip") {
        return Err(anyhow!(
            "unsupported ONNX Runtime archive format for {platform}: {artifact_name}"
        ));
    }
    const EXTRACT_SCRIPT: &str = r#"
param(
  [string]$ArchivePath,
  [string]$Destination,
  [string]$ExpectedVersion,
  [long]$MaxExpandedBytes
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem
$expectedFiles = [System.Collections.Generic.HashSet[string]]::new(
  [string[]]@('LICENSE', 'ThirdPartyNotices.txt', 'VERSION_NUMBER', 'GIT_COMMIT_ID', 'lib/onnxruntime.dll'),
  [System.StringComparer]::Ordinal
)
$expectedEntries = [System.Collections.Generic.HashSet[string]]::new($expectedFiles, [System.StringComparer]::Ordinal)
[void]$expectedEntries.Add('lib')
$seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
$entries = @{}
[long]$totalLength = 0
$archive = [System.IO.Compression.ZipFile]::OpenRead($ArchivePath)
try {
  foreach ($entry in $archive.Entries) {
    $rawName = $entry.FullName
    if (
      [string]::IsNullOrEmpty($rawName) -or
      $rawName.Contains('\') -or
      $rawName.StartsWith('/', [System.StringComparison]::Ordinal) -or
      $rawName -match '^[A-Za-z]:'
    ) {
      throw "unsafe runtime archive entry path: '$rawName'"
    }
    $isDirectory = $rawName.EndsWith('/', [System.StringComparison]::Ordinal)
    $name = if ($isDirectory) { $rawName.Substring(0, $rawName.Length - 1) } else { $rawName }
    $expectedRawName = if ($name -ceq 'lib') { 'lib/' } else { $name }
    if (
      $rawName -cne $expectedRawName -or
      -not $expectedEntries.Contains($name) -or
      -not $seen.Add($name)
    ) {
      throw "unexpected, duplicate, or non-canonical runtime archive entry: '$rawName'"
    }
    $unixMode = ($entry.ExternalAttributes -shr 16) -band 0xFFFF
    $fileType = $unixMode -band 0xF000
    if (($unixMode -band 0x0E00) -ne 0) {
      throw "unsafe permission bits on runtime archive entry: '$rawName'"
    }
    if ($name -ceq 'lib') {
      if (-not $isDirectory -or $fileType -ne 0x4000) {
        throw 'runtime lib entry is not a directory'
      }
    } elseif ($isDirectory -or $fileType -ne 0x8000) {
      throw "runtime archive entry is not a regular file: '$rawName'"
    }
    $totalLength += $entry.Length
    if ($totalLength -gt $MaxExpandedBytes) {
      throw 'runtime archive expands beyond the 1 GiB safety limit'
    }
    $entries[$name] = $entry
  }
  if ($seen.Count -ne $expectedEntries.Count) {
    $missing = @($expectedEntries | Where-Object { -not $seen.Contains($_) })
    throw "runtime archive entries do not exactly match the expected layout; missing: $($missing -join ', ')"
  }
  $versionStream = $entries['VERSION_NUMBER'].Open()
  try {
    $reader = [System.IO.StreamReader]::new($versionStream, [System.Text.UTF8Encoding]::new($false, $true))
    try {
      $versionText = $reader.ReadToEnd()
    } finally {
      $reader.Dispose()
    }
  } finally {
    $versionStream.Dispose()
  }
  if ($versionText -cne ($ExpectedVersion + [char]10)) {
    throw "runtime VERSION_NUMBER is not exactly $ExpectedVersion"
  }
  New-Item -ItemType Directory -Path (Join-Path $Destination 'lib') -Force | Out-Null
  foreach ($name in $expectedFiles) {
    $target = Join-Path $Destination ($name.Replace('/', '\'))
    $sourceStream = $entries[$name].Open()
    try {
      $targetStream = [System.IO.File]::Open($target, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
      try {
        $sourceStream.CopyTo($targetStream)
        $targetStream.Flush($true)
      } finally {
        $targetStream.Dispose()
      }
    } finally {
      $sourceStream.Dispose()
    }
  }
} finally {
  $archive.Dispose()
}
"#;
    let script_path = archive_path.with_extension("extract.ps1");
    fs::write(&script_path, EXTRACT_SCRIPT)
        .with_context(|| format!("write runtime extraction helper {}", script_path.display()))?;
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script_path)
        .arg("-ArchivePath")
        .arg(archive_path)
        .arg("-Destination")
        .arg(destination)
        .arg("-ExpectedVersion")
        .arg(version)
        .arg("-MaxExpandedBytes")
        .arg(runtime_expanded_size_limit().to_string())
        .output()
        .context("run Windows ONNX Runtime extraction helper");
    let _ = fs::remove_file(&script_path);
    let output = output?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(anyhow!("extract ONNX Runtime sidecar: {stderr}"));
    }
    Ok(())
}

fn runtime_expanded_size_limit() -> u64 {
    if cfg!(debug_assertions) {
        if let Ok(value) = env::var("CTX_UPGRADE_RUNTIME_MAX_EXPANDED_BYTES_FOR_TESTS") {
            if let Ok(value) = value.parse::<u64>() {
                if value > 0 {
                    return value;
                }
            }
        }
    }
    MAX_RUNTIME_EXPANDED_BYTES
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
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum JournalPhase {
    Publishing,
    Committed,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum JournalPathKind {
    File,
    Directory,
}

#[cfg(unix)]
#[derive(Debug, Clone, Deserialize, Serialize)]
struct JournalPath {
    label: String,
    staged: PathBuf,
    target: PathBuf,
    backup: PathBuf,
    kind: JournalPathKind,
}

#[cfg(unix)]
#[derive(Debug, Deserialize, Serialize)]
struct InstallTransactionJournal {
    schema_version: u32,
    transaction_id: String,
    phase: JournalPhase,
    install_path: PathBuf,
    paths: Vec<JournalPath>,
}

#[cfg(unix)]
fn install_transaction_path(data_root: &Path) -> PathBuf {
    data_root.join(INSTALL_TRANSACTION_FILE)
}

#[cfg(unix)]
fn write_install_transaction(data_root: &Path, journal: &InstallTransactionJournal) -> Result<()> {
    if cfg!(debug_assertions)
        && journal.phase == JournalPhase::Committed
        && env_flag("CTX_UPGRADE_FAIL_COMMIT_JOURNAL_WRITE_FOR_TESTS")
    {
        return Err(anyhow!("injected committed journal write failure"));
    }
    let path = install_transaction_path(data_root);
    fs::create_dir_all(data_root)?;
    let temporary = data_root.join(format!(
        ".{INSTALL_TRANSACTION_FILE}.tmp.{}",
        std::process::id()
    ));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)
            .with_context(|| format!("create install transaction {}", temporary.display()))?;
        file.write_all(&serde_json::to_vec_pretty(journal)?)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, &path).with_context(|| {
            format!(
                "publish install transaction {} to {}",
                temporary.display(),
                path.display()
            )
        })?;
        sync_directory(data_root)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn remove_install_transaction(data_root: &Path) -> Result<()> {
    let path = install_transaction_path(data_root);
    match fs::remove_file(&path) {
        Ok(()) => sync_directory(data_root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

#[cfg(unix)]
pub(super) fn recover_interrupted_install(data_root: &Path) -> Result<bool> {
    let path = install_transaction_path(data_root);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let journal: InstallTransactionJournal = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse interrupted install transaction {}", path.display()))?;
    validate_install_transaction(&journal, data_root)?;
    match journal.phase {
        JournalPhase::Publishing => rollback_journal_paths(&journal.paths)?,
        JournalPhase::Committed => finish_committed_journal(&journal)?,
    }
    remove_install_transaction(data_root)?;
    Ok(true)
}

#[cfg(not(unix))]
pub(super) fn recover_interrupted_install(_data_root: &Path) -> Result<bool> {
    Ok(false)
}

#[cfg(unix)]
fn validate_install_transaction(
    journal: &InstallTransactionJournal,
    data_root: &Path,
) -> Result<()> {
    if journal.schema_version != 1
        || journal.transaction_id.is_empty()
        || journal.transaction_id.len() > 128
        || !journal
            .transaction_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
    {
        return Err(anyhow!("invalid install transaction identity"));
    }
    let expected_install_path = current_install_path()?;
    if journal.install_path != expected_install_path {
        return Err(anyhow!(
            "install transaction targets {}, expected current managed install {}",
            journal.install_path.display(),
            expected_install_path.display()
        ));
    }
    let binary = journal
        .paths
        .iter()
        .find(|path| path.label == "ctx binary")
        .ok_or_else(|| anyhow!("install transaction missing ctx binary"))?;
    let marker = journal
        .paths
        .iter()
        .find(|path| path.label == "ctx install marker")
        .ok_or_else(|| anyhow!("install transaction missing ctx install marker"))?;
    if journal.paths.len() != 2 && journal.paths.len() != 3 {
        return Err(anyhow!("install transaction has an unexpected path count"));
    }
    if binary.kind != JournalPathKind::File
        || binary.target != journal.install_path
        || binary.staged
            != journal
                .install_path
                .parent()
                .ok_or_else(|| anyhow!("install transaction install path has no parent"))?
                .join(format!(".ctx-upgrade-{}.new", journal.transaction_id))
        || binary.backup
            != transaction_backup_path(&journal.install_path, &journal.transaction_id, "binary")
    {
        return Err(anyhow!("install transaction has invalid binary paths"));
    }
    let expected_marker = install_marker_path(&journal.install_path);
    if marker.kind != JournalPathKind::File
        || marker.target != expected_marker
        || marker.staged
            != journal.install_path.parent().unwrap().join(format!(
                ".ctx-upgrade-{}.install.json.new",
                journal.transaction_id
            ))
        || marker.backup
            != transaction_backup_path(&expected_marker, &journal.transaction_id, "marker")
    {
        return Err(anyhow!("install transaction has invalid marker paths"));
    }
    let runtimes = journal
        .paths
        .iter()
        .filter(|path| path.label == "ONNX Runtime sidecar")
        .collect::<Vec<_>>();
    if journal.paths.len() == 3 && runtimes.len() != 1 {
        return Err(anyhow!("install transaction has invalid runtime paths"));
    }
    if let Some(runtime) = runtimes.first() {
        let name = runtime
            .target
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow!("install transaction runtime target has no file name"))?;
        if runtime.kind != JournalPathKind::Directory
            || runtime.staged
                != runtime.target.with_file_name(format!(
                    ".{name}.ctx-upgrade-{}.new",
                    journal.transaction_id
                ))
            || runtime.backup
                != transaction_backup_path(&runtime.target, &journal.transaction_id, "runtime")
        {
            return Err(anyhow!("install transaction has invalid runtime paths"));
        }
        let expected_runtime_root = semantic_runtime_root(data_root)?.join("onnxruntime");
        let relative = runtime
            .target
            .strip_prefix(&expected_runtime_root)
            .map_err(|_| {
                anyhow!("install transaction runtime is outside the selected runtime root")
            })?;
        let components = relative.components().collect::<Vec<_>>();
        if components.len() != 2
            || components
                .iter()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
            || runtime.target.file_name().and_then(|value| value.to_str()) != Some(platform_key()?)
        {
            return Err(anyhow!("install transaction has invalid runtime identity"));
        }
    }
    if journal.paths.iter().any(|path| {
        !matches!(
            path.label.as_str(),
            "ONNX Runtime sidecar" | "ctx binary" | "ctx install marker"
        )
    }) {
        return Err(anyhow!("install transaction has an unknown path label"));
    }
    Ok(())
}

#[cfg(unix)]
fn rollback_journal_paths(paths: &[JournalPath]) -> Result<()> {
    for path in paths.iter().rev() {
        let staged_exists = path.staged.exists();
        let backup_exists = path.backup.exists();
        if backup_exists {
            if staged_exists {
                match path.kind {
                    JournalPathKind::File if path.target.exists() => {
                        remove_journal_path(&path.backup, path.kind)?;
                    }
                    JournalPathKind::Directory if path.target.exists() => {
                        return Err(anyhow!(
                            "interrupted {} has both target and staged directories; recoverable backup retained at {}",
                            path.label,
                            path.backup.display()
                        ));
                    }
                    _ => {
                        fs::rename(&path.backup, &path.target).with_context(|| {
                            format!(
                                "restore interrupted {} from {}",
                                path.label,
                                path.backup.display()
                            )
                        })?;
                    }
                }
            } else {
                if path.target.exists() {
                    remove_journal_path(&path.target, path.kind)?;
                }
                fs::rename(&path.backup, &path.target).with_context(|| {
                    format!(
                        "restore interrupted {} from {}",
                        path.label,
                        path.backup.display()
                    )
                })?;
            }
        } else if !staged_exists && path.target.exists() {
            remove_journal_path(&path.target, path.kind)?;
        }
        if path.staged.exists() {
            remove_journal_path(&path.staged, path.kind)?;
        }
        if let Some(parent) = path.target.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn finish_committed_journal(journal: &InstallTransactionJournal) -> Result<()> {
    for path in &journal.paths {
        if !path.target.exists() || path.staged.exists() {
            return Err(anyhow!(
                "committed install transaction has incomplete {} publication",
                path.label
            ));
        }
    }
    for path in &journal.paths {
        if !path.backup.exists() {
            continue;
        }
        if path.label == "ctx binary" {
            retain_journal_binary_backup(path, &backup_path(&journal.install_path))?;
        } else {
            remove_journal_path(&path.backup, path.kind)?;
        }
        if let Some(parent) = path.target.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn retain_journal_binary_backup(path: &JournalPath, durable_backup: &Path) -> Result<()> {
    if durable_backup.exists() {
        fs::remove_file(durable_backup)
            .with_context(|| format!("remove old ctx backup {}", durable_backup.display()))?;
    }
    fs::rename(&path.backup, durable_backup).with_context(|| {
        format!(
            "retain previous ctx binary {} at {}",
            path.backup.display(),
            durable_backup.display()
        )
    })
}

#[cfg(unix)]
fn remove_journal_path(path: &Path, kind: JournalPathKind) -> Result<()> {
    let result = match kind {
        JournalPathKind::File => fs::remove_file(path),
        JournalPathKind::Directory => fs::remove_dir_all(path),
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

#[cfg(unix)]
fn replace_binary(
    staged: &Path,
    plan: &UpgradePlan,
    staged_runtime: Option<&StagedRuntime>,
    marker_staged: &Path,
    unique: &str,
    data_root: &Path,
    _upgrade_lock_path: &Path,
) -> Result<ApplyResult> {
    let marker_path = install_marker_path(&plan.install_path);
    let mut runtime = staged_runtime.map(|runtime| {
        UnixPublishedPath::new(
            "ONNX Runtime sidecar",
            runtime.staged_path.clone(),
            runtime.target_path.clone(),
            transaction_backup_path(&runtime.target_path, unique, "runtime"),
            PublishedPathKind::Directory,
        )
    });
    let mut binary = UnixPublishedPath::new(
        "ctx binary",
        staged.to_path_buf(),
        plan.install_path.clone(),
        transaction_backup_path(&plan.install_path, unique, "binary"),
        PublishedPathKind::File,
    );
    let marker_backup = transaction_backup_path(&marker_path, unique, "marker");
    let mut marker = UnixPublishedPath::new(
        "ctx install marker",
        marker_staged.to_path_buf(),
        marker_path,
        marker_backup,
        PublishedPathKind::File,
    );
    let mut journal = InstallTransactionJournal {
        schema_version: 1,
        transaction_id: unique.to_owned(),
        phase: JournalPhase::Publishing,
        install_path: plan.install_path.clone(),
        paths: runtime
            .iter()
            .map(UnixPublishedPath::journal_path)
            .chain(std::iter::once(binary.journal_path()))
            .chain(std::iter::once(marker.journal_path()))
            .collect(),
    };
    write_install_transaction(data_root, &journal)?;
    let publish_result = (|| -> Result<()> {
        if let Some(runtime) = runtime.as_mut() {
            runtime.publish(false)?;
            abort_after_publish_for_tests("runtime");
        }
        binary.publish(false)?;
        abort_after_publish_for_tests("binary");
        marker.publish(true)?;
        abort_after_publish_for_tests("marker");
        Ok(())
    })();
    if let Err(primary) = publish_result {
        let mut rollback_errors =
            rollback_unix_publication(&mut marker, &mut binary, runtime.as_mut(), true);
        if rollback_errors.is_empty() {
            if let Err(error) = remove_install_transaction(data_root) {
                rollback_errors.push(format!("remove transaction journal: {error:#}"));
            } else {
                return Err(primary);
            }
        }
        return Err(anyhow!(
            "{primary:#}; rollback failures: {}",
            rollback_errors.join("; ")
        ));
    }

    journal.phase = JournalPhase::Committed;
    if let Err(primary) = write_install_transaction(data_root, &journal) {
        let mut rollback_errors =
            rollback_unix_publication(&mut marker, &mut binary, runtime.as_mut(), false);
        if rollback_errors.is_empty() {
            if let Err(error) = remove_install_transaction(data_root) {
                rollback_errors.push(format!("remove transaction journal: {error:#}"));
            } else {
                return Err(primary);
            }
        }
        return Err(anyhow!(
            "{primary:#}; rollback failures: {}",
            rollback_errors.join("; ")
        ));
    }
    if cfg!(debug_assertions) && env_flag("CTX_UPGRADE_ABORT_AFTER_COMMIT_FOR_TESTS") {
        std::process::exit(88);
    }
    marker.discard_backup()?;
    if let Some(runtime) = runtime.as_mut() {
        runtime.discard_backup()?;
    }
    binary.retain_backup_as(&backup_path(&plan.install_path))?;
    remove_install_transaction(data_root)?;
    Ok(ApplyResult::Applied)
}

#[cfg(unix)]
fn rollback_unix_publication(
    marker: &mut UnixPublishedPath,
    binary: &mut UnixPublishedPath,
    runtime: Option<&mut UnixPublishedPath>,
    inject_runtime_restore_failure: bool,
) -> Vec<String> {
    let mut errors = Vec::new();
    if let Err(error) = marker.rollback(false) {
        errors.push(format!("{error:#}"));
    }
    if let Err(error) = binary.rollback(false) {
        errors.push(format!("{error:#}"));
    }
    if let Some(runtime) = runtime {
        if let Err(error) = runtime.rollback(inject_runtime_restore_failure) {
            errors.push(format!("{error:#}"));
        }
    }
    errors
}

#[cfg(unix)]
fn abort_after_publish_for_tests(point: &str) {
    if cfg!(debug_assertions)
        && env::var("CTX_UPGRADE_ABORT_AFTER_PUBLISH_FOR_TESTS")
            .ok()
            .is_some_and(|value| value == point)
    {
        std::process::exit(86);
    }
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum PublishedPathKind {
    File,
    Directory,
}

#[cfg(unix)]
struct UnixPublishedPath {
    label: &'static str,
    staged: PathBuf,
    target: PathBuf,
    backup: PathBuf,
    kind: PublishedPathKind,
    had_previous: bool,
    published: bool,
}

#[cfg(unix)]
impl UnixPublishedPath {
    fn new(
        label: &'static str,
        staged: PathBuf,
        target: PathBuf,
        backup: PathBuf,
        kind: PublishedPathKind,
    ) -> Self {
        Self {
            label,
            staged,
            target,
            backup,
            kind,
            had_previous: false,
            published: false,
        }
    }

    fn journal_path(&self) -> JournalPath {
        JournalPath {
            label: self.label.to_owned(),
            staged: self.staged.clone(),
            target: self.target.clone(),
            backup: self.backup.clone(),
            kind: match self.kind {
                PublishedPathKind::File => JournalPathKind::File,
                PublishedPathKind::Directory => JournalPathKind::Directory,
            },
        }
    }

    fn publish(&mut self, inject_marker_failure: bool) -> Result<()> {
        if self.backup.exists() {
            return Err(anyhow!(
                "{} transaction backup already exists at {}",
                self.label,
                self.backup.display()
            ));
        }
        if self.target.exists() {
            match self.kind {
                PublishedPathKind::File => {
                    backup_file_for_atomic_replace(&self.target, &self.backup, self.label)?
                }
                PublishedPathKind::Directory => {
                    fs::rename(&self.target, &self.backup).with_context(|| {
                        format!(
                            "backup {} {} to {}",
                            self.label,
                            self.target.display(),
                            self.backup.display()
                        )
                    })?;
                }
            }
            self.had_previous = true;
            if let Some(parent) = self.target.parent() {
                sync_directory(parent)?;
            }
            abort_after_backup_for_tests(self.label);
        }
        if inject_marker_failure
            && cfg!(debug_assertions)
            && env_flag("CTX_UPGRADE_FAIL_MARKER_PUBLISH_FOR_TESTS")
        {
            return Err(anyhow!("injected install marker publication failure"));
        }
        fs::rename(&self.staged, &self.target).with_context(|| {
            format!(
                "publish {} {} to {}",
                self.label,
                self.staged.display(),
                self.target.display()
            )
        })?;
        self.published = true;
        if let Some(parent) = self.target.parent() {
            sync_parent(parent);
        }
        Ok(())
    }

    fn rollback(&mut self, inject_runtime_restore_failure: bool) -> Result<()> {
        if self.published && self.target.exists() {
            remove_published_path(&self.target, self.kind).with_context(|| {
                format!(
                    "remove newly published {} at {}; recoverable backup is {}",
                    self.label,
                    self.target.display(),
                    self.backup.display()
                )
            })?;
            self.published = false;
        }
        if self.had_previous {
            if inject_runtime_restore_failure
                && cfg!(debug_assertions)
                && env_flag("CTX_UPGRADE_FAIL_RUNTIME_RESTORE_FOR_TESTS")
            {
                return Err(anyhow!(
                    "injected ONNX Runtime restore failure; recoverable backup retained at {}",
                    self.backup.display()
                ));
            }
            match self.kind {
                PublishedPathKind::File => {
                    fs::rename(&self.backup, &self.target).with_context(|| {
                        format!(
                            "restore {} {} from recoverable backup {}",
                            self.label,
                            self.target.display(),
                            self.backup.display()
                        )
                    })?;
                }
                PublishedPathKind::Directory => {
                    fs::rename(&self.backup, &self.target).with_context(|| {
                        format!(
                            "restore {} {} from recoverable backup {}",
                            self.label,
                            self.target.display(),
                            self.backup.display()
                        )
                    })?;
                }
            }
            self.had_previous = false;
        }
        if let Some(parent) = self.target.parent() {
            sync_parent(parent);
        }
        Ok(())
    }

    fn discard_backup(&mut self) -> Result<()> {
        if self.had_previous {
            remove_published_path(&self.backup, self.kind).with_context(|| {
                format!("remove previous {} {}", self.label, self.backup.display())
            })?;
            self.had_previous = self.backup.exists();
        }
        if let Some(parent) = self.target.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    }

    fn retain_backup_as(&mut self, durable_backup: &Path) -> Result<()> {
        if !self.had_previous {
            return Ok(());
        }
        if durable_backup.exists() {
            fs::remove_file(durable_backup)
                .with_context(|| format!("remove old ctx backup {}", durable_backup.display()))?;
        }
        fs::rename(&self.backup, durable_backup).with_context(|| {
            format!(
                "retain previous {} {} at {}",
                self.label,
                self.backup.display(),
                durable_backup.display()
            )
        })?;
        self.had_previous = false;
        if let Some(parent) = self.target.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn backup_file_for_atomic_replace(target: &Path, backup: &Path, label: &str) -> Result<()> {
    if let Err(link_error) = fs::hard_link(target, backup) {
        fs::copy(target, backup).with_context(|| {
            format!(
                "backup {label} {} to {} after hard-link failed: {link_error}",
                target.display(),
                backup.display()
            )
        })?;
        fs::File::open(backup)?.sync_all()?;
    }
    Ok(())
}

#[cfg(unix)]
fn abort_after_backup_for_tests(label: &str) {
    let point = match label {
        "ONNX Runtime sidecar" => "runtime",
        "ctx binary" => "binary",
        "ctx install marker" => "marker",
        _ => return,
    };
    if cfg!(debug_assertions)
        && env::var("CTX_UPGRADE_ABORT_AFTER_BACKUP_FOR_TESTS")
            .ok()
            .is_some_and(|value| value == point)
    {
        std::process::exit(87);
    }
}

#[cfg(unix)]
fn remove_published_path(path: &Path, kind: PublishedPathKind) -> std::io::Result<()> {
    match kind {
        PublishedPathKind::File => fs::remove_file(path),
        PublishedPathKind::Directory => fs::remove_dir_all(path),
    }
}

#[cfg(unix)]
fn transaction_backup_path(target: &Path, unique: &str, label: &str) -> PathBuf {
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(label);
    target.with_file_name(format!(".{name}.ctx-upgrade-{unique}.{label}.previous"))
}

#[cfg(windows)]
fn replace_binary(
    staged: &Path,
    plan: &UpgradePlan,
    staged_runtime: Option<&StagedRuntime>,
    marker_staged: &Path,
    unique: &str,
    data_root: &Path,
    upgrade_lock_path: &Path,
) -> Result<ApplyResult> {
    let target = &plan.install_path;
    let backup = backup_path(target);
    let script = staged.with_extension("ps1");
    let marker_path = install_marker_path(target);
    let marker_backup = marker_path.with_file_name(format!(
        ".ctx.install.json.ctx-upgrade-{unique}.marker.previous"
    ));
    let state_path = data_root.join(super::state::STATE_FILE);
    let parent = std::process::id();
    let (runtime_variables, runtime_install, runtime_rollback, runtime_finish) =
        windows_runtime_script(staged_runtime);
    let body = format!(
        r#"$ErrorActionPreference = 'Stop'
$parent = {parent}
$staged = {staged}
$target = {target}
$backup = {backup}
$markerTmp = {marker_tmp}
$markerPath = {marker_path}
$markerBackup = {marker_backup}
$lockPath = {lock_path}
$statePath = {state_path}
$currentVersion = {current_version}
$latestVersion = {latest_version}
$channel = {channel}
$platform = {platform}
$metadataUrl = {metadata_url}
$artifactUrl = {artifact_url}
$markerHadPrevious = $false
$markerPublished = $false
$binaryHadPrevious = Test-Path -LiteralPath $target
$binaryPublished = $false
{runtime_variables}

function Test-OwnsUpgradeLock {{
  if (-not (Test-Path -LiteralPath $lockPath)) {{ return $false }}
  try {{
    $fields = ((Get-Content -LiteralPath $lockPath -Raw).Trim() -split '\s+')
    return $fields.Count -ge 1 -and $fields[0] -eq [string]$PID
  }} catch {{
    return $false
  }}
}}

function Write-TerminalUpgradeState([string]$status, [string]$errorMessage) {{
  $state = [ordered]@{{
    schema_version = 1
    status = $status
    checked_at = [DateTime]::UtcNow.ToString('o')
    last_checked_unix_s = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    current_version = $(if ($status -eq 'applied') {{ $latestVersion }} else {{ $currentVersion }})
    latest_version = $latestVersion
    update_available = ($status -ne 'applied')
    channel = $channel
    platform = $platform
    metadata_url = $metadataUrl
    artifact_url = $artifactUrl
    install_path = $target
    managed = $true
  }}
  if (-not [string]::IsNullOrEmpty($errorMessage)) {{ $state['error'] = $errorMessage }}
  $stateTmp = "$statePath.tmp.$PID"
  $utf8 = [System.Text.UTF8Encoding]::new($false)
  [System.IO.File]::WriteAllText($stateTmp, (($state | ConvertTo-Json -Depth 4) + [char]10), $utf8)
  $stateStream = [System.IO.File]::Open($stateTmp, [System.IO.FileMode]::Open, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::Read)
  try {{ $stateStream.Flush($true) }} finally {{ $stateStream.Dispose() }}
  if (Test-Path -LiteralPath $statePath) {{
    [System.IO.File]::Replace($stateTmp, $statePath, $null, $true)
  }} else {{
    Move-Item -LiteralPath $stateTmp -Destination $statePath
  }}
}}

while ($null -ne (Get-Process -Id $parent -ErrorAction SilentlyContinue)) {{
  Start-Sleep -Milliseconds 250
}}

$terminalError = $null
try {{
if (-not (Test-OwnsUpgradeLock)) {{
  throw "ctx upgrade helper did not receive the serialization lock"
}}
try {{
{runtime_install}
if (Test-Path -LiteralPath $target) {{
  [System.IO.File]::Replace($staged, $target, $backup, $true)
}} else {{
  Move-Item -LiteralPath $staged -Destination $target -Force
}}
$binaryPublished = $true
if (Test-Path -LiteralPath $markerBackup) {{
  throw "install marker transaction backup already exists at $markerBackup"
}}
if (Test-Path -LiteralPath $markerPath) {{
  Move-Item -LiteralPath $markerPath -Destination $markerBackup
  $markerHadPrevious = $true
}}
if (Test-Path -LiteralPath $markerTmp) {{
  Move-Item -LiteralPath $markerTmp -Destination $markerPath
  $markerPublished = $true
}} else {{
  throw "staged install marker is missing at $markerTmp"
}}
}} catch {{
  $publishError = $_
  $rollbackErrors = @()
  try {{
{runtime_rollback}
  }} catch {{
    $rollbackErrors += $_.Exception.Message
  }}
  try {{
    if ($binaryPublished -and (Test-Path -LiteralPath $target)) {{
      Remove-Item -LiteralPath $target -Force
    }}
    if ($binaryHadPrevious -and (Test-Path -LiteralPath $backup)) {{
      Move-Item -LiteralPath $backup -Destination $target -Force
    }}
  }} catch {{
    $rollbackErrors += $_.Exception.Message
  }}
  try {{
    if ($markerPublished -and (Test-Path -LiteralPath $markerPath)) {{
      Remove-Item -LiteralPath $markerPath -Force
    }}
    if ($markerHadPrevious -and (Test-Path -LiteralPath $markerBackup)) {{
      Move-Item -LiteralPath $markerBackup -Destination $markerPath -Force
    }}
  }} catch {{
    $rollbackErrors += $_.Exception.Message
  }}
  if ($rollbackErrors.Count -gt 0) {{
    throw "$($publishError.Exception.Message); rollback failures: $($rollbackErrors -join '; ')"
  }}
  throw $publishError
}}
{runtime_finish}
if (Test-Path -LiteralPath $markerBackup) {{
  Remove-Item -LiteralPath $markerBackup -Force -ErrorAction SilentlyContinue
}}
Write-TerminalUpgradeState 'applied' $null
}} catch {{
  $terminalError = $_.Exception.Message
  try {{ Write-TerminalUpgradeState 'error' $terminalError }} catch {{}}
}} finally {{
  if (Test-OwnsUpgradeLock) {{
    Remove-Item -LiteralPath $lockPath -Force -ErrorAction SilentlyContinue
  }}
  Remove-Item -LiteralPath $MyInvocation.MyCommand.Path -Force -ErrorAction SilentlyContinue
}}
if ($null -ne $terminalError) {{ exit 1 }}
"#,
        staged = ps_single_quote(staged),
        target = ps_single_quote(target),
        backup = ps_single_quote(&backup),
        marker_tmp = ps_single_quote(marker_staged),
        marker_path = ps_single_quote(&marker_path),
        marker_backup = ps_single_quote(&marker_backup),
        lock_path = ps_single_quote(upgrade_lock_path),
        state_path = ps_single_quote(&state_path),
        current_version = ps_single_quote_value(&plan.current_version),
        latest_version = ps_single_quote_value(&plan.latest_version),
        channel = ps_single_quote_value(&plan.channel),
        platform = ps_single_quote_value(&plan.platform),
        metadata_url = ps_single_quote_value(&plan.metadata_url),
        artifact_url = ps_single_quote_value(&plan.artifact_url),
        runtime_variables = runtime_variables,
        runtime_install = runtime_install,
        runtime_rollback = runtime_rollback,
        runtime_finish = runtime_finish,
    );
    fs::write(&script, body)?;
    let child = std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("spawn Windows ctx replacement helper")?;
    Ok(ApplyResult::Scheduled {
        helper_pid: child.id(),
    })
}

#[cfg(not(any(unix, windows)))]
fn replace_binary(
    _staged: &Path,
    _plan: &UpgradePlan,
    _staged_runtime: Option<&StagedRuntime>,
    _marker_staged: &Path,
    _unique: &str,
    _data_root: &Path,
    _upgrade_lock_path: &Path,
) -> Result<ApplyResult> {
    Err(anyhow!(
        "self-upgrade replacement is unsupported on this platform"
    ))
}

#[cfg(windows)]
fn ps_single_quote(path: &Path) -> String {
    ps_single_quote_value(&path.display().to_string())
}

#[cfg(windows)]
fn ps_single_quote_value(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(windows)]
fn windows_runtime_script(runtime: Option<&StagedRuntime>) -> (String, String, String, String) {
    let Some(runtime) = runtime else {
        return (String::new(), String::new(), String::new(), String::new());
    };
    let target_name = runtime
        .target_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("runtime");
    let backup = runtime.target_path.with_file_name(format!(
        ".{target_name}.ctx-upgrade-{}.previous",
        std::process::id()
    ));
    let variables = format!(
        "$runtimeStaged = {}\n$runtimeTarget = {}\n$runtimeBackup = {}\n$runtimeHadPrevious = $false\n$runtimePublished = $false",
        ps_single_quote(&runtime.staged_path),
        ps_single_quote(&runtime.target_path),
        ps_single_quote(&backup),
    );
    let install = r#"  if (Test-Path -LiteralPath $runtimeBackup) {
    throw "ONNX Runtime transaction backup already exists at $runtimeBackup"
  }
  if (Test-Path -LiteralPath $runtimeTarget) {
    Move-Item -LiteralPath $runtimeTarget -Destination $runtimeBackup
    $runtimeHadPrevious = $true
  }
  Move-Item -LiteralPath $runtimeStaged -Destination $runtimeTarget
  $runtimePublished = $true"#
        .to_owned();
    let rollback = r#"  if ($runtimePublished -and (Test-Path -LiteralPath $runtimeTarget)) {
    Remove-Item -LiteralPath $runtimeTarget -Recurse -Force
  }
  if ($runtimeHadPrevious -and (Test-Path -LiteralPath $runtimeBackup)) {
    Move-Item -LiteralPath $runtimeBackup -Destination $runtimeTarget
  }"#
    .to_owned();
    let finish = r#"if (Test-Path -LiteralPath $runtimeBackup) {
  Remove-Item -LiteralPath $runtimeBackup -Recurse -Force -ErrorAction SilentlyContinue
  }"#
    .to_owned();
    (variables, install, rollback, finish)
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
    let _ = sync_directory(parent);
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) {}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .with_context(|| format!("sync directory {}", path.display()))
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
