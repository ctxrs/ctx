use super::*;

/// Downloaded and verified automatic-upgrade inputs held under the one
/// installation lease. The daemon keeps serving while these are staged, then
/// drains its services before handing the value to
/// `finish_daemon_auto_upgrade`.
pub(crate) struct PreparedDaemonUpgrade(PreparedDaemonUpgradeKind);

struct PreparedProvisioningArtifacts {
    runtime: Option<DownloadedArtifact>,
    semantic: Vec<DownloadedArtifact>,
}

impl PreparedDaemonUpgrade {
    pub(crate) fn attempt_id(&self) -> Option<&str> {
        match &self.0 {
            PreparedDaemonUpgradeKind::Apply { attempt, .. } => Some(attempt.id()),
            PreparedDaemonUpgradeKind::Recover { recovery, .. } => Some(&recovery.attempt_id),
        }
    }
}

#[allow(clippy::large_enum_variant)]
enum PreparedDaemonUpgradeKind {
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
    },
}

/// Called only by the ready enabled long-lived daemon. Foreground commands do
/// not have access to this automatic scheduling entry point.
pub(crate) fn prepare_daemon_auto_upgrade(
    data_root: &Path,
    config: &AppConfig,
) -> Result<Option<PreparedDaemonUpgrade>> {
    let started = Instant::now();
    let current = AppConfig::load(data_root)?;
    if !config.daemon.enabled
        || !current.daemon.enabled
        || !config.auto_upgrade_enabled()
        || !current.auto_upgrade_enabled()
    {
        return Ok(None);
    }
    if let Some(recovery) = pending_recovery(data_root)? {
        if let Some(terminal) = recovery.terminal.as_ref() {
            let Some(lock) = UpgradeLock::try_acquire_terminal_recovery(&recovery)? else {
                return Ok(None);
            };
            let current = AppConfig::load(data_root)?;
            let (applied, detail) = match terminal {
                TerminalRecovery::Applied { warning } => (true, warning.as_deref()),
                TerminalRecovery::Failed { error } => (false, Some(error.as_str())),
            };
            let automatic = reconcile_replacement_terminal_locked(
                &lock,
                &recovery.attempt_id,
                applied,
                detail,
                current.upgrade.interval,
            )?;
            remove_terminal_recovery(&recovery, lock.installation())?;
            drop(lock);
            if automatic {
                send_daemon_upgrade_terminal(
                    data_root,
                    &current,
                    None,
                    &recovery.attempt_id,
                    if applied {
                        UpgradeStatus::Applied
                    } else {
                        UpgradeStatus::Failed
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
        let Some(lock) = UpgradeLock::try_acquire_recovery(&recovery)? else {
            return Ok(None);
        };
        let current = AppConfig::load(data_root)?;
        if !current.daemon.enabled || !current.auto_upgrade_enabled() {
            return Ok(None);
        }
        begin_recovery_attempt_locked(&lock, &recovery.attempt_id, "daemon")?;
        return Ok(Some(PreparedDaemonUpgrade(
            PreparedDaemonUpgradeKind::Recover {
                recovery,
                interval: current.upgrade.interval,
                started,
                lock,
            },
        )));
    }
    let (attempt, lock) = match claim_daemon_auto_upgrade(current.upgrade.interval)? {
        AutoUpgradeClaim::Claimed { attempt, lock } => (attempt, lock),
        AutoUpgradeClaim::NotDue | AutoUpgradeClaim::Contended => return Ok(None),
    };
    let mut attempt = Some(attempt);
    let mut lock = Some(lock);
    let prepared = (|| -> Result<Option<PreparedDaemonUpgrade>> {
        let plan = build_upgrade_plan(lock.as_ref().unwrap(), &current, None, true)?;
        let semantic_repair_required = semantic_install_required(&plan, data_root)?;
        if !plan.update_available && !semantic_repair_required {
            write_state_checked_locked(
                data_root,
                lock.as_ref().unwrap(),
                attempt.as_ref().unwrap(),
                &plan,
                "up_to_date",
                current.upgrade.interval,
            )?;
            drop(lock.take());
            send_daemon_upgrade_terminal(
                data_root,
                &current,
                Some(&plan),
                attempt.as_ref().unwrap().id(),
                UpgradeStatus::UpToDate,
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
        let runtime_artifact = if plan.update_available && plan.semantic_provisioning.is_none() {
            match (
                plan.metadata.onnxruntime.as_ref(),
                plan.onnxruntime_artifact_url(),
            ) {
                (Some(runtime), Some(runtime_url)) => Some(
                    DownloadedArtifact::download_or_reuse_verified(
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
        if semantic_repair_required {
            let provisioning = plan
                .semantic_provisioning
                .as_ref()
                .ok_or_else(|| anyhow!("Semantic repair has no signed provisioning plan"))?;
            for asset in &provisioning.assets {
                let url = plan.semantic_artifact_url(&asset.metadata.artifact);
                semantic_artifacts.push(
                    DownloadedArtifact::download_or_reuse_verified(
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
            current.upgrade.interval,
        )?;
        Ok(Some(PreparedDaemonUpgrade(
            PreparedDaemonUpgradeKind::Apply {
                data_root: data_root.to_path_buf(),
                interval: current.upgrade.interval,
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
                    data_root,
                    &current,
                    None,
                    attempt.as_ref().unwrap().id(),
                    UpgradeStatus::Failed,
                    false,
                    Some(upgrade_failure_kind(&error)),
                    started.elapsed(),
                );
            }
            Err(error)
        }
    }
}

/// Completes a staged daemon-owned attempt after the daemon has stopped
/// accepting work and released its per-root lifecycle lock.
pub(crate) fn finish_daemon_auto_upgrade(
    prepared: PreparedDaemonUpgrade,
    restart: (&str, u64, u64),
    handoff: Option<crate::semantic::DaemonUpgradeHandoff>,
) -> Result<()> {
    let handoff =
        handoff.ok_or_else(|| anyhow!("automatic upgrade has no daemon lifecycle handoff"))?;
    let (data_root, interval, started, lock, attempt, plan, mut artifact, mut provisioning) =
        match prepared.0 {
            PreparedDaemonUpgradeKind::Apply {
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
            PreparedDaemonUpgradeKind::Recover {
                recovery,
                interval,
                started,
                lock,
            } => {
                let current = AppConfig::load(&recovery.data_root)?;
                if !current.daemon.enabled || !current.auto_upgrade_enabled() {
                    reconcile_replacement_terminal_locked(
                        &lock,
                        &recovery.attempt_id,
                        false,
                        Some("automatic interrupted-install recovery was disabled"),
                        interval,
                    )?;
                    drop(lock);
                    return handoff.resume_with(&current_install_path()?);
                }
                return match recover_interrupted_install(&recovery, lock.installation())? {
                    InstallRecovery::None => Err(anyhow!(
                        "interrupted ctx installation recovery disappeared while owned"
                    )),
                    InstallRecovery::Recovered { committed } => {
                        let detail = (!committed).then_some(CURRENT_FORMAT_ROLLBACK_DETAIL);
                        let automatic = reconcile_replacement_terminal_locked(
                            &lock,
                            &recovery.attempt_id,
                            committed,
                            detail,
                            interval,
                        )?;
                        drop(lock);
                        let resumed = handoff.resume_with(&current_install_path()?);
                        if automatic {
                            send_daemon_upgrade_terminal(
                                &recovery.data_root,
                                &current,
                                None,
                                &recovery.attempt_id,
                                if committed {
                                    UpgradeStatus::Applied
                                } else {
                                    UpgradeStatus::Failed
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
                        let automatic = reconcile_replacement_terminal_locked(
                            &lock,
                            &recovery.attempt_id,
                            false,
                            Some(CURRENT_FORMAT_ROLLBACK_DETAIL),
                            interval,
                        )?;
                        if automatic {
                            send_daemon_upgrade_terminal(
                                &recovery.data_root,
                                &current,
                                None,
                                &recovery.attempt_id,
                                UpgradeStatus::Failed,
                                false,
                                Some(UpgradeFailureKind::ApplyFailed),
                                started.elapsed(),
                            );
                        }
                        drop(lock);
                        super::continue_current_format_recovery_reexec(handoff, reexec)
                    }
                };
            }
        };
    let current = AppConfig::load(&data_root)?;
    if !current.daemon.enabled || !current.auto_upgrade_enabled() {
        write_state_checked_locked(&data_root, &lock, &attempt, &plan, "disabled", interval)?;
        drop(lock);
        let restart = handoff.resume_with(&plan.install_path);
        send_daemon_upgrade_terminal(
            &data_root,
            &current,
            Some(&plan),
            attempt.id(),
            UpgradeStatus::Skipped,
            false,
            None,
            started.elapsed(),
        );
        return restart;
    }
    write_state_phase_locked(&lock, &attempt, "quiescing")?;
    let mut record_applying = || {
        write_state_phase_locked(&lock, &attempt, "applying")?;
        Ok(())
    };
    let result = apply_artifact(
        lock.installation(),
        &plan,
        artifact.as_mut(),
        provisioning.runtime.as_mut(),
        &mut provisioning.semantic,
        &data_root,
        attempt.id(),
        Some(restart),
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
            write_state_checked_locked(&data_root, &lock, &attempt, &plan, "applied", interval)?;
            drop(lock);
            let restart = handoff.resume_with(&plan.install_path);
            send_daemon_upgrade_terminal(
                &data_root,
                &current,
                Some(&plan),
                attempt.id(),
                UpgradeStatus::Applied,
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
                    &data_root,
                    &current,
                    Some(&plan),
                    attempt.id(),
                    UpgradeStatus::Failed,
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

#[allow(clippy::too_many_arguments)]
fn send_daemon_upgrade_terminal(
    data_root: &Path,
    config: &AppConfig,
    plan: Option<&UpgradePlan>,
    attempt_id: &str,
    status: UpgradeStatus,
    applied: bool,
    failure_kind: Option<UpgradeFailureKind>,
    duration: Duration,
) {
    let event = PublicEventV1::OperationCompleted(OperationCompletedV1::for_automatic_upgrade(
        UpgradeTelemetry {
            mode: UpgradeMode::Auto,
            operation: UpgradeOperation::Apply,
            dry_run: false,
            suppress_event: false,
            status: Some(status),
            applied: Some(applied),
            scheduled: Some(false),
            update_available: Some(false),
            update_was_available: plan.map(|plan| plan.update_available),
            upgrade_attempt_id: Some(attempt_id.to_owned()),
            managed_install: plan.map(|plan| plan.managed),
            self_upgrade_allowed: plan.map(|plan| plan.metadata.self_upgrade_allowed),
            auto_upgrade_allowed: plan.map(|plan| plan.metadata.auto_upgrade_allowed),
            warning_count: plan.map(|plan| count_bucket(plan.warnings.len() as u64)),
            channel: plan.map(|plan| UpgradeChannel::from_config(&plan.channel)),
            failure_kind,
        },
        if failure_kind.is_some() {
            Outcome::Failure
        } else {
            Outcome::Success
        },
        duration,
    ));
    analytics::send_batch(data_root, config, &[event]);
}
