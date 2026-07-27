#[cfg(windows)]
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

#[cfg(windows)]
use super::super::journal::{
    self, InstallTransactionJournal, JournalPathKind, WindowsDaemonRestart,
};

#[cfg(windows)]
const PID_RECORD_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(windows)]
const PID_RECORD_POLL: Duration = Duration::from_millis(10);
const READY_PREFIX: &str = "ctx-upgrade-helper-ready-v1";

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct PathContract {
    label: String,
    staged: PathBuf,
    target: PathBuf,
    backup: PathBuf,
    kind: JournalPathKind,
    target_preexisted: bool,
}

/// Immutable identity compared every time the helper reopens the journal.
/// Durable phase, per-path state, failure text, and helper PID are deliberately
/// excluded because those are the transaction's mutable state.
#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JournalIdentity {
    schema_version: u32,
    attempt_id: String,
    data_root: PathBuf,
    runtime_root: PathBuf,
    install_path: PathBuf,
    parent_pid: u32,
    helper_path: PathBuf,
    expected_binary_sha256: String,
    expected_marker_sha256: String,
    daemon_restart: Option<WindowsDaemonRestart>,
    paths: Vec<PathContract>,
}

#[cfg(windows)]
impl JournalIdentity {
    pub(super) fn from_journal(transaction: &InstallTransactionJournal) -> Result<Self> {
        let helper = transaction
            .windows_helper
            .as_ref()
            .ok_or_else(|| anyhow!("Windows install journal has no replacement helper"))?;
        Ok(Self {
            schema_version: transaction.schema_version,
            attempt_id: transaction.attempt_id.clone(),
            data_root: transaction.data_root.clone(),
            runtime_root: transaction.runtime_root.clone(),
            install_path: transaction.install_path.clone(),
            parent_pid: helper.parent_pid,
            helper_path: helper.helper_path.clone(),
            expected_binary_sha256: helper.expected_binary_sha256.clone(),
            expected_marker_sha256: helper.expected_marker_sha256.clone(),
            daemon_restart: helper.daemon_restart.clone(),
            paths: transaction
                .paths
                .iter()
                .map(|path| PathContract {
                    label: path.label.clone(),
                    staged: path.staged.clone(),
                    target: path.target.clone(),
                    backup: path.backup.clone(),
                    kind: path.kind,
                    target_preexisted: path.target_preexisted,
                })
                .collect(),
        })
    }

    pub(super) fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    pub(super) fn parent_pid(&self) -> u32 {
        self.parent_pid
    }

    pub(super) fn helper_path(&self) -> &Path {
        &self.helper_path
    }
}

#[cfg(windows)]
pub(super) fn prepare_launch(
    transaction: &mut InstallTransactionJournal,
    parent_pid: u32,
) -> Result<JournalIdentity> {
    if parent_pid == 0 {
        return Err(anyhow!(
            "Windows replacement helper parent PID must be nonzero"
        ));
    }
    let helper = transaction
        .windows_helper
        .as_mut()
        .ok_or_else(|| anyhow!("Windows install journal has no replacement helper"))?;
    helper.parent_pid = parent_pid;
    helper.helper_pid = None;
    journal::write(transaction)?;
    journal::validate_for_helper(transaction, &transaction.install_path)?;
    JournalIdentity::from_journal(transaction)
}

#[cfg(windows)]
pub(super) fn record_helper_pid(
    install_path: &Path,
    expected: &JournalIdentity,
    helper_pid: u32,
) -> Result<()> {
    if helper_pid == 0 {
        return Err(anyhow!("Windows replacement helper PID must be nonzero"));
    }
    let mut transaction = read_matching(install_path, expected)?;
    let recorded = transaction
        .windows_helper
        .as_mut()
        .expect("validated Windows helper");
    if recorded.helper_pid.is_some_and(|pid| pid != helper_pid) {
        return Err(anyhow!(
            "Windows replacement journal already names a different helper PID"
        ));
    }
    recorded.helper_pid = Some(helper_pid);
    journal::write(&transaction)?;
    let reread = read_matching(install_path, expected)?;
    require_helper_pid(&reread, helper_pid)
}

#[cfg(windows)]
pub(super) fn wait_for_recorded_helper(
    install_path: &Path,
    expected: &JournalIdentity,
    helper_pid: u32,
) -> Result<InstallTransactionJournal> {
    let deadline = Instant::now() + PID_RECORD_TIMEOUT;
    loop {
        let transaction = read_matching(install_path, expected)?;
        match transaction
            .windows_helper
            .as_ref()
            .and_then(|helper| helper.helper_pid)
        {
            Some(pid) if pid == helper_pid => return Ok(transaction),
            Some(_) => {
                return Err(anyhow!(
                    "Windows replacement journal names a different helper PID"
                ))
            }
            None if Instant::now() < deadline => std::thread::sleep(PID_RECORD_POLL),
            None => {
                return Err(anyhow!(
                    "timed out waiting for durable Windows helper PID handoff"
                ))
            }
        }
    }
}

#[cfg(windows)]
pub(super) fn read_matching(
    install_path: &Path,
    expected: &JournalIdentity,
) -> Result<InstallTransactionJournal> {
    let transaction = journal::read(install_path)?
        .ok_or_else(|| anyhow!("Windows replacement journal disappeared"))?;
    journal::validate_for_helper(&transaction, install_path)?;
    if JournalIdentity::from_journal(&transaction)? != *expected {
        return Err(anyhow!(
            "Windows replacement journal identity changed during helper handoff"
        ));
    }
    Ok(transaction)
}

#[cfg(windows)]
pub(super) fn require_helper_pid(
    transaction: &InstallTransactionJournal,
    helper_pid: u32,
) -> Result<()> {
    if transaction
        .windows_helper
        .as_ref()
        .and_then(|helper| helper.helper_pid)
        != Some(helper_pid)
    {
        return Err(anyhow!(
            "Windows replacement journal does not name the executing helper PID"
        ));
    }
    Ok(())
}

pub(super) fn ready_receipt(attempt_id: &str, helper_pid: u32) -> String {
    format!("{READY_PREFIX} {attempt_id} {helper_pid}\n")
}

pub(super) fn validate_ready_receipt(
    receipt: &str,
    attempt_id: &str,
    helper_pid: u32,
) -> Result<()> {
    if receipt != ready_receipt(attempt_id, helper_pid) {
        return Err(anyhow!(
            "Windows replacement helper returned an invalid readiness receipt"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_receipt_is_exact_and_attempt_bound() {
        let receipt = ready_receipt("attempt_1", 42);
        validate_ready_receipt(&receipt, "attempt_1", 42).unwrap();
        assert!(validate_ready_receipt(&receipt, "attempt_2", 42).is_err());
        assert!(validate_ready_receipt(&receipt, "attempt_1", 43).is_err());
        assert!(validate_ready_receipt(receipt.trim_end(), "attempt_1", 42).is_err());
    }
}
