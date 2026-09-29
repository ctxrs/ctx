//! Installed-path lock and retry observations for candidate-owned migration.
#[cfg(test)]
use std::cell::Cell;
use std::path::Path;

use anyhow::{anyhow, bail, Result};

use super::{
    complete_install_with_fault, ensure_legacy_pair_transaction_inactive_with_state, journal_path,
    managed_install_path_identity_matches, normalized_sha256, read_journal,
    reject_unexpected_inputs, run_locked, validate_existing_pair_for_install,
    validate_install_path, validate_journal, HostedTransactionArgs, Journal, TransactionKind,
};

#[cfg(test)]
thread_local! {
    static HOSTED_INSTALL_FAULT: Cell<Option<&'static str>> = const { Cell::new(None) };
}

#[cfg(test)]
pub(in crate::upgrade) fn set_hosted_install_fault_for_test(point: Option<&'static str>) {
    HOSTED_INSTALL_FAULT.with(|fault| fault.set(point));
}

pub(in crate::upgrade) fn run_under_upgrade_lock(
    args: HostedTransactionArgs,
    lock: &crate::upgrade::state::UpgradeLock,
) -> Result<()> {
    reject_unexpected_inputs(&args)?;
    let install_path = validate_install_path(&args.install_path)?;
    if !managed_install_path_identity_matches(&install_path, lock.install_path()) {
        bail!("hosted transaction lock does not own the install path");
    }
    run_locked(args, install_path, true)
}

pub(in crate::upgrade) fn hosted_install_journal_exists(install_path: &Path) -> Result<bool> {
    let install_path = validate_install_path(install_path)?;
    Ok(pending_hosted_install(&install_path)?.is_some())
}

pub(in crate::upgrade) fn validate_hosted_migration_under_installation_lock(
    args: &HostedTransactionArgs,
) -> Result<()> {
    reject_unexpected_inputs(args)?;
    let install_path = validate_install_path(&args.install_path)?;
    let journal = pending_hosted_install(&install_path)?;
    if let Some(journal) = &journal {
        let digest = normalized_sha256(args.binary_sha256.as_deref().unwrap_or_default())?;
        if journal.binary_sha256 != digest {
            bail!("an interrupted hosted install records a different signed candidate");
        }
    }
    // Check filesystem transaction ownership even when a hosted retry exists.
    // Its scheduler exception is narrower than the post-admission bypass.
    ensure_legacy_pair_transaction_inactive_with_state(&install_path, true)?;
    crate::upgrade::state::ensure_hosted_install_scheduler_available(
        &install_path,
        journal.is_some(),
    )
}

fn pending_hosted_install(install_path: &Path) -> Result<Option<Journal>> {
    let Some(journal) = read_journal(&journal_path(install_path))? else {
        return Ok(None);
    };
    validate_journal(&journal, install_path, journal.kind)?;
    if journal.kind != TransactionKind::Install {
        bail!("finish the pending hosted uninstall before reinstalling");
    }
    Ok(Some(journal))
}

pub(in crate::upgrade) fn validated_hosted_pair_digest(install_path: &Path) -> Result<String> {
    validate_existing_pair_for_install(install_path)?
        .map(|pair| pair.0)
        .ok_or_else(|| anyhow!("hosted migration has no complete managed installation"))
}

pub(super) fn complete_install(
    source: &Path,
    journal_path: &Path,
    journal: &mut Journal,
) -> Result<()> {
    complete_install_with_fault(source, journal_path, journal, &mut |point| {
        if crate::upgrade::test_harness_enabled()
            && std::env::var("CTX_HOSTED_INSTALL_FAIL_AFTER_FOR_TESTS").as_deref() == Ok(point)
        {
            bail!("injected hosted install fault after {point}");
        }
        #[cfg(test)]
        HOSTED_INSTALL_FAULT.with(|fault| {
            if fault.get() == Some(point) {
                fault.set(None);
                bail!("injected hosted install fault after {point}");
            }
            Ok(())
        })?;
        Ok(())
    })
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::upgrade::{
        install::{path_identity::windows_disk_path_identity, HostedTransactionAction},
        state::UpgradeLock,
    };
    use ctx_history_platform::platform_security::{
        create_private_directory_all, restrict_private_directory,
    };
    use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};

    #[test]
    fn ordinary_windows_migration_path_reaches_the_locked_transaction() -> Result<()> {
        let temp = tempfile::tempdir()?;
        restrict_private_directory(temp.path())?;
        let parent = temp.path().join("bin");
        create_private_directory_all(&parent)?;
        let certified = std::fs::canonicalize(&parent)?.join("ctx.exe");
        let ordinary = PathBuf::from(OsString::from_wide(
            &windows_disk_path_identity(&certified).unwrap(),
        ));
        let original = b"synthetic installed executable";
        std::fs::write(&certified, original)?;
        let lock = UpgradeLock::acquire_for_installation(&ordinary)?;
        let args = |install_path| HostedTransactionArgs {
            action: HostedTransactionAction::Install,
            install_path,
            attempt_id: Some("fixture".into()),
            marker_source: Some(parent.join("unused-marker.json")),
            ownership_source: None,
            binary_sha256: Some("deliberately-invalid-digest".into()),
        };
        // Both certified and ordinary installer spellings must pass ownership
        // and reach the transaction's normal digest validation.
        for path in [&ordinary, &certified] {
            let error = run_under_upgrade_lock(args(path.clone()), &lock).unwrap_err();
            assert!(error.to_string().contains("SHA-256"), "{error:#}");
        }
        let error = run_under_upgrade_lock(args(parent.join("other.exe")), &lock).unwrap_err();
        assert!(error.to_string().contains("lock does not own"), "{error:#}");
        assert_eq!(std::fs::read(&certified)?, original);
        Ok(())
    }
}
