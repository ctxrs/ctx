use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};

use super::super::UpgradePlan;
use super::{
    durability::{stage_downloaded_binary, sync_parent},
    marker::{existing_install_attribution, install_marker_path, write_install_marker_to},
    runtime::{
        stage_downloaded_runtime_artifact, stage_semantic_artifacts, StagedRuntime,
        StagedSemanticInstall,
    },
};
use crate::upgrade::download::DownloadedArtifact;

mod journal;
#[cfg(test)]
mod tests;
#[cfg(unix)]
mod unix;
mod windows;

#[cfg(windows)]
pub(in crate::upgrade) use windows::HelperOutcome;

#[cfg(windows)]
pub(in crate::upgrade) fn run_windows_replacement_helper(
    install_path: &Path,
    attempt_id: &str,
    parent_pid: u32,
) -> Result<HelperOutcome> {
    windows::run_replacement_helper(install_path, attempt_id, parent_pid)
}

#[cfg(unix)]
pub(super) fn discard_legacy_previous_binary(install_path: &Path) -> Result<()> {
    let previous = super::durability::backup_path(install_path);
    unix::remove_owner_regular_file(&previous)?;
    if let Some(parent) = install_path.parent() {
        super::durability::sync_directory(parent)?;
    }
    Ok(())
}

pub(in crate::upgrade) const RECOVERY_REEXEC_ENV: &str = "CTX_UPGRADE_RECOVERY_REEXEC_ATTEMPT";

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(in crate::upgrade) enum ApplyResult {
    Applied,
    AppliedCleanupPending { warning: String },
    Scheduled { helper_pid: u32 },
}

