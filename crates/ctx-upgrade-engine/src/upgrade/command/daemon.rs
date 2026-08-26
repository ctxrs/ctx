//! Shared automatic upgrade state machine.

use super::*;

/// Downloaded and verified automatic-upgrade inputs held under the one
/// installation lease. The daemon keeps serving while these are staged, then
/// drains its services before handing the value to
/// `finish_automatic_upgrade`.
pub struct PreparedAutomaticUpgrade(PreparedAutomaticUpgradeKind);

struct PreparedProvisioningArtifacts {
    runtime: Option<DownloadedArtifact>,
    semantic: Vec<DownloadedArtifact>,
}

impl PreparedAutomaticUpgrade {
    pub fn attempt_id(&self) -> Option<&str> {
        match &self.0 {
            PreparedAutomaticUpgradeKind::Apply { attempt, .. } => Some(attempt.id()),
            PreparedAutomaticUpgradeKind::Recover { recovery, .. } => Some(&recovery.attempt_id),
        }
    }

    pub fn data_root(&self) -> &Path {
        match &self.0 {
            PreparedAutomaticUpgradeKind::Apply { data_root, .. } => data_root,
            PreparedAutomaticUpgradeKind::Recover { recovery, .. } => &recovery.data_root,
        }
    }

    pub fn install_path(&self) -> &Path {
        match &self.0 {
            PreparedAutomaticUpgradeKind::Apply { plan, .. } => &plan.install_path,
            PreparedAutomaticUpgradeKind::Recover { recovery, .. } => &recovery.install_path,
        }
    }

    pub fn abort(self, error: &anyhow::Error) -> Result<()> {
        let (data_root, lock, attempt) = match self.0 {
            PreparedAutomaticUpgradeKind::Apply {
                data_root,
                lock,
                attempt,
                ..
            } => (data_root, lock, attempt),
            PreparedAutomaticUpgradeKind::Recover {
                recovery,
                lock,
                attempt,
                ..
            } => (recovery.data_root, lock, attempt),
        };
        let durable =
            write_state_error_locked(&data_root, &lock, &attempt, "failed", &format!("{error:#}"))?;
        drop(lock);
        if !durable {
            return Err(anyhow!(
                "automatic upgrade attempt changed before abort could be recorded"
            ));
        }
        Ok(())
    }
}

#[allow(clippy::large_enum_variant)]
enum PreparedAutomaticUpgradeKind {
    Apply {
        data_root: PathBuf,
        interval: Duration,
        started: Instant,
        lock: UpgradeLock,
        attempt: UpgradeAttempt,
        plan: UpgradePlan,
        artifact: Option<DownloadedArtifact>,
        provisioning: PreparedProvisioningArtifacts,
    },
    Recover {
        recovery: PendingRecovery,
        interval: Duration,
        started: Instant,
        lock: UpgradeLock,
        attempt: UpgradeAttempt,
    },
}

