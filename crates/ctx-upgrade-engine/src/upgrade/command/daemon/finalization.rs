use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_automatic_applied<L, S, O>(
    data_root: &Path,
    interval: Duration,
    started: Instant,
    lock: UpgradeLock,
    attempt: &UpgradeAttempt,
    plan: &UpgradePlan,
    current: &S,
    observer: &O,
    handoff: L,
    cleanup_warning: Option<&str>,
) -> Result<()>
where
    L: DaemonUpgradeLease,
    S: AutomaticUpgradePolicySnapshot,
    O: UpgradeObserver<S>,
{
    let mut warnings = plan.warnings.clone();
    if let Some(warning) = cleanup_warning {
        warnings.push(warning.to_owned());
    }
    let state_durable = match write_state_checked_locked(
        data_root, &lock, attempt, plan, "applied", interval,
    ) {
        Ok(true) => true,
        Ok(false) => {
            warnings.push(
                "ctx upgrade is applied, but applied-state finalization is pending: automatic upgrade attempt changed before finalization"
                    .to_owned(),
            );
            false
        }
        Err(error) => {
            warnings.push(format!(
                "ctx upgrade is applied, but applied-state finalization is pending: {error:#}"
            ));
            false
        }
    };
    drop(lock);
    if let Err(error) = handoff.resume_with(&plan.install_path) {
        warnings.push(format!(
            "ctx upgrade is applied, but daemon restart is pending: {error:#}"
        ));
    }
    if state_durable {
        send_daemon_upgrade_terminal(
            observer,
            data_root,
            current,
            Some(plan),
            attempt.id(),
            UpgradeTerminalStatus::Applied,
            true,
            None,
            started.elapsed(),
        );
    }
    observer.observe_automatic_warnings(data_root, current, &warnings);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_automatic_recovery<L, S, O>(
    lock: UpgradeLock,
    data_root: &Path,
    install_path: &Path,
    attempt_id: &str,
    interval: Duration,
    committed: bool,
    failure_detail: Option<&str>,
    observe_without_retry_evidence: bool,
    handoff: L,
    observer: &O,
    current: &S,
    started: Instant,
) -> Result<()>
where
    L: DaemonUpgradeLease,
    S: AutomaticUpgradePolicySnapshot,
    O: UpgradeObserver<S>,
{
    let terminal = reconcile_replacement_terminal_locked(
        &lock,
        attempt_id,
        committed,
        failure_detail,
        interval,
    );
    drop(lock);
    let restart = handoff.resume_with(install_path);
    if committed {
        let mut warnings = Vec::new();
        let automatic = match terminal {
            Ok(automatic) => automatic,
            Err(error) => {
                warnings.push(format!(
                    "ctx upgrade is applied, but applied-state recovery is pending: {error:#}"
                ));
                observe_without_retry_evidence
            }
        };
        if let Err(error) = restart {
            warnings.push(format!(
                "ctx upgrade is applied, but daemon restart is pending: {error:#}"
            ));
        }
        observer.observe_automatic_warnings(data_root, current, &warnings);
        if automatic {
            send_daemon_upgrade_terminal(
                observer,
                data_root,
                current,
                None,
                attempt_id,
                UpgradeTerminalStatus::Applied,
                true,
                None,
                started.elapsed(),
            );
        }
        return Ok(());
    }
    let automatic = terminal?;
    if automatic {
        send_daemon_upgrade_terminal(
            observer,
            data_root,
            current,
            None,
            attempt_id,
            UpgradeTerminalStatus::Failed,
            false,
            Some(UpgradeFailureKind::ApplyFailed),
            started.elapsed(),
        );
    }
    restart
}
