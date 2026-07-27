#![cfg(any(windows, test))]

//! Windows deferred replacement coordinator.
//!
//! The executable-scoped journal is the only transaction authority. A copied
//! Rust helper validates a bounded readiness receipt, inherits the origin-root
//! daemon contract, waits for its parent, and then reconciles the durable
//! filesystem phase under the executable installation lock.

use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

use super::journal::{
    self, InstallTransactionJournal, JournalPhase, WindowsTerminalJournal, WindowsTerminalOutcome,
};
#[cfg(windows)]
use super::{
    journal::{WindowsDaemonRestart, WindowsHelperJournal},
    ApplyResult, RecoveryOutcome,
};
use anyhow::Result;
#[cfg(windows)]
use anyhow::{anyhow, Context};

#[cfg(windows)]
mod helper;
mod layout;
mod protocol;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::upgrade) enum HelperOutcome {
    Applied { warning: Option<String> },
    Failed { error: String },
}

#[cfg(windows)]
pub(super) fn publish_install(
    staged: Option<&Path>,
    plan: &super::super::super::UpgradePlan,
    staged_runtime: Option<&super::super::runtime::StagedRuntime>,
    staged_semantic: Option<&super::super::runtime::StagedSemanticInstall>,
    marker_staged: Option<&Path>,
    attempt_id: &str,
    data_root: &Path,
    daemon_restart: Option<(&str, u64, u64)>,
    before_publish: &mut dyn FnMut() -> Result<()>,
) -> Result<ApplyResult> {
    helper::cleanup_stale_copies(&plan.install_path)?;
    let helper_path = helper_path(&plan.install_path, attempt_id)?;
    let paths = layout::journal_paths(
        staged,
        plan,
        staged_runtime,
        staged_semantic,
        marker_staged,
        attempt_id,
    )?;
    let has_semantic_runtime = paths.iter().any(|path| {
        matches!(
            path.label.as_str(),
            "Semantic CPU runtime" | "Semantic Windows ML runtime" | "Semantic CUDA runtime"
        )
    });
    let windows_helper = WindowsHelperJournal {
        parent_pid: std::process::id(),
        helper_pid: None,
        helper_path,
        expected_binary_sha256: plan.install_fingerprint.binary_sha256.clone(),
        expected_marker_sha256: plan.install_fingerprint.marker_sha256.clone(),
        daemon_restart: daemon_restart.map(|(trigger, idle_exit, loop_interval)| {
            WindowsDaemonRestart {
                trigger: trigger.to_owned(),
                idle_exit_seconds: idle_exit,
                loop_interval_seconds: loop_interval,
            }
        }),
        failure: None,
        terminal: None,
    };
    let mut transaction = InstallTransactionJournal::new(
        attempt_id.to_owned(),
        std::fs::canonicalize(data_root)
            .with_context(|| format!("canonicalize upgrade data root {}", data_root.display()))?,
        if has_semantic_runtime {
            super::super::runtime::semantic_provisioning_runtime_root(data_root)?
        } else {
            super::super::runtime::semantic_runtime_root(data_root)?
        },
        plan.install_path.clone(),
        paths,
        Some(windows_helper),
    );
    if staged_semantic.is_some() {
        transaction.semantic_cache_root = Some(
            std::fs::canonicalize(super::super::runtime::semantic_cache_root(data_root)?)
                .context("canonicalize selected Semantic cache root")?,
        );
    }
    journal::write(&transaction)?;
    before_publish()?;
    let helper_pid = helper::spawn(&mut transaction, std::process::id())?;
    Ok(ApplyResult::Scheduled { helper_pid })
}