/// Claim and stage one automatic attempt. Persistent daemons and detached
/// invocation workers both enter through this installation-scoped scheduler.
pub(crate) fn prepare_automatic_upgrade<D, P, O>(
    engine: &UpgradeEngine<'_, D>,
    policy_provider: &P,
    observer: &O,
    data_root: &Path,
    startup_policy: &P::Snapshot,
) -> Result<Option<PreparedAutomaticUpgrade>>
where
    D: DaemonUpgradePort + ?Sized,
    P: AutomaticUpgradePolicyProvider,
    O: UpgradeObserver<P::Snapshot>,
{
    let started = Instant::now();
    let current = policy_provider.reload(data_root)?;
    if !startup_policy.automatic_upgrade_enabled() || !current.automatic_upgrade_enabled() {
        return Ok(None);
    }
    if let Some(recovery) = pending_recovery(data_root, engine.semantic_layout)? {
        if let Some(terminal) = recovery.terminal.as_ref() {
            let Some(lock) =
                UpgradeLock::try_acquire_terminal_recovery(&recovery, engine.semantic_layout)?
            else {
                return Ok(None);
            };
            let current = policy_provider.reload(data_root)?;
            let (applied, detail) = match terminal {
                TerminalRecovery::Applied { warning } => (true, warning.as_deref()),
                TerminalRecovery::Failed { error } => (false, Some(error.as_str())),
            };
            let automatic = reconcile_replacement_terminal_locked(
                &lock,
                &recovery.attempt_id,
                applied,
                detail,
                current.interval(),
            )?;
            remove_terminal_recovery(&recovery, lock.installation(), engine.semantic_layout)?;
            drop(lock);
            if automatic {
                send_daemon_upgrade_terminal(
                    observer,
                    data_root,
                    &current,
                    None,
                    &recovery.attempt_id,
                    if applied {
                        UpgradeTerminalStatus::Applied
                    } else {
                        UpgradeTerminalStatus::Failed
                    },
                    applied,
                    if applied {
                        None
                    } else {
                        Some(UpgradeFailureKind::ApplyFailed)
                    },
                    started.elapsed(),
                );
            }
            return Ok(None);
        }
        let Some(lock) = UpgradeLock::try_acquire_recovery(&recovery, engine.semantic_layout)?
        else {
            return Ok(None);
        };
        let current = policy_provider.reload(data_root)?;
        if !current.automatic_upgrade_enabled() {
            return Ok(None);
        }
        let attempt = begin_recovery_attempt_locked(&lock, &recovery.attempt_id, "automatic")?;
        return Ok(Some(PreparedAutomaticUpgrade(
            PreparedAutomaticUpgradeKind::Recover {
                recovery,
                interval: current.interval(),
                started,
                lock,
                attempt,
            },
        )));
    }
    let (attempt, lock) = match claim_automatic_upgrade(current.interval())? {
        AutoUpgradeClaim::Claimed { attempt, lock } => (attempt, lock),
        AutoUpgradeClaim::NotDue | AutoUpgradeClaim::Contended => return Ok(None),
    };
    let mut attempt = Some(attempt);
    let mut lock = Some(lock);
    let prepared = (|| -> Result<Option<PreparedAutomaticUpgrade>> {
        let policy = UpgradePolicy {
            channel: current.channel(),
            interval: current.interval(),
            semantic_enabled: current.semantic_enabled(),
        };
        let plan = build_upgrade_plan(engine, policy, None, true)?;
        let repairs = classify_repair_requirements(
            engine.semantic_layout,
            &plan,
            data_root,
            policy.semantic_enabled,
        )?;
        if !plan.update_available && !repairs.any() {
            write_state_checked_locked(
                data_root,
                lock.as_ref().unwrap(),
                attempt.as_ref().unwrap(),
                &plan,
                "up_to_date",
                current.interval(),
            )?;
            drop(lock.take());
            send_daemon_upgrade_terminal(
                observer,
                data_root,
                &current,
                Some(&plan),
                attempt.as_ref().unwrap().id(),
                UpgradeTerminalStatus::UpToDate,
                false,
                None,
                started.elapsed(),
            );
            return Ok(None);
        }
        if (plan.update_available && !plan.metadata.self_upgrade_allowed)
            || !plan.metadata.auto_upgrade_allowed
        {
            return Err(anyhow!(
                "release {} does not allow automatic self-upgrade",
                plan.latest_version
            ));
        }
        if plan.update_available
            && plan.semantic_provisioning.is_none()
            && plan.metadata.onnxruntime.is_none()
        {
            return Err(anyhow!(
                "release {} has no complete ONNX Runtime sidecar metadata",
                plan.latest_version
            ));
        }
        let artifact = if plan.update_available {
            Some(
                DownloadedArtifact::download_verified(
                    engine.transport,
                    data_root,
                    &plan.artifact_url,
                    &plan.artifact_sha256,
                    RELEASE_ARTIFACT_MAX_BYTES as u64,
                    RELEASE_ARTIFACT_TIMEOUT,
                )
                .with_context(|| format!("download {}", plan.artifact_url))?,
            )
        } else {
            None
        };
        let runtime_artifact = if (plan.update_available || repairs.legacy_runtime)
            && plan.semantic_provisioning.is_none()
        {
            match (
                plan.metadata.onnxruntime.as_ref(),
                plan.onnxruntime_artifact_url(),
            ) {
                (Some(runtime), Some(runtime_url)) => Some(
                    DownloadedArtifact::download_or_reuse_verified(
                        engine.transport,
                        data_root,
                        &runtime_url,
                        &runtime.sha256,
                        RELEASE_ONNXRUNTIME_ARTIFACT_MAX_BYTES as u64,
                        RELEASE_ARTIFACT_TIMEOUT,
                    )
                    .with_context(|| format!("download or reuse {runtime_url}"))?,
                ),
                _ => return Err(anyhow!("incomplete ONNX Runtime upgrade plan")),
            }
        } else {
            None
        };
        let mut semantic_artifacts = Vec::new();
        if repairs.catalog {
            let provisioning = plan
                .semantic_provisioning
                .as_ref()
                .ok_or_else(|| anyhow!("Semantic repair has no signed provisioning plan"))?;
            for asset in &provisioning.assets {
                let url = plan.semantic_artifact_url(&asset.metadata.artifact);
                semantic_artifacts.push(
                    DownloadedArtifact::download_or_reuse_verified(
                        engine.transport,
                        data_root,
                        &url,
                        &asset.metadata.archive_sha256,
                        semantic_archive_download_limit(&asset.metadata)?,
                        RELEASE_ARTIFACT_TIMEOUT,
                    )
                    .with_context(|| format!("download or reuse {url}"))?,
                );
            }
        }
        write_state_checked_locked(
            data_root,
            lock.as_ref().unwrap(),
            attempt.as_ref().unwrap(),
            &plan,
            "staged",
            current.interval(),
        )?;
        Ok(Some(PreparedAutomaticUpgrade(
            PreparedAutomaticUpgradeKind::Apply {
                data_root: data_root.to_path_buf(),
                interval: current.interval(),
                started,
                lock: lock.take().unwrap(),
                attempt: attempt.take().unwrap(),
                plan,
                artifact,
                provisioning: PreparedProvisioningArtifacts {
                    runtime: runtime_artifact,
                    semantic: semantic_artifacts,
                },
            },
        )))
    })();
    match prepared {
        Ok(prepared) => Ok(prepared),
        Err(error) => {
            let durable = matches!(
                write_state_error_locked(
                    data_root,
                    lock.as_ref().unwrap(),
                    attempt.as_ref().unwrap(),
                    "failed",
                    &format!("{error:#}"),
                ),
                Ok(true)
            );
            drop(lock.take());
            if durable {
                send_daemon_upgrade_terminal(
                    observer,
                    data_root,
                    &current,
                    None,
                    attempt.as_ref().unwrap().id(),
                    UpgradeTerminalStatus::Failed,
                    false,
                    Some(upgrade_failure_kind(&error)),
                    started.elapsed(),
                );
            }
            Err(error)
        }
    }
}

