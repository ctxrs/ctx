use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(unix)]
use super::super::super::env_flag;
#[cfg(unix)]
use super::super::durability::sync_directory;
#[cfg(not(windows))]
use super::super::marker::current_install_path;
#[cfg(windows)]
use super::super::marker::current_install_path_for_recovery;
use super::super::marker::install_marker_path;
#[cfg(unix)]
use std::path::Component;

mod semantic;

const INSTALL_TRANSACTION_FILE: &str = "upgrade-install-transaction.json";
pub(super) const INSTALL_TRANSACTION_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(in crate::upgrade) enum JournalPhase {
    Prepared,
    HelperReady,
    Publishing,
    RollingBack,
    RolledBack,
    Committed,
    CleanupPending,
    Failed,
}

impl JournalPhase {
    #[cfg_attr(windows, allow(dead_code))]
    pub(super) fn mutation_may_have_started(self) -> bool {
        matches!(self, Self::Publishing | Self::RollingBack | Self::Failed)
    }

    pub(super) fn committed(self) -> bool {
        matches!(self, Self::Committed | Self::CleanupPending)
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum JournalPathKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum JournalPathState {
    Staged,
    BackedUp,
    Published,
    Cleaned,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct JournalPathIdentity {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) length: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JournalPath {
    pub(super) label: String,
    pub(super) staged: PathBuf,
    pub(super) target: PathBuf,
    pub(super) backup: PathBuf,
    pub(super) kind: JournalPathKind,
    pub(super) target_preexisted: bool,
    pub(super) state: JournalPathState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) staged_identity: Option<JournalPathIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) original_target_identity: Option<JournalPathIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) backup_identity: Option<JournalPathIdentity>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InstallTransactionJournal {
    pub(super) schema_version: u32,
    pub(super) attempt_id: String,
    /// The data root that staged this transaction.  Recovery may be invoked
    /// from another data root, but runtime cleanup must stay with this one.
    pub(super) data_root: PathBuf,
    pub(super) runtime_root: PathBuf,
    /// Canonical model/bundle cache authority for signed Semantic paths.
    /// Absent in pre-provisioning schema-v2 journals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) semantic_cache_root: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) windows_helper: Option<WindowsHelperJournal>,
    pub(super) phase: JournalPhase,
    pub(super) install_path: PathBuf,
    pub(super) paths: Vec<JournalPath>,
}

/// The Windows helper launch contract is journaled with the file mutations so
/// there is no script or receipt state machine to reconcile during recovery.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WindowsHelperJournal {
    pub(super) parent_pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) helper_pid: Option<u32>,
    pub(super) helper_path: PathBuf,
    pub(super) expected_binary_sha256: String,
    pub(super) expected_marker_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) daemon_restart: Option<WindowsDaemonRestart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) failure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) terminal: Option<WindowsTerminalJournal>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum WindowsTerminalOutcome {
    Applied,
    Failed,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct WindowsTerminalJournal {
    pub(super) outcome: WindowsTerminalOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) warning_or_error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct WindowsDaemonRestart {
    pub(super) trigger: String,
    pub(super) idle_exit_seconds: u64,
    pub(super) loop_interval_seconds: u64,
}

// This is intentionally the only compatibility decoder.  It is the exact
// Unix schema shipped in v0.25.0; later or invented schemas fail closed.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum LegacyJournalPhase {
    Publishing,
    Committed,
}

#[cfg(unix)]
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LegacyJournalPath {
    pub(super) label: String,
    pub(super) staged: PathBuf,
    pub(super) target: PathBuf,
    pub(super) backup: PathBuf,
    pub(super) kind: JournalPathKind,
}

#[cfg(unix)]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LegacyInstallTransactionJournal {
    pub(super) schema_version: u32,
    pub(super) transaction_id: String,
    pub(super) phase: LegacyJournalPhase,
    pub(super) install_path: PathBuf,
    pub(super) paths: Vec<LegacyJournalPath>,
}

impl InstallTransactionJournal {
    pub(super) fn new(
        attempt_id: String,
        data_root: PathBuf,
        runtime_root: PathBuf,
        install_path: PathBuf,
        paths: Vec<JournalPath>,
        windows_helper: Option<WindowsHelperJournal>,
    ) -> Self {
        Self {
            schema_version: INSTALL_TRANSACTION_SCHEMA_VERSION,
            attempt_id,
            data_root,
            runtime_root,
            semantic_cache_root: None,
            windows_helper,
            phase: JournalPhase::Prepared,
            install_path,
            paths,
        }
    }
}