/// Called by the hidden command in the copied executable.
#[cfg(windows)]
pub(in crate::upgrade) fn run_replacement_helper(
    install_path: &Path,
    attempt_id: &str,
    parent_pid: u32,
) -> Result<HelperOutcome> {
    if parent_pid == 0 || parent_pid == std::process::id() {
        return Err(anyhow::anyhow!(
            "Windows replacement helper has an invalid parent PID"
        ));
    }
    let initial = journal::read(install_path)?
        .ok_or_else(|| anyhow!("Windows replacement helper has no install journal"))?;
    journal::validate_for_helper(&initial, install_path)?;
    if initial.attempt_id != attempt_id {
        return Err(anyhow::anyhow!(
            "Windows replacement helper attempt does not match its journal"
        ));
    }
    let expected = protocol::JournalIdentity::from_journal(&initial)?;
    if expected.parent_pid() != parent_pid {
        return Err(anyhow!(
            "Windows replacement helper parent does not match its journal"
        ));
    }
    let current = std::env::current_exe().context("resolve Windows replacement helper path")?;
    if current != expected.helper_path() {
        return Err(anyhow!(
            "Windows replacement helper was not launched from its journaled copy"
        ));
    }

    // Open and retain the parent before any potentially blocking operation.
    // Every OpenProcess failure is fatal or conservatively handled by the
    // caller; a missing handle never means that replacement is safe.
    let parent = helper::ParentProcess::open(parent_pid)?;
    let helper_pid = std::process::id();
    let recorded = protocol::wait_for_recorded_helper(install_path, &expected, helper_pid)?;
    crate::semantic::mark_replacement_helper_handoff(&recorded.data_root, attempt_id, helper_pid)?;
    helper::write_ready(attempt_id, helper_pid)?;

    let installation_lock = super::super::InstallationLock::acquire_for_recovery(install_path)?;
    let mut transaction = protocol::read_matching(install_path, &expected)?;
    protocol::require_helper_pid(&transaction, helper_pid)?;
    if transaction.phase == JournalPhase::Prepared {
        transaction.phase = JournalPhase::HelperReady;
        journal::write(&transaction)?;
    }
    // Publishing and rollback phases are intentionally never regressed.
    parent.wait()?;
    let outcome = execute_transaction(&mut transaction);
    drop(installation_lock);
    if outcome.is_ok() {
        // Terminal state and daemon readiness are already durable. Handoff
        // cleanup is best-effort and cannot turn recovery into a failure.
        let _ = crate::semantic::finish_replacement_daemon_handoff(
            &transaction.data_root,
            &transaction.attempt_id,
        );
    }
    outcome
}

#[cfg(windows)]
pub(super) fn recover_transaction(
    transaction: &mut InstallTransactionJournal,
    _installation_lock: &super::super::InstallationLock,
) -> Result<RecoveryOutcome> {
    layout::repair_executable_presence(transaction)?;
    #[cfg(test)]
    if std::env::var_os("CTX_UPGRADE_WINDOWS_STOP_AFTER_RECOVERY_REPAIR_FOR_TESTS").is_some() {
        return Err(anyhow!("stopped after Windows recovery executable repair"));
    }
    helper::cleanup_stale_copies(&transaction.install_path)?;
    let helper_pid = helper::spawn(transaction, std::process::id())?;
    Ok(RecoveryOutcome::WindowsHelperScheduled {
        attempt_id: transaction.attempt_id.clone(),
        helper_pid,
    })
}

fn execute_transaction(transaction: &mut InstallTransactionJournal) -> Result<HelperOutcome> {
    match transaction.phase {
        JournalPhase::Committed | JournalPhase::CleanupPending => finalize_committed(transaction),
        JournalPhase::RolledBack => finalize_rollback(transaction),
        JournalPhase::RollingBack | JournalPhase::Failed => {
            let failure = transaction
                .windows_helper
                .as_ref()
                .and_then(|helper| helper.failure.clone())
                .unwrap_or_else(|| "previous Windows replacement publication failed".to_owned());
            rollback_after_failure(transaction, failure)
        }
        JournalPhase::Prepared | JournalPhase::HelperReady | JournalPhase::Publishing => {
            let publication = (|| -> Result<()> {
                layout::revalidate_fingerprint(transaction)?;
                if transaction.phase != JournalPhase::Publishing {
                    transaction.phase = JournalPhase::Publishing;
                    journal::write(transaction)?;
                }
                layout::publish_paths(transaction)?;
                transaction.phase = JournalPhase::Committed;
                transaction
                    .windows_helper
                    .as_mut()
                    .expect("validated Windows helper")
                    .failure = None;
                journal::write(transaction)
            })();
            match publication {
                Ok(()) => finalize_committed(transaction),
                Err(error) => rollback_after_failure(transaction, format!("{error:#}")),
            }
        }
    }
}