/// Completes a staged automatic attempt after daemon lifecycle handoff has
/// quiesced every process using this installation.
pub(crate) fn finish_automatic_upgrade<D, P, O>(
    engine: &UpgradeEngine<'_, D>,
    policy_provider: &P,
    observer: &O,
    prepared: PreparedAutomaticUpgrade,
    handoff: Option<D::Lease>,
) -> Result<()>
where
    D: DaemonUpgradePort + ?Sized,
    P: AutomaticUpgradePolicyProvider,
    O: UpgradeObserver<P::Snapshot>,
{
    let handoff = match handoff {
        Some(handoff) => handoff,
        None => {
            let error = anyhow!("automatic upgrade has no daemon lifecycle handoff");
            let abort_error = prepared.abort(&error).err();
            return Err(with_automatic_cleanup_errors(error, abort_error, None));
        }
    };
    let restart = handoff
        .replacement_restart()
        .map(|restart| (restart.trigger, restart.loop_interval_seconds));
    let (data_root, interval, started, lock, attempt, plan, mut artifact, mut provisioning) =
        match prepared.0 {
            PreparedAutomaticUpgradeKind::Apply {
                data_root,
                interval,
                started,
                lock,
                attempt,
                plan,
                artifact,
                provisioning,
            } => (
                data_root,
                interval,
                started,
                lock,
                attempt,
                plan,
                artifact,
                provisioning,
            ),
            PreparedAutomaticUpgradeKind::Recover {
                recovery,
                interval,
                started,
                lock,
                attempt,
            } => {
                let current = match policy_provider.reload(&recovery.data_root) {
                    Ok(current) => current,
                    Err(error) => {
                        return fail_automatic_before_apply(
                            &recovery.data_root,
                            lock,
                            attempt,
                            handoff,
                            &recovery.install_path,
                            error,
                        );
                    }
                };
                if !current.automatic_upgrade_enabled() {
                    if let Err(error) = reconcile_replacement_terminal_locked(
                        &lock,
                        &recovery.attempt_id,
                        false,
                        Some("automatic interrupted-install recovery was disabled"),
                        interval,
                    ) {
                        return fail_automatic_before_apply(
                            &recovery.data_root,
                            lock,
                            attempt,
                            handoff,
                            &recovery.install_path,
                            error,
                        );
                    }
                    drop(lock);
                    return handoff.resume_with(&recovery.install_path);
                }
                let recovery_result = match recover_interrupted_install(
                    engine.process,
                    &recovery,
                    lock.installation(),
                    engine.semantic_layout,
                ) {
                    Ok(result) => result,
                    Err(error) => {
                        return fail_automatic_before_apply(
                            &recovery.data_root,
                            lock,
                            attempt,
                            handoff,
                            &recovery.install_path,
                            error,
                        );
                    }
                };
                return match recovery_result {
                    InstallRecovery::None => fail_automatic_before_apply(
                        &recovery.data_root,
                        lock,
                        attempt,
                        handoff,
                        &recovery.install_path,
                        anyhow!("interrupted ctx installation recovery disappeared while owned"),
                    ),
                    InstallRecovery::Recovered { committed } => {
                        let detail = (!committed).then_some(CURRENT_FORMAT_ROLLBACK_DETAIL);
                        let automatic = match reconcile_replacement_terminal_locked(
                            &lock,
                            &recovery.attempt_id,
                            committed,
                            detail,
                            interval,
                        ) {
                            Ok(automatic) => automatic,
                            Err(error) => {
                                return fail_automatic_before_apply(
                                    &recovery.data_root,
                                    lock,
                                    attempt,
                                    handoff,
                                    &recovery.install_path,
                                    error,
                                );
                            }
                        };
                        drop(lock);
                        let resumed = handoff.resume_with(&recovery.install_path);
                        if automatic {
                            send_daemon_upgrade_terminal(
                                observer,
                                &recovery.data_root,
                                &current,
                                None,
                                &recovery.attempt_id,
                                if committed {
                                    UpgradeTerminalStatus::Applied
                                } else {
                                    UpgradeTerminalStatus::Failed
                                },
                                committed,
                                (!committed).then_some(UpgradeFailureKind::ApplyFailed),
                                started.elapsed(),
                            );
                        }
                        resumed
                    }
                    #[cfg(windows)]
                    InstallRecovery::Scheduled { helper_pid, .. } => {
                        handoff.transfer_to_replacement_helper(helper_pid)
                    }
                    #[cfg(unix)]
                    InstallRecovery::ReexecCurrentFormat(reexec) => {
                        let automatic = match reconcile_replacement_terminal_locked(
                            &lock,
                            &recovery.attempt_id,
                            false,
                            Some(CURRENT_FORMAT_ROLLBACK_DETAIL),
                            interval,
                        ) {
                            Ok(automatic) => automatic,
                            Err(error) => {
                                return fail_automatic_before_apply(
                                    &recovery.data_root,
                                    lock,
                                    attempt,
                                    handoff,
                                    &recovery.install_path,
                                    error,
                                );
                            }
                        };
                        if automatic {
                            send_daemon_upgrade_terminal(
                                observer,
                                &recovery.data_root,
                                &current,
                                None,
                                &recovery.attempt_id,
                                UpgradeTerminalStatus::Failed,
                                false,
                                Some(UpgradeFailureKind::ApplyFailed),
                                started.elapsed(),
                            );
                        }
                        drop(lock);
                        super::continue_current_format_recovery_reexec(
                            engine.process,
                            handoff,
                            reexec,
                        )
                    }
                };
            }
        };
    let current = match policy_provider.reload(&data_root) {
        Ok(current) => current,
        Err(error) => {
            return fail_automatic_before_apply(
                &data_root,
                lock,
                attempt,
                handoff,
                &plan.install_path,
                error,
            );
        }
    };
    if !current.automatic_upgrade_enabled() {
        if let Err(error) =
            write_state_checked_locked(&data_root, &lock, &attempt, &plan, "disabled", interval)
        {
            return fail_automatic_before_apply(
                &data_root,
                lock,
                attempt,
                handoff,
                &plan.install_path,
                error,
            );
        }
        drop(lock);
        let restart = handoff.resume_with(&plan.install_path);
        send_daemon_upgrade_terminal(
            observer,
            &data_root,
            &current,
            Some(&plan),
            attempt.id(),
            UpgradeTerminalStatus::Skipped,
            false,
            None,
            started.elapsed(),
        );
        return restart;
    }
    if let Err(error) = write_state_phase_locked(&lock, &attempt, "quiescing") {
        return fail_automatic_before_apply(
            &data_root,
            lock,
            attempt,
            handoff,
            &plan.install_path,
            error,
        );
    }
    let mut record_applying = || {
        write_state_phase_locked(&lock, &attempt, "applying")?;
        Ok(())
    };
    let result = apply_artifact(
        engine.process,
        engine.semantic_layout,
        lock.installation(),
        &plan,
        artifact.as_mut(),
        provisioning.runtime.as_mut(),
        &mut provisioning.semantic,
        &data_root,
        attempt.id(),
        restart,
        &mut record_applying,
    );
    match result {
        Ok(ApplyResult::Scheduled { helper_pid }) => {
            let _ = write_state_checked_locked(
                &data_root,
                &lock,
                &attempt,
                &plan,
                "scheduled",
                interval,
            );
            handoff.transfer_to_replacement_helper(helper_pid)?;
            Ok(())
        }
        Ok(result) => {
            let mut warnings = plan.warnings.clone();
            if let Some(warning) = result.cleanup_warning() {
                warnings.push(warning.to_owned());
            }
            if let Err(error) =
                write_state_checked_locked(&data_root, &lock, &attempt, &plan, "applied", interval)
            {
                return fail_automatic_before_apply(
                    &data_root,
                    lock,
                    attempt,
                    handoff,
                    &plan.install_path,
                    error,
                );
            }
            drop(lock);
            let restart = handoff.resume_with(&plan.install_path);
            send_daemon_upgrade_terminal(
                observer,
                &data_root,
                &current,
                Some(&plan),
                attempt.id(),
                UpgradeTerminalStatus::Applied,
                true,
                None,
                started.elapsed(),
            );
            restart
        }
        Err(error) => {
            let durable = matches!(
                write_state_error_locked(
                    &data_root,
                    &lock,
                    &attempt,
                    "failed",
                    &format!("{error:#}"),
                ),
                Ok(true)
            );
            drop(lock);
            let restart_error = handoff.resume_with(&plan.install_path).err();
            if durable {
                send_daemon_upgrade_terminal(
                    observer,
                    &data_root,
                    &current,
                    Some(&plan),
                    attempt.id(),
                    UpgradeTerminalStatus::Failed,
                    false,
                    Some(upgrade_failure_kind(&error)),
                    started.elapsed(),
                );
            }
            match restart_error {
                Some(restart_error) => Err(error.context(format!(
                    "also failed to restart daemon after automatic upgrade failure: {restart_error:#}"
                ))),
                None => Ok(()),
            }
        }
    }
}

