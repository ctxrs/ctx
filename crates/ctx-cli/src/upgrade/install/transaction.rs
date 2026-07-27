use std::{fs, path::Path};

#[cfg(unix)]
use std::{env, io::Write as _, path::PathBuf};

#[cfg(unix)]
use anyhow::Context;
use anyhow::{anyhow, Result};
#[cfg(unix)]
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use super::super::env_flag;
use super::super::state::now_unix_s;
use super::super::UpgradePlan;
#[cfg(unix)]
use super::durability::sync_directory;
use super::durability::{backup_path, stage_binary, sync_parent};
#[cfg(unix)]
use super::marker::current_install_path;
use super::marker::{existing_install_attribution, install_marker_path, write_install_marker_to};
#[cfg(unix)]
use super::runtime::semantic_runtime_root;
use super::runtime::{stage_runtime_artifact, StagedRuntime};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
const INSTALL_TRANSACTION_FILE: &str = "upgrade-install-transaction.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(in crate::upgrade) enum ApplyResult {
    Applied,
    Scheduled { helper_pid: u32 },
}

pub(in crate::upgrade) fn apply_artifact(
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
    if let Err(error) = stage_binary(&staged, &plan.install_path, bytes, &plan.latest_version) {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    let install_attribution = existing_install_attribution(&marker_path);
    if let Err(error) = write_install_marker_to(&marker_staged, plan, install_attribution.as_ref())
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
    let result = publish_install(
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
pub(in crate::upgrade) fn recover_interrupted_install(data_root: &Path) -> Result<bool> {
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
pub(in crate::upgrade) fn recover_interrupted_install(_data_root: &Path) -> Result<bool> {
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
            || runtime.target.file_name().and_then(|value| value.to_str())
                != Some(super::super::platform_key()?)
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
fn publish_install(
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
        PublishedPath::new(
            "ONNX Runtime sidecar",
            runtime.staged_path.clone(),
            runtime.target_path.clone(),
            transaction_backup_path(&runtime.target_path, unique, "runtime"),
            PublishedPathKind::Directory,
        )
    });
    let mut binary = PublishedPath::new(
        "ctx binary",
        staged.to_path_buf(),
        plan.install_path.clone(),
        transaction_backup_path(&plan.install_path, unique, "binary"),
        PublishedPathKind::File,
    );
    let marker_backup = transaction_backup_path(&marker_path, unique, "marker");
    let mut marker = PublishedPath::new(
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
            .map(PublishedPath::journal_path)
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
            rollback_publication(&mut marker, &mut binary, runtime.as_mut(), true);
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
            rollback_publication(&mut marker, &mut binary, runtime.as_mut(), false);
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
fn rollback_publication(
    marker: &mut PublishedPath,
    binary: &mut PublishedPath,
    runtime: Option<&mut PublishedPath>,
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
pub(super) enum PublishedPathKind {
    File,
    Directory,
}

#[cfg(unix)]
struct PublishedPath {
    label: &'static str,
    staged: PathBuf,
    target: PathBuf,
    backup: PathBuf,
    kind: PublishedPathKind,
    had_previous: bool,
    published: bool,
}

#[cfg(unix)]
impl PublishedPath {
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
            unix::backup_target(&self.target, &self.backup, self.label, self.kind)?;
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
        unix::publish_staged(&self.staged, &self.target, self.label)?;
        self.published = true;
        if let Some(parent) = self.target.parent() {
            sync_parent(parent);
        }
        Ok(())
    }

    fn rollback(&mut self, inject_runtime_restore_failure: bool) -> Result<()> {
        if self.published && self.target.exists() {
            unix::remove_published(&self.target, &self.backup, self.label, self.kind)?;
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
            unix::restore_backup(&self.backup, &self.target, self.label)?;
            self.had_previous = false;
        }
        if let Some(parent) = self.target.parent() {
            sync_parent(parent);
        }
        Ok(())
    }

    fn discard_backup(&mut self) -> Result<()> {
        if self.had_previous {
            unix::discard_backup(&self.backup, self.label, self.kind)?;
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
        unix::retain_backup_as(&self.backup, durable_backup, self.label)?;
        self.had_previous = false;
        if let Some(parent) = self.target.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    }
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
fn transaction_backup_path(target: &Path, unique: &str, label: &str) -> PathBuf {
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(label);
    target.with_file_name(format!(".{name}.ctx-upgrade-{unique}.{label}.previous"))
}

#[cfg(windows)]
fn publish_install(
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
    let marker_path = install_marker_path(target);
    let marker_backup = marker_path.with_file_name(format!(
        ".ctx.install.json.ctx-upgrade-{unique}.marker.previous"
    ));
    let state_path = data_root.join(super::super::state::STATE_FILE);
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
    let outcome = windows::schedule_replacement(staged, &body)?;
    Ok(ApplyResult::Scheduled {
        helper_pid: outcome.helper_pid,
    })
}

#[cfg(not(any(unix, windows)))]
fn publish_install(
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