pub(super) fn is_valid_attempt_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

pub(super) fn install_transaction_path(install_path: &Path) -> PathBuf {
    let name = install_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("ctx");
    install_path.with_file_name(format!(".{name}.{INSTALL_TRANSACTION_FILE}"))
}

pub(super) fn read(install_path: &Path) -> Result<Option<InstallTransactionJournal>> {
    let path = install_transaction_path(install_path);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let journal = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse interrupted install transaction {}", path.display()))?;
    Ok(Some(journal))
}

#[cfg(unix)]
pub(super) fn read_legacy(data_root: &Path) -> Result<Option<LegacyInstallTransactionJournal>> {
    let path = data_root.join(INSTALL_TRANSACTION_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let journal = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "parse v0.25 interrupted install transaction {}",
            path.display()
        )
    })?;
    Ok(Some(journal))
}

#[cfg(unix)]
pub(super) fn remove_legacy(data_root: &Path) -> Result<()> {
    let path = data_root.join(INSTALL_TRANSACTION_FILE);
    match fs::remove_file(&path) {
        Ok(()) => sync_removed_parent(data_root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

pub(super) fn write(journal: &InstallTransactionJournal) -> Result<()> {
    #[cfg(unix)]
    {
        if cfg!(debug_assertions)
            && journal.phase == JournalPhase::Committed
            && env_flag("CTX_UPGRADE_FAIL_COMMIT_JOURNAL_WRITE_FOR_TESTS")
        {
            return Err(anyhow!("injected committed journal write failure"));
        }
    }
    let path = install_transaction_path(&journal.install_path);
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("install journal has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{INSTALL_TRANSACTION_FILE}.tmp.{}",
        Uuid::new_v4().simple()
    ));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("create install transaction {}", temporary.display()))?;
        file.write_all(&serde_json::to_vec_pretty(journal)?)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        #[cfg(windows)]
        {
            ctx_history_core::platform_security::restrict_private_file(&temporary)?;
            ctx_history_core::platform_security::verify_private_file(&temporary)?;
        }
        publish_temporary(&temporary, &path, parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn publish_temporary(temporary: &Path, path: &Path, data_root: &Path) -> Result<()> {
    fs::rename(temporary, path).with_context(|| {
        format!(
            "publish install transaction {} to {}",
            temporary.display(),
            path.display()
        )
    })?;
    sync_directory(data_root)
}

#[cfg(windows)]
fn publish_temporary(temporary: &Path, path: &Path, _data_root: &Path) -> Result<()> {
    super::windows::durable_replace_file(temporary, path)
}

#[cfg(not(any(unix, windows)))]
fn publish_temporary(temporary: &Path, path: &Path, _data_root: &Path) -> Result<()> {
    fs::rename(temporary, path).with_context(|| {
        format!(
            "publish install transaction {} to {}",
            temporary.display(),
            path.display()
        )
    })
}

pub(super) fn remove(install_path: &Path) -> Result<()> {
    #[cfg(test)]
    if std::env::var_os("CTX_UPGRADE_FAIL_WINDOWS_JOURNAL_REMOVE_FOR_TESTS").as_deref()
        == Some(install_path.as_os_str())
    {
        return Err(anyhow!("injected Windows journal removal failure"));
    }
    let path = install_transaction_path(install_path);
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("install journal has no parent"))?;
    match fs::remove_file(&path) {
        Ok(()) => sync_removed_parent(parent),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

#[cfg(unix)]
fn sync_removed_parent(data_root: &Path) -> Result<()> {
    sync_directory(data_root)
}

#[cfg(not(unix))]
fn sync_removed_parent(_data_root: &Path) -> Result<()> {
    Ok(())
}

pub(super) fn validate(journal: &InstallTransactionJournal) -> Result<()> {
    validate_identity(journal)?;
    validate_phase_state(journal)?;
    #[cfg(windows)]
    let expected_install_path = current_install_path_for_recovery()?;
    #[cfg(not(windows))]
    let expected_install_path = current_install_path()?;
    if journal.install_path != expected_install_path {
        return Err(anyhow!(
            "install transaction targets {}, expected current managed install {}",
            journal.install_path.display(),
            expected_install_path.display()
        ));
    }
    validate_paths(journal)
}

/// A copied Windows helper cannot use `current_exe()` as an install identity.
/// It instead authenticates the explicit journal target and every invariant
/// shared with ordinary recovery.
#[cfg(windows)]
pub(super) fn validate_for_helper(
    journal: &InstallTransactionJournal,
    install_path: &Path,
) -> Result<()> {
    validate_without_current_executable(journal)?;
    if journal.install_path != install_path {
        return Err(anyhow!(
            "Windows replacement helper targets a different executable"
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_without_current_executable(journal: &InstallTransactionJournal) -> Result<()> {
    validate_identity(journal)?;
    validate_phase_state(journal)?;
    validate_paths(journal)
}

fn validate_paths(journal: &InstallTransactionJournal) -> Result<()> {
    validate_paths_for_platform(journal, super::super::super::platform_key()?)
}

fn validate_paths_for_platform(journal: &InstallTransactionJournal, platform: &str) -> Result<()> {
    if !(2..=6).contains(&journal.paths.len()) {
        return Err(anyhow!("install transaction has an unexpected path count"));
    }
    let binary = optional_one_path(journal, "ctx binary")?;
    let marker = optional_one_path(journal, "ctx install marker")?;
    if binary.is_some() != marker.is_some() {
        return Err(anyhow!(
            "install transaction must publish the ctx binary and marker together"
        ));
    }
    if let Some(binary) = binary {
        validate_binary_path(journal, binary)?;
        validate_marker_path(journal, marker.expect("validated binary/marker pair"))?;
    }
    let runtimes = journal
        .paths
        .iter()
        .filter(|path| path.label == "ONNX Runtime sidecar")
        .collect::<Vec<_>>();
    if runtimes.len() > 1 || (!runtimes.is_empty() && binary.is_none()) {
        return Err(anyhow!("install transaction has invalid runtime paths"));
    }
    if let Some(runtime) = runtimes.first() {
        validate_runtime_path(journal, runtime)?;
    }
    let semantic = journal
        .paths
        .iter()
        .filter(|path| path.label.starts_with("Semantic "))
        .collect::<Vec<_>>();
    if !runtimes.is_empty() && !semantic.is_empty() {
        return Err(anyhow!(
            "install transaction mixes legacy and signed Semantic runtime paths"
        ));
    }
    if semantic.is_empty() {
        if binary.is_none() {
            return Err(anyhow!("install transaction has no publishable paths"));
        }
    } else {
        semantic::validate_paths(journal, &semantic, platform)?;
    }
    if journal.paths.iter().any(|path| {
        !matches!(
            path.label.as_str(),
            "ONNX Runtime sidecar"
                | "ctx binary"
                | "ctx install marker"
                | "Semantic model"
                | "Semantic CPU runtime"
                | "Semantic Windows ML runtime"
                | "Semantic CUDA runtime"
                | "Semantic Core ML bundle"
                | "Semantic Core ML completion marker"
        )
    }) {
        return Err(anyhow!("install transaction has an unknown path label"));
    }
    #[cfg(unix)]
    if journal.windows_helper.is_some()
        || journal.paths.iter().any(|path| {
            path.staged_identity.is_none()
                || path.target_preexisted != path.original_target_identity.is_some()
                || (!path.target_preexisted && path.backup_identity.is_some())
        })
    {
        return Err(anyhow!(
            "Unix install transaction has invalid ownership data"
        ));
    }
    #[cfg(windows)]
    validate_windows_helper(journal)?;
    Ok(())
}

#[cfg(test)]
pub(super) fn validate_paths_for_platform_for_test(
    journal: &InstallTransactionJournal,
    platform: &str,
) -> Result<()> {
    validate_paths_for_platform(journal, platform)
}

#[cfg(windows)]
fn validate_windows_helper(journal: &InstallTransactionJournal) -> Result<()> {
    let helper = journal
        .windows_helper
        .as_ref()
        .ok_or_else(|| anyhow!("Windows install transaction has no helper data"))?;
    let parent = journal
        .install_path
        .parent()
        .ok_or_else(|| anyhow!("Windows install transaction install path has no parent"))?;
    if helper.parent_pid == 0
        || helper.helper_path
            != parent.join(format!(
                ".{}.ctx-upgrade-{}.helper.exe",
                journal
                    .install_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| anyhow!(
                        "Windows install transaction install path has no file name"
                    ))?,
                journal.attempt_id
            ))
        || !valid_sha256(&helper.expected_binary_sha256)
        || !valid_sha256(&helper.expected_marker_sha256)
        || helper.daemon_restart.as_ref().is_some_and(|restart| {
            !matches!(restart.trigger.as_str(), "setup" | "import" | "search")
                || restart.idle_exit_seconds == 0
                || restart.loop_interval_seconds == 0
        })
        || helper
            .failure
            .as_ref()
            .is_some_and(|value| value.len() > 16 * 1024)
        || helper.terminal.as_ref().is_some_and(|terminal| {
            terminal
                .warning_or_error
                .as_ref()
                .is_some_and(|value| value.len() > 16 * 1024)
                || (terminal.outcome == WindowsTerminalOutcome::Failed
                    && terminal.warning_or_error.is_none())
                || match terminal.outcome {
                    WindowsTerminalOutcome::Applied => !journal.phase.committed(),
                    WindowsTerminalOutcome::Failed => !matches!(
                        journal.phase,
                        JournalPhase::RollingBack | JournalPhase::RolledBack | JournalPhase::Failed
                    ),
                }
        })
    {
        return Err(anyhow!(
            "Windows install transaction has invalid helper data"
        ));
    }
    if optional_one_path(journal, "ctx binary")?.is_some_and(|binary| !binary.target_preexisted) {
        return Err(anyhow!(
            "Windows install transaction cannot replace an absent executable"
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(unix)]
pub(super) fn validate_legacy(
    journal: &LegacyInstallTransactionJournal,
    data_root: &Path,
) -> Result<()> {
    if journal.schema_version != 1
        || journal.transaction_id.is_empty()
        || journal.transaction_id.len() > 128
        || !journal
            .transaction_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return Err(anyhow!("invalid v0.25 install transaction identity"));
    }
    if journal.install_path != current_install_path()? {
        return Err(anyhow!(
            "v0.25 install transaction targets a different executable"
        ));
    }
    if journal.paths.len() != 2 && journal.paths.len() != 3 {
        return Err(anyhow!(
            "v0.25 install transaction has an unexpected path count"
        ));
    }
    let binary = legacy_one_path(journal, "ctx binary")?;
    let marker = legacy_one_path(journal, "ctx install marker")?;
    let parent = journal
        .install_path
        .parent()
        .ok_or_else(|| anyhow!("v0.25 install path has no parent"))?;
    if binary.kind != JournalPathKind::File
        || binary.target != journal.install_path
        || binary.staged != parent.join(format!(".ctx-upgrade-{}.new", journal.transaction_id))
        || binary.backup
            != transaction_backup_path(&journal.install_path, &journal.transaction_id, "binary")
    {
        return Err(anyhow!(
            "v0.25 install transaction has invalid binary paths"
        ));
    }
    let marker_path = install_marker_path(&journal.install_path);
    if marker.kind != JournalPathKind::File
        || marker.target != marker_path
        || marker.staged
            != parent.join(format!(
                ".ctx-upgrade-{}.install.json.new",
                journal.transaction_id
            ))
        || marker.backup != transaction_backup_path(&marker_path, &journal.transaction_id, "marker")
    {
        return Err(anyhow!(
            "v0.25 install transaction has invalid marker paths"
        ));
    }
    let runtimes = journal
        .paths
        .iter()
        .filter(|path| path.label == "ONNX Runtime sidecar")
        .collect::<Vec<_>>();
    if (journal.paths.len() == 3) != (runtimes.len() == 1) {
        return Err(anyhow!(
            "v0.25 install transaction has invalid runtime paths"
        ));
    }
    if let Some(runtime) = runtimes.first() {
        // v0.25 did not persist a runtime root.  Recompute it using the
        // exact runtime-root selection contract it used while staging.  If a
        // custom CTX_RUNTIME_DIR from the original attempt is no longer
        // supplied, the journal cannot prove ownership and recovery fails
        // closed instead of touching a guessed default root.
        let runtime_root = super::super::runtime::semantic_runtime_root(data_root)?;
        let name = runtime
            .target
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow!("v0.25 runtime target has no name"))?;
        if runtime.kind != JournalPathKind::Directory
            || runtime.staged
                != runtime.target.with_file_name(format!(
                    ".{name}.ctx-upgrade-{}.new",
                    journal.transaction_id
                ))
            || runtime.backup
                != transaction_backup_path(&runtime.target, &journal.transaction_id, "runtime")
            || !runtime.target.starts_with(runtime_root.join("onnxruntime"))
        {
            return Err(anyhow!(
                "v0.25 install transaction has invalid runtime paths"
            ));
        }
    }
    if journal.paths.iter().any(|path| {
        !matches!(
            path.label.as_str(),
            "ctx binary" | "ctx install marker" | "ONNX Runtime sidecar"
        )
    }) {
        return Err(anyhow!(
            "v0.25 install transaction has an unknown path label"
        ));
    }
    Ok(())
}

pub(super) fn validate_phase_state(journal: &InstallTransactionJournal) -> Result<()> {
    if matches!(
        journal.phase,
        JournalPhase::Prepared | JournalPhase::HelperReady
    ) && journal
        .paths
        .iter()
        .any(|path| path.state != JournalPathState::Staged)
    {
        return Err(anyhow!(
            "install transaction has mutations recorded before publication"
        ));
    }
    if journal.phase.committed()
        && journal.paths.iter().any(|path| {
            !matches!(
                path.state,
                JournalPathState::Published | JournalPathState::Cleaned
            )
        })
    {
        return Err(anyhow!(
            "committed install transaction has an incomplete path state"
        ));
    }
    if journal
        .paths
        .iter()
        .any(|path| !path.target_preexisted && path.state == JournalPathState::BackedUp)
    {
        return Err(anyhow!(
            "install transaction backed up a target recorded as absent"
        ));
    }
    Ok(())
}

fn validate_identity(journal: &InstallTransactionJournal) -> Result<()> {
    if journal.schema_version != INSTALL_TRANSACTION_SCHEMA_VERSION
        || !is_valid_attempt_id(&journal.attempt_id)
        || !journal.data_root.is_absolute()
        || !journal.runtime_root.is_absolute()
        || journal
            .semantic_cache_root
            .as_ref()
            .is_some_and(|root| !root.is_absolute())
    {
        return Err(anyhow!("invalid install transaction identity"));
    }
    #[cfg(unix)]
    {
        validate_owner_private_root(&journal.data_root, "install transaction data root")?;
        if journal
            .paths
            .iter()
            .any(|path| is_runtime_label(&path.label))
        {
            validate_owner_private_root(&journal.runtime_root, "install transaction runtime root")?;
        }
        if journal
            .paths
            .iter()
            .any(|path| path.label.starts_with("Semantic "))
        {
            validate_owner_private_root(
                journal
                    .semantic_cache_root
                    .as_deref()
                    .ok_or_else(|| anyhow!("Semantic install transaction has no cache root"))?,
                "install transaction Semantic cache root",
            )?;
        } else if journal.semantic_cache_root.is_some() {
            return Err(anyhow!(
                "non-Semantic install transaction records a Semantic cache root"
            ));
        }
    }
    #[cfg(windows)]
    {
        validate_windows_private_root(&journal.data_root, "install transaction data root")?;
        if journal
            .paths
            .iter()
            .any(|path| is_runtime_label(&path.label))
        {
            validate_windows_private_root(
                &journal.runtime_root,
                "install transaction runtime root",
            )?;
        }
        if journal
            .paths
            .iter()
            .any(|path| path.label.starts_with("Semantic "))
        {
            validate_windows_private_root(
                journal
                    .semantic_cache_root
                    .as_deref()
                    .ok_or_else(|| anyhow!("Semantic install transaction has no cache root"))?,
                "install transaction Semantic cache root",
            )?;
        } else if journal.semantic_cache_root.is_some() {
            return Err(anyhow!(
                "non-Semantic install transaction records a Semantic cache root"
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn validate_windows_private_root(path: &Path, label: &str) -> Result<()> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("canonicalize {label} {}", path.display()))?;
    if canonical != path {
        return Err(anyhow!("{label} is not canonical: {}", path.display()));
    }
    ctx_history_core::platform_security::verify_private_directory(path)
        .with_context(|| format!("verify {label} {}", path.display()))
}

/// Journal roots are mutation authorities.  Require canonical, owner-private
/// directories before accepting a cross-root recovery record so a copied or
/// symlinked journal cannot redirect scheduler/daemon work.
#[cfg(unix)]
fn validate_owner_private_root(path: &Path, label: &str) -> Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(anyhow!(
            "{label} is not a safe absolute path: {}",
            path.display()
        ));
    }
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("canonicalize {label} {}", path.display()))?;
    if canonical != path {
        return Err(anyhow!("{label} is not canonical: {}", path.display()));
    }
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect {label} {}", path.display()))?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(anyhow!("{label} is not owner-private: {}", path.display()));
    }
    Ok(())
}

fn one_path<'a>(journal: &'a InstallTransactionJournal, label: &str) -> Result<&'a JournalPath> {
    let mut matching = journal.paths.iter().filter(|path| path.label == label);
    let path = matching
        .next()
        .ok_or_else(|| anyhow!("install transaction missing {label}"))?;
    if matching.next().is_some() {
        return Err(anyhow!("install transaction duplicates {label}"));
    }
    Ok(path)
}

fn optional_one_path<'a>(
    journal: &'a InstallTransactionJournal,
    label: &str,
) -> Result<Option<&'a JournalPath>> {
    let mut matching = journal.paths.iter().filter(|path| path.label == label);
    let path = matching.next();
    if matching.next().is_some() {
        return Err(anyhow!("install transaction duplicates {label}"));
    }
    Ok(path)
}

#[cfg(unix)]
fn legacy_one_path<'a>(
    journal: &'a LegacyInstallTransactionJournal,
    label: &str,
) -> Result<&'a LegacyJournalPath> {
    let mut matching = journal.paths.iter().filter(|path| path.label == label);
    let path = matching
        .next()
        .ok_or_else(|| anyhow!("v0.25 install transaction missing {label}"))?;
    if matching.next().is_some() {
        return Err(anyhow!("v0.25 install transaction duplicates {label}"));
    }
    Ok(path)
}

fn validate_binary_path(journal: &InstallTransactionJournal, binary: &JournalPath) -> Result<()> {
    let parent = journal
        .install_path
        .parent()
        .ok_or_else(|| anyhow!("install transaction install path has no parent"))?;
    if binary.kind != JournalPathKind::File
        || binary.target != journal.install_path
        || binary.staged != parent.join(format!(".ctx-upgrade-{}.new", journal.attempt_id))
        || binary.backup
            != transaction_backup_path(&journal.install_path, &journal.attempt_id, "binary")
    {
        return Err(anyhow!("install transaction has invalid binary paths"));
    }
    Ok(())
}

fn validate_marker_path(journal: &InstallTransactionJournal, marker: &JournalPath) -> Result<()> {
    let expected = install_marker_path(&journal.install_path);
    let parent = journal.install_path.parent().expect("validated above");
    if marker.kind != JournalPathKind::File
        || marker.target != expected
        || marker.staged
            != parent.join(format!(
                ".ctx-upgrade-{}.install.json.new",
                journal.attempt_id
            ))
        || marker.backup != transaction_backup_path(&expected, &journal.attempt_id, "marker")
    {
        return Err(anyhow!("install transaction has invalid marker paths"));
    }
    Ok(())
}

fn validate_runtime_path(journal: &InstallTransactionJournal, runtime: &JournalPath) -> Result<()> {
    let name = runtime
        .target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("install transaction runtime target has no file name"))?;
    if runtime.kind != JournalPathKind::Directory
        || runtime.staged
            != runtime
                .target
                .with_file_name(format!(".{name}.ctx-upgrade-{}.new", journal.attempt_id))
        || runtime.backup
            != transaction_backup_path(&runtime.target, &journal.attempt_id, "runtime")
    {
        return Err(anyhow!("install transaction has invalid runtime paths"));
    }
    let expected_root = journal.runtime_root.join("onnxruntime");
    let relative = runtime
        .target
        .strip_prefix(&expected_root)
        .map_err(|_| anyhow!("install transaction runtime is outside the selected runtime root"))?;
    let components = relative.components().collect::<Vec<_>>();
    if components.len() != 2
        || components
            .iter()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || runtime.target.file_name().and_then(|value| value.to_str())
            != Some(super::super::super::platform_key()?)
    {
        return Err(anyhow!("install transaction has invalid runtime identity"));
    }
    Ok(())
}

fn is_runtime_label(label: &str) -> bool {
    matches!(
        label,
        "ONNX Runtime sidecar"
            | "Semantic CPU runtime"
            | "Semantic Windows ML runtime"
            | "Semantic CUDA runtime"
    )
}

pub(super) fn transaction_backup_path(target: &Path, unique: &str, label: &str) -> PathBuf {
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(label);
    target.with_file_name(format!(".{name}.ctx-upgrade-{unique}.{label}.previous"))
}