fn fail_automatic_before_apply<L: DaemonUpgradeLease>(
    data_root: &Path,
    lock: UpgradeLock,
    attempt: UpgradeAttempt,
    handoff: L,
    install_path: &Path,
    error: anyhow::Error,
) -> Result<()> {
    let state_error =
        match write_state_error_locked(data_root, &lock, &attempt, "failed", &format!("{error:#}"))
        {
            Ok(true) => None,
            Ok(false) => Some(anyhow!(
                "automatic upgrade attempt changed before failure could be recorded"
            )),
            Err(error) => Some(error),
        };
    drop(lock);
    let restart_error = handoff.resume_with(install_path).err();
    Err(with_automatic_cleanup_errors(
        error,
        state_error,
        restart_error,
    ))
}

fn with_automatic_cleanup_errors(
    error: anyhow::Error,
    state_error: Option<anyhow::Error>,
    restart_error: Option<anyhow::Error>,
) -> anyhow::Error {
    let mut cleanup = Vec::new();
    if let Some(state_error) = state_error {
        cleanup.push(format!(
            "failed to terminalize automatic upgrade state: {state_error:#}"
        ));
    }
    if let Some(restart_error) = restart_error {
        cleanup.push(format!(
            "failed to restart daemon after automatic upgrade abort: {restart_error:#}"
        ));
    }
    if cleanup.is_empty() {
        error
    } else {
        error.context(cleanup.join("; "))
    }
}

#[allow(clippy::too_many_arguments)]
fn send_daemon_upgrade_terminal<S, O>(
    observer: &O,
    data_root: &Path,
    policy: &S,
    plan: Option<&UpgradePlan>,
    attempt_id: &str,
    status: UpgradeTerminalStatus,
    applied: bool,
    failure_kind: Option<UpgradeFailureKind>,
    duration: Duration,
) where
    S: AutomaticUpgradePolicySnapshot,
    O: UpgradeObserver<S>,
{
    observer.observe_automatic_terminal(
        data_root,
        policy,
        AutomaticUpgradeObservation {
            plan,
            attempt_id,
            status,
            applied,
            failure_kind,
            duration,
        },
    );
}