fn rollback_after_failure(
    transaction: &mut InstallTransactionJournal,
    failure: String,
) -> Result<HelperOutcome> {
    transaction.phase = JournalPhase::RollingBack;
    transaction
        .windows_helper
        .as_mut()
        .expect("validated Windows helper")
        .failure = Some(failure);
    journal::write(transaction)?;
    if let Err(error) = layout::rollback_paths(transaction) {
        let helper = transaction
            .windows_helper
            .as_mut()
            .expect("validated Windows helper");
        let combined = format!(
            "{}; rollback failures: {error:#}",
            helper.failure.as_deref().unwrap_or("replacement failed")
        );
        helper.failure = Some(combined.clone());
        transaction.phase = JournalPhase::Failed;
        let _ = journal::write(transaction);
        return finish_failed(transaction, combined);
    }
    transaction.phase = JournalPhase::RolledBack;
    journal::write(transaction)?;
    finalize_rollback(transaction)
}

fn finalize_rollback(transaction: &mut InstallTransactionJournal) -> Result<HelperOutcome> {
    let mut failure = transaction
        .windows_helper
        .as_ref()
        .and_then(|helper| helper.failure.clone())
        .unwrap_or_else(|| "Windows replacement was rolled back".to_owned());
    if let Err(error) = restart_daemon(transaction) {
        failure.push_str(&format!("; daemon restart remains pending: {error:#}"));
    }
    finish_failed(transaction, failure)
}

fn finalize_committed(transaction: &mut InstallTransactionJournal) -> Result<HelperOutcome> {
    let cleanup_warning = layout::finish_committed(transaction)?;
    let restart_warning = restart_daemon(transaction)
        .err()
        .map(|error| format!("replacement daemon restart remains pending: {error:#}"));
    let warning = merge_warnings(cleanup_warning, restart_warning);
    finish_terminal(
        transaction,
        WindowsTerminalOutcome::Applied,
        warning.clone(),
    )?;
    Ok(HelperOutcome::Applied { warning })
}

fn finish_failed(
    transaction: &mut InstallTransactionJournal,
    failure: String,
) -> Result<HelperOutcome> {
    finish_terminal(
        transaction,
        WindowsTerminalOutcome::Failed,
        Some(failure.clone()),
    )?;
    Ok(HelperOutcome::Failed { error: failure })
}

fn finish_terminal(
    transaction: &mut InstallTransactionJournal,
    outcome: WindowsTerminalOutcome,
    warning_or_error: Option<String>,
) -> Result<()> {
    let helper = transaction
        .windows_helper
        .as_mut()
        .expect("validated Windows helper");
    if let Some(terminal) = &helper.terminal {
        if terminal.outcome != outcome {
            return Err(anyhow::anyhow!(
                "Windows replacement terminal outcome conflicts with its journal"
            ));
        }
        return Ok(());
    }
    helper.terminal = Some(WindowsTerminalJournal {
        outcome,
        warning_or_error,
    });
    journal::write(transaction)
}

fn merge_warnings(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(first), Some(second)) => Some(format!("{first}; {second}")),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[cfg(windows)]
fn restart_daemon(transaction: &InstallTransactionJournal) -> Result<()> {
    let helper = transaction
        .windows_helper
        .as_ref()
        .expect("validated Windows helper");
    let restart = helper.daemon_restart.as_ref().map(|restart| {
        (
            restart.trigger.as_str(),
            restart.idle_exit_seconds,
            restart.loop_interval_seconds,
        )
    });
    crate::semantic::complete_replacement_daemon_handoff(
        &transaction.data_root,
        &transaction.install_path,
        &transaction.attempt_id,
        restart,
    )
}

#[cfg(all(test, not(windows)))]
fn restart_daemon(_transaction: &InstallTransactionJournal) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn helper_path(install_path: &Path, attempt_id: &str) -> Result<PathBuf> {
    let parent = install_path
        .parent()
        .ok_or_else(|| anyhow!("ctx install path has no parent"))?;
    let name = install_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("ctx install path has no file name"))?;
    Ok(parent.join(format!(".{name}.ctx-upgrade-{attempt_id}.helper.exe")))
}

#[cfg(windows)]
pub(super) fn durable_replace_file(temporary: &Path, target: &Path) -> Result<()> {
    layout::durable_replace_file(temporary, target)
}

#[cfg(windows)]
pub(super) fn remove_unpublished_file(path: &Path) -> Result<()> {
    layout::remove_unpublished_file(path)
}

#[cfg(windows)]
pub(super) fn remove_unpublished_directory(path: &Path) -> Result<()> {
    layout::remove_unpublished_directory(path)
}
