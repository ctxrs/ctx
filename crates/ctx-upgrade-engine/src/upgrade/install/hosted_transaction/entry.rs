//! Hosted transaction entry points with one installation-lock owner.
use std::path::PathBuf;

use anyhow::Result;

use super::super::lock::{installation_lock_path, OwnerFileLock};
use super::{
    install, reject_unexpected_inputs, uninstall_arm, uninstall_commit, uninstall_prepare,
    validate_install_path, HostedTransactionAction, HostedTransactionArgs,
};

pub fn run(args: HostedTransactionArgs) -> Result<()> {
    reject_unexpected_inputs(&args)?;
    let install_path = validate_install_path(&args.install_path)?;
    let _installation_lock = OwnerFileLock::acquire(&installation_lock_path(&install_path)?)?;
    run_locked(args, install_path, false)
}

pub(super) fn run_locked(
    args: HostedTransactionArgs,
    install_path: PathBuf,
    migration_owns_state: bool,
) -> Result<()> {
    match args.action {
        HostedTransactionAction::Install => install(args, install_path, migration_owns_state),
        HostedTransactionAction::UninstallPrepare => uninstall_prepare(args, install_path),
        HostedTransactionAction::UninstallArm => uninstall_arm(args, install_path),
        HostedTransactionAction::UninstallCommit => uninstall_commit(args, install_path),
    }
}