impl ApplyResult {
    #[allow(dead_code)]
    pub(in crate::upgrade) fn cleanup_warning(&self) -> Option<&str> {
        match self {
            Self::AppliedCleanupPending { warning } => Some(warning),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(in crate::upgrade) enum RecoveryOutcome {
    None,
    RolledBack {
        restored_executable: Option<PathBuf>,
    },
    Committed,
    CleanupPending {
        warning: String,
    },
    WindowsHelperScheduled {
        attempt_id: String,
        helper_pid: u32,
    },
}

#[derive(Debug, Clone)]
pub(in crate::upgrade) struct PendingRecovery {
    pub(in crate::upgrade) attempt_id: String,
    /// Validated origin roots from the journal.  An executable-scoped journal
    /// can be discovered by another data root, but recovery must use the
    /// validated roots that created it.
    pub(in crate::upgrade) data_root: PathBuf,
    pub(in crate::upgrade) install_path: PathBuf,
    /// A Windows helper has made the filesystem transaction terminal. The
    /// resumed daemon or explicit manual command still owns scheduler
    /// reconciliation and journal removal.
    pub(in crate::upgrade) terminal: Option<TerminalRecovery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::upgrade) enum TerminalRecovery {
    Applied { warning: Option<String> },
    Failed { error: String },
}

impl RecoveryOutcome {
    #[cfg_attr(windows, allow(dead_code))]
    pub(in crate::upgrade) fn recovered(&self) -> bool {
        !matches!(self, Self::None)
    }

    #[allow(dead_code)]
    pub(in crate::upgrade) fn restored_executable(&self) -> Option<&Path> {
        match self {
            Self::RolledBack {
                restored_executable: Some(path),
            } => Some(path),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub(in crate::upgrade) fn warning(&self) -> Option<&str> {
        match self {
            Self::CleanupPending { warning } => Some(warning),
            _ => None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::upgrade) fn apply_artifact_for_attempt(
    plan: &UpgradePlan,
    artifact: Option<&mut DownloadedArtifact>,
    runtime_artifact: Option<&mut DownloadedArtifact>,
    semantic_artifacts: &mut [DownloadedArtifact],
    data_root: &Path,
    attempt_id: &str,
    daemon_restart: Option<(&str, u64, u64)>,
    before_publish: &mut dyn FnMut() -> Result<()>,
) -> Result<ApplyResult> {
    if !journal::is_valid_attempt_id(attempt_id) {
        return Err(anyhow!("invalid supplied upgrade attempt identity"));
    }
    if journal::read(&plan.install_path)?.is_some() {
        return Err(anyhow!(
            "an interrupted install transaction must be recovered before applying another artifact"
        ));
    }
    if artifact.is_none() && runtime_artifact.is_none() && semantic_artifacts.is_empty() {
        return Err(anyhow!("upgrade transaction has no artifacts to publish"));
    }
    if runtime_artifact.is_some() && !semantic_artifacts.is_empty() {
        return Err(anyhow!(
            "upgrade transaction mixes legacy and signed Semantic runtime artifacts"
        ));
    }
    let parent = plan.install_path.parent().ok_or_else(|| {
        anyhow!(
            "install path has no parent: {}",
            plan.install_path.display()
        )
    })?;
    fs::create_dir_all(parent)?;
    // The release payload was already downloaded and verified before daemon
    // quiesce. These attempt-named files are transaction-private publication
    // inputs; they do not mutate an installed target. The platform publisher
    // journals every path before `before_publish` records `applying` and before
    // any target/backup mutation.
    let staged = artifact
        .as_ref()
        .map(|_| parent.join(format!(".ctx-upgrade-{attempt_id}.new")));
    let marker_path = install_marker_path(&plan.install_path);
    let marker_staged = artifact
        .as_ref()
        .map(|_| parent.join(format!(".ctx-upgrade-{attempt_id}.install.json.new")));
    if let (Some(artifact), Some(staged), Some(marker_staged)) =
        (artifact, staged.as_ref(), marker_staged.as_ref())
    {
        if let Err(error) =
            stage_downloaded_binary(staged, &plan.install_path, artifact, &plan.latest_version)
        {
            remove_unpublished_file(staged);
            return Err(error);
        }
        let install_attribution = existing_install_attribution(&marker_path);
        if let Err(error) =
            write_install_marker_to(marker_staged, plan, install_attribution.as_ref())
        {
            remove_unpublished_file(staged);
            remove_unpublished_file(marker_staged);
            return Err(error);
        }
    }
    let staged_runtime = match runtime_artifact {
        Some(runtime_artifact) => {
            match stage_downloaded_runtime_artifact(plan, runtime_artifact, attempt_id, data_root) {
                Ok(runtime) => Some(runtime),
                Err(error) => {
                    if let Some(staged) = &staged {
                        remove_unpublished_file(staged);
                    }
                    if let Some(marker_staged) = &marker_staged {
                        remove_unpublished_file(marker_staged);
                    }
                    return Err(error);
                }
            }
        }
        None => None,
    };
    let staged_semantic = if semantic_artifacts.is_empty() {
        None
    } else {
        match stage_semantic_artifacts(plan, semantic_artifacts, attempt_id, data_root) {
            Ok(semantic) => Some(semantic),
            Err(error) => {
                if let Some(staged) = &staged {
                    remove_unpublished_file(staged);
                }
                if let Some(marker_staged) = &marker_staged {
                    remove_unpublished_file(marker_staged);
                }
                if let Some(runtime) = &staged_runtime {
                    remove_unpublished_directory(&runtime.staged_path);
                }
                return Err(error);
            }
        }
    };
    let result = publish_install(
        staged.as_deref(),
        plan,
        staged_runtime.as_ref(),
        staged_semantic.as_ref(),
        marker_staged.as_deref(),
        attempt_id,
        data_root,
        daemon_restart,
        before_publish,
    );
    let transaction_retained = journal::install_transaction_path(&plan.install_path)
        .try_exists()
        .unwrap_or(true);
    if result.is_err() && !transaction_retained {
        if let Some(staged) = &staged {
            remove_unpublished_file(staged);
        }
        if let Some(marker_staged) = &marker_staged {
            remove_unpublished_file(marker_staged);
        }
        if let Some(runtime) = &staged_runtime {
            remove_unpublished_directory(&runtime.staged_path);
        }
        if let Some(semantic) = &staged_semantic {
            semantic.cleanup();
        }
    }
    let result = result?;
    sync_parent(parent);
    Ok(result)
}

pub(in crate::upgrade) fn pending_recovery(_data_root: &Path) -> Result<Option<PendingRecovery>> {
    let install_path = recovery_install_path()?;
    if let Some(transaction) = journal::read(&install_path)? {
        journal::validate(&transaction)?;
        let terminal = terminal_recovery(&transaction);
        return Ok(Some(PendingRecovery {
            attempt_id: transaction.attempt_id,
            data_root: transaction.data_root,
            install_path: transaction.install_path,
            terminal,
        }));
    }
    Ok(None)
}

pub(in crate::upgrade) fn remove_terminal_recovery(
    expected: &PendingRecovery,
    _installation_lock: &super::InstallationLock,
) -> Result<()> {
    let _ = read_matching_recovery(expected, true)?;
    journal::remove(&expected.install_path)
}

pub(in crate::upgrade) fn validate_recovery_observation(
    expected: &PendingRecovery,
    terminal: bool,
) -> Result<()> {
    let _ = read_matching_recovery(expected, terminal)?;
    Ok(())
}

pub(in crate::upgrade) fn recover_interrupted_install_outcome(
    expected: &PendingRecovery,
    installation_lock: &super::InstallationLock,
) -> Result<RecoveryOutcome> {
    let mut transaction = read_matching_recovery(expected, false)?;
    #[cfg(unix)]
    let origin_data_root = transaction.data_root.clone();
    #[cfg(unix)]
    {
        let _ = installation_lock;
        unix::recover_transaction(&origin_data_root, &mut transaction)
    }
    #[cfg(windows)]
    {
        windows::recover_transaction(&mut transaction, installation_lock)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (transaction, installation_lock);
        Err(anyhow!(
            "self-upgrade transaction recovery is unsupported on this platform"
        ))
    }
}

fn read_matching_recovery(
    expected: &PendingRecovery,
    terminal: bool,
) -> Result<journal::InstallTransactionJournal> {
    if let Some(transaction) = journal::read(&expected.install_path)? {
        journal::validate(&transaction)?;
        if transaction.attempt_id != expected.attempt_id
            || transaction.data_root != expected.data_root
            || transaction.install_path != expected.install_path
            || terminal_recovery(&transaction) != expected.terminal
            || terminal != expected.terminal.is_some()
        {
            return Err(stale_recovery_observation());
        }
        return Ok(transaction);
    }
    Err(stale_recovery_observation())
}

fn terminal_recovery(transaction: &journal::InstallTransactionJournal) -> Option<TerminalRecovery> {
    transaction
        .windows_helper
        .as_ref()
        .and_then(|helper| helper.terminal.as_ref())
        .map(|terminal| match terminal.outcome {
            journal::WindowsTerminalOutcome::Applied => TerminalRecovery::Applied {
                warning: terminal.warning_or_error.clone(),
            },
            journal::WindowsTerminalOutcome::Failed => TerminalRecovery::Failed {
                error: terminal
                    .warning_or_error
                    .clone()
                    .unwrap_or_else(|| "Windows replacement failed".to_owned()),
            },
        })
}

fn stale_recovery_observation() -> anyhow::Error {
    anyhow!(
        "interrupted ctx installation recovery changed after discovery; refusing stale recovery ownership"
    )
}

fn recovery_install_path() -> Result<PathBuf> {
    #[cfg(windows)]
    return super::marker::current_install_path_for_recovery();
    #[cfg(not(windows))]
    super::marker::current_install_path()
}

#[cfg(unix)]
#[allow(dead_code)]
pub(in crate::upgrade) fn reexec_restored_executable(path: &Path, attempt_id: &str) -> Result<()> {
    unix::reexec_restored_executable(path, attempt_id)
}

#[cfg(not(unix))]
#[allow(dead_code)]
pub(in crate::upgrade) fn reexec_restored_executable(path: &Path, _attempt_id: &str) -> Result<()> {
    Err(anyhow!(
        "re-exec of restored ctx {} is unsupported on this platform",
        path.display()
    ))
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn publish_install(
    staged: Option<&Path>,
    plan: &UpgradePlan,
    staged_runtime: Option<&StagedRuntime>,
    staged_semantic: Option<&StagedSemanticInstall>,
    marker_staged: Option<&Path>,
    attempt_id: &str,
    data_root: &Path,
    _daemon_restart: Option<(&str, u64, u64)>,
    before_publish: &mut dyn FnMut() -> Result<()>,
) -> Result<ApplyResult> {
    unix::publish_install(
        staged,
        plan,
        staged_runtime,
        staged_semantic,
        marker_staged,
        attempt_id,
        data_root,
        before_publish,
    )
}

#[cfg(windows)]
fn publish_install(
    staged: Option<&Path>,
    plan: &UpgradePlan,
    staged_runtime: Option<&StagedRuntime>,
    staged_semantic: Option<&StagedSemanticInstall>,
    marker_staged: Option<&Path>,
    attempt_id: &str,
    data_root: &Path,
    daemon_restart: Option<(&str, u64, u64)>,
    before_publish: &mut dyn FnMut() -> Result<()>,
) -> Result<ApplyResult> {
    windows::publish_install(
        staged,
        plan,
        staged_runtime,
        staged_semantic,
        marker_staged,
        attempt_id,
        data_root,
        daemon_restart,
        before_publish,
    )
}

#[cfg(not(any(unix, windows)))]
fn publish_install(
    _staged: Option<&Path>,
    _plan: &UpgradePlan,
    _staged_runtime: Option<&StagedRuntime>,
    _staged_semantic: Option<&StagedSemanticInstall>,
    _marker_staged: Option<&Path>,
    _attempt_id: &str,
    _data_root: &Path,
    _daemon_restart: Option<(&str, u64, u64)>,
    _before_publish: &mut dyn FnMut() -> Result<()>,
) -> Result<ApplyResult> {
    Err(anyhow!(
        "self-upgrade replacement is unsupported on this platform"
    ))
}

fn remove_unpublished_file(path: &Path) {
    #[cfg(unix)]
    {
        let _ = unix::remove_owner_regular_file(path);
    }
    #[cfg(windows)]
    {
        let _ = windows::remove_unpublished_file(path);
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = fs::remove_file(path);
    }
}

fn remove_unpublished_directory(path: &Path) {
    #[cfg(unix)]
    {
        let _ = unix::remove_owner_directory_tree(path);
    }
    #[cfg(windows)]
    {
        let _ = windows::remove_unpublished_directory(path);
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = fs::remove_dir_all(path);
    }
}
