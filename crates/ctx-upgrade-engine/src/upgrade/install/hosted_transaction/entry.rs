//! Hosted transaction entry points with one installation-lock owner.
use std::path::PathBuf;

use anyhow::{Result, bail};

use super::super::lock::{OwnerFileLock, installation_lock_path};
use super::{
    HostedTransactionAction, HostedTransactionArgs, install, uninstall_arm, uninstall_commit,
    uninstall_prepare, validate_install_path,
};

pub fn run(args: HostedTransactionArgs) -> Result<()> {
    reject_unexpected_inputs(&args)?;
    let install_path = validate_install_path(&args.install_path)?;
    let _installation_lock = OwnerFileLock::acquire(&installation_lock_path(&install_path)?)?;
    run_locked(args, install_path, None)
}

pub(super) fn run_locked(
    args: HostedTransactionArgs,
    install_path: PathBuf,
    finish_migration: Option<&mut dyn FnMut() -> Result<()>>,
) -> Result<()> {
    match args.action {
        HostedTransactionAction::Install => install(args, install_path, finish_migration),
        HostedTransactionAction::UninstallPrepare => uninstall_prepare(args, install_path),
        HostedTransactionAction::UninstallArm => uninstall_arm(args, install_path),
        HostedTransactionAction::UninstallCommit => uninstall_commit(args, install_path),
    }
}

pub(super) fn reject_unexpected_inputs(args: &HostedTransactionArgs) -> Result<()> {
    match args.action {
        HostedTransactionAction::Install => {
            if args.attempt_id.is_none()
                || args.marker_source.is_none()
                || args.binary_sha256.is_none()
            {
                bail!("hosted install transaction is missing required inputs");
            }
        }
        HostedTransactionAction::UninstallPrepare => {
            if args.attempt_id.is_none()
                || args.marker_source.is_some()
                || args.ownership_source.is_some()
                || args.binary_sha256.is_some()
            {
                bail!("hosted uninstall preparation has invalid inputs");
            }
        }
        HostedTransactionAction::UninstallArm | HostedTransactionAction::UninstallCommit => {
            if args.attempt_id.is_some()
                || args.marker_source.is_some()
                || args.ownership_source.is_some()
                || args.binary_sha256.is_some()
            {
                bail!("hosted uninstall continuation has invalid inputs");
            }
        }
    }
    Ok(())
}
