//! Installer-owned migration into a larger signed executable.
use std::path::Path;

use anyhow::{anyhow, Result};

use crate::upgrade::{
    install::{
        cleanup_legacy_managed_pair_under_installation_lock, hosted_install_journal_exists,
        run_hosted_transaction_under_upgrade_lock, validated_hosted_pair_digest,
        HostedTransactionAction, HostedTransactionArgs,
    },
    state::{
        begin_manual_attempt_locked, finish_hosted_migration_locked, write_state_error_locked,
        write_state_phase_locked, UpgradeLock,
    },
    DaemonUpgradeLease, DaemonUpgradePort, UpgradeEngine,
};

impl<D: DaemonUpgradePort + ?Sized> UpgradeEngine<'_, D> {
    /// The installer has verified signed metadata and the candidate digest.
    /// Quiesce the installed image before its hosted transaction runs.
    pub fn migrate_hosted_install(
        &self,
        data_root: &Path,
        args: HostedTransactionArgs,
    ) -> Result<()> {
        if args.action != HostedTransactionAction::Install || !args.install_path.is_file() {
            return Err(anyhow!(
                "hosted migration requires an existing managed executable"
            ));
        }
        self.prepare_data_root(data_root)?;
        let upgrade_lock = UpgradeLock::acquire_for_installation(&args.install_path)?;
        let attempt = begin_manual_attempt_locked(data_root, &upgrade_lock, "hosted_migration")?;
        let install_path = args.install_path.clone();
        write_state_phase_locked(&upgrade_lock, &attempt, "quiescing")?;
        let handoff =
            match self
                .daemon
                .begin_for_installation(data_root, attempt.id(), &install_path)
            {
                Ok(handoff) => handoff,
                Err(error) => {
                    if !hosted_install_journal_exists(&install_path)? {
                        let _ = write_state_error_locked(
                            data_root,
                            &upgrade_lock,
                            &attempt,
                            "error",
                            &format!("{error:#}"),
                        );
                    }
                    return Err(error);
                }
            };
        match run_hosted_transaction_under_upgrade_lock(args, &upgrade_lock) {
            Ok(()) => {
                finish_hosted_migration_locked(&upgrade_lock, &attempt)?;
                cleanup_legacy_managed_pair_under_installation_lock(&install_path)?;
                handoff.resume_with(&install_path)
            }
            Err(error) => {
                // A journal can describe a new executable with the old marker.
                // Preserve the active scheduler fence until a signed retry finishes it.
                if hosted_install_journal_exists(&install_path)? {
                    return Err(error.context("hosted migration remains fenced pending retry"));
                }
                // Before a journal is written (or after it was fully removed),
                // restart only a complete pair whose digest was checked here.
                if validated_hosted_pair_digest(&install_path).is_err() {
                    return Err(error.context(
                        "hosted migration remains fenced: installed identity is incomplete",
                    ));
                }
                write_state_error_locked(
                    data_root,
                    &upgrade_lock,
                    &attempt,
                    "error",
                    &format!("{error:#}"),
                )?;
                let restart = handoff.resume_with(&install_path);
                match restart {
                    Ok(()) => Err(error),
                    Err(restart_error) => Err(error.context(format!(
                        "also failed to resume daemon lifecycle: {restart_error:#}"
                    ))),
                }
            }
        }
    }
}
